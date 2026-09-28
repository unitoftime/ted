//! The tree-sitter backend: definitions and references from each grammar's tags query,
//! with no language server.
//!
//! The current buffer is read from its live tree, so unsaved edits count, and a local
//! binding (`let`, a parameter) in scope at point wins outright. The rest of the project
//! is searched on a job thread, over the project's files of the buffer's language (from
//! git, so ignored files stay out). An index remembers what each file held when last
//! read, keyed by its modification time and size: the identifiers it contains, so files
//! that can't match are skipped unread, and its definitions once it has been parsed. Only
//! files changed since are read again. Without scope or type information, a common name
//! can yield several candidates; a language server, when attached, answers first.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::SystemTime;

use parking_lot::Mutex;
use tree_sitter::{Node, Parser, Tree};

use super::{Item, Kind, Query};
use crate::chain::{Backend, Reply};
use crate::editor::Editor;
use crate::locations::Location;
use crate::project::Project;
use crate::syntax::{in_comment_or_string, Grammar, Tag, TagRole};
use crate::text::is_ident_char;

/// Files larger than this are generated or data; skip them.
const MAX_FILE_BYTES: u64 = 8 << 20;

pub struct TagsBackend;

impl Backend<Query> for TagsBackend {
    fn name(&self) -> &str {
        "tree-sitter"
    }

    fn start(&self, ed: &mut Editor, query: &Query, reply: Reply<Query>) -> bool {
        let buf = &mut ed.buffers[query.buffer];
        let grammar = buf.mode().grammar.and_then(|load| load()).filter(|g| g.tags.is_some());
        let (Some(grammar), Some(path)) = (grammar, buf.path().map(Path::to_path_buf)) else {
            return false;
        };
        let mode = buf.mode().clone();
        let Some(parsed) = buf.parsed() else {
            return false;
        };
        let (kind, symbol) = (query.kind, query.symbol.clone());
        let src = String::from(parsed.text);
        let cursor = parsed.text.char_to_byte(query.pos);
        let found = Source::new(&path, &src).scan(&grammar, parsed.tree, kind, &symbol, Some(cursor));
        let local = match found {
            Found::Binding(item) => {
                reply.send(ed, Ok(vec![item]));
                return true;
            }
            Found::Items(items) => items,
        };

        let project = Project::of(Some(&path));
        let (lists, index) = (ed.file_lists(), ed.ext_mut::<TagIndex>().clone());
        ed.spawn(move |ctx| {
            let cached = lists.cached(&project);
            let listed = cached.clone().unwrap_or_else(|| lists.list(&project));
            let files: Vec<PathBuf> =
                listed.iter().map(|f| project.root.join(f)).filter(|f| *f != path && mode.matches(f)).collect();
            let mut items = local;
            items.extend(index.search(&project, &listed, &files, &grammar, kind, &symbol));
            ctx.send(move |ed| reply.send(ed, Ok(items)));
            // Picks up files added since, for next time.
            if cached.is_some() {
                lists.list(&project);
            }
        });
        true
    }
}

/// What a file held when last read.
struct FileEntry {
    /// Its modification time and size then.
    stamp: (SystemTime, u64),
    /// Hashes of the identifiers in it, sorted.
    words: Box<[u32]>,
    /// Its definitions as (name hash, start byte), sorted; filled in once it is parsed.
    definitions: OnceLock<Box<[(u32, u32)]>>,
}

impl FileEntry {
    fn new(stamp: (SystemTime, u64), src: &str) -> Self {
        let mut words: Vec<u32> = src.split(|c: char| !is_ident_char(c)).filter(|w| !w.is_empty()).map(hash).collect();
        words.sort_unstable();
        words.dedup();
        Self { stamp, words: words.into(), definitions: OnceLock::new() }
    }
}

/// What each file of a project held, by absolute path.
type ProjectIndex = HashMap<PathBuf, Arc<FileEntry>>;

/// The index of every project searched, by root.
#[derive(Clone, Default)]
struct TagIndex(Arc<Mutex<HashMap<PathBuf, ProjectIndex>>>);

impl TagIndex {
    /// Hits in `files` (absolute), updating the index of `project`, whose files are `listed`.
    fn search(
        &self,
        project: &Project,
        listed: &[PathBuf],
        files: &[PathBuf],
        grammar: &Arc<Grammar>,
        kind: Kind,
        symbol: &str,
    ) -> Vec<Item> {
        let mut projects = self.0.lock();
        let known = projects.entry(project.root.clone()).or_default();
        let (items, read) = scan_files(files, known, grammar, kind, symbol);
        known.extend(read);
        let listed: HashSet<&Path> = listed.iter().map(PathBuf::as_path).collect();
        known.retain(|path, _| path.strip_prefix(&project.root).is_ok_and(|rel| listed.contains(rel)));
        items
    }
}

/// The key identifiers are indexed by.
fn hash(word: &str) -> u32 {
    word.bytes().fold(0x811c_9dc5, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// What a scan looks for, and how files that may contain it are recognized.
struct Target<'a> {
    kind: Kind,
    symbol: &'a str,
    hash: u32,
    /// Whether the index of identifiers can rule files out (the symbol is one identifier).
    by_word: bool,
}

/// Scans `files` on all cores, reusing what `known` says about unchanged ones. Returns the
/// hits, sorted by file and position, and the entries of the files read again.
fn scan_files(
    files: &[PathBuf],
    known: &ProjectIndex,
    grammar: &Arc<Grammar>,
    kind: Kind,
    symbol: &str,
) -> (Vec<Item>, Vec<(PathBuf, Arc<FileEntry>)>) {
    let target = Target { kind, symbol, hash: hash(symbol), by_word: symbol.chars().all(is_ident_char) };
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism().map_or(4, |n| n.get()).min(8).min(files.len());
    let (mut items, mut read) = (Vec::new(), Vec::new());
    thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut parser = grammar.parser();
                    let (mut found, mut read) = (Vec::new(), Vec::new());
                    while let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let file = FileScan { parser: &mut parser, grammar, path, target: &target };
                        file.run(known.get(path), &mut found, &mut read);
                    }
                    (found, read)
                })
            })
            .collect();
        for (found, entries) in handles.into_iter().filter_map(|h| h.join().ok()) {
            items.extend(found);
            read.extend(entries);
        }
    });
    items.sort_by(|a, b| {
        let (a, b) = (&a.location, &b.location);
        (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col))
    });
    (items, read)
}

/// One file of a scan.
struct FileScan<'a> {
    parser: &'a mut Parser,
    grammar: &'a Grammar,
    path: &'a Path,
    target: &'a Target<'a>,
}

impl FileScan<'_> {
    /// Adds the file's hits to `out`. The file is read only if it changed since `known`,
    /// and parsed only if it may hold a hit that isn't indexed yet.
    fn run(self, known: Option<&Arc<FileEntry>>, out: &mut Vec<Item>, read: &mut Vec<(PathBuf, Arc<FileEntry>)>) {
        let Ok(meta) = fs::metadata(self.path) else { return };
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
        let mut src = None;
        let entry = match known.filter(|e| e.stamp == stamp) {
            Some(entry) => entry.clone(),
            None => {
                // Too large means generated or data: indexed as empty.
                src = (meta.len() <= MAX_FILE_BYTES).then(|| fs::read_to_string(self.path).ok()).flatten();
                let entry = Arc::new(FileEntry::new(stamp, src.as_deref().unwrap_or_default()));
                read.push((self.path.to_path_buf(), entry.clone()));
                entry
            }
        };
        let target = self.target;
        if target.by_word && entry.words.binary_search(&target.hash).is_err() {
            return;
        }
        let Some(src) = src.or_else(|| fs::read_to_string(self.path).ok()) else { return };
        let source = Source::new(self.path, &src);
        match target.kind {
            Kind::Definition => {
                let definitions = entry.definitions.get_or_init(|| self.definitions(&src));
                let first = definitions.partition_point(|&(h, _)| h < target.hash);
                let hits = definitions[first..].iter().take_while(|&&(h, _)| h == target.hash);
                let hits = hits.map(|&(_, byte)| byte as usize).filter(|&b| src[b..].starts_with(target.symbol));
                out.extend(hits.map(|byte| source.item_at(byte)));
            }
            Kind::References => {
                if let Some(tree) = self.parser.parse(&src, None) {
                    out.extend(references(&tree, &src, target.symbol).map(|byte| source.item_at(byte)));
                }
            }
        }
    }

    /// Every definition in `src`, as (name hash, start byte), sorted.
    fn definitions(self, src: &str) -> Box<[(u32, u32)]> {
        let (Some(tags), Some(tree)) = (&self.grammar.tags, self.parser.parse(src, None)) else {
            return Box::default();
        };
        let tags = tags.collect(&tree, src.as_bytes());
        let mut definitions: Vec<(u32, u32)> = tags
            .iter()
            .filter(|t| t.role == TagRole::Definition)
            .map(|t| (hash(&src[t.name.byte_range()]), t.name.start_byte() as u32))
            .collect();
        definitions.sort_unstable();
        definitions.into()
    }
}

enum Found {
    /// A local binding in scope at the cursor: the answer, no need to look further.
    Binding(Item),
    Items(Vec<Item>),
}

/// One file's text, indexed by line so hits convert to locations cheaply.
struct Source<'a> {
    path: &'a Path,
    src: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> Source<'a> {
    fn new(path: &'a Path, src: &'a str) -> Self {
        let line_starts = std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
        Self { path, src, line_starts }
    }

    /// Finds `symbol` in this file, parsed as `tree`. With a `cursor`, a local binding in
    /// scope there is preferred over everything else.
    fn scan(&self, grammar: &Grammar, tree: &Tree, kind: Kind, symbol: &str, cursor: Option<usize>) -> Found {
        let src = self.src;
        let bytes: Vec<usize> = match kind {
            Kind::Definition => {
                let tags = grammar.tags.as_ref().map_or_else(Vec::new, |tags| tags.collect(tree, src.as_bytes()));
                if let Some(binding) = cursor.and_then(|at| local_binding(&tags, src, at, symbol)) {
                    return Found::Binding(self.item_at(binding.start_byte()));
                }
                definitions(&tags, src, symbol).collect()
            }
            Kind::References => references(tree, src, symbol).collect(),
        };
        Found::Items(bytes.into_iter().map(|byte| self.item_at(byte)).collect())
    }

    /// The location of `byte`, with its line's text.
    fn item_at(&self, byte: usize) -> Item {
        let line = self.line_starts.partition_point(|&start| start <= byte) - 1;
        let start = self.line_starts[line];
        let end = self.line_starts.get(line + 1).map_or(self.src.len(), |&next| next - 1);
        let col = self.src[start..byte].chars().count();
        let text = self.src[start..end].trim_end_matches('\r').to_string();
        Item { location: Location { path: self.path.to_path_buf(), line, col }, text }
    }
}

/// Start bytes of the definitions of `symbol` visible outside their file.
fn definitions<'a>(tags: &'a [Tag], src: &'a str, symbol: &'a str) -> impl Iterator<Item = usize> + 'a {
    tags.iter()
        .filter(move |t| t.role == TagRole::Definition && &src[t.name.byte_range()] == symbol)
        .map(|t| t.name.start_byte())
}

/// Start bytes of the whole-word occurrences of `symbol` outside comments and strings.
fn references<'a>(tree: &'a Tree, src: &'a str, symbol: &'a str) -> impl Iterator<Item = usize> + 'a {
    src.match_indices(symbol)
        .map(|(byte, _)| byte)
        .filter(move |&byte| {
            let before = src[..byte].chars().next_back();
            let after = src[byte + symbol.len()..].chars().next();
            !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
        })
        .filter(move |&byte| !in_comment_or_string(tree, byte))
}

/// The innermost local binding of `symbol` in scope at `cursor`: declared before it, in a
/// scope that contains it. Among equally deep scopes the latest binding (shadowing) wins.
fn local_binding<'t>(tags: &[Tag<'t>], src: &str, cursor: usize, symbol: &str) -> Option<Node<'t>> {
    tags.iter()
        .filter(|t| t.role == TagRole::LocalDefinition && &src[t.name.byte_range()] == symbol)
        .filter(|t| t.name.start_byte() <= cursor)
        .filter_map(|t| {
            let scope = scope_of(t.node).map_or(0..usize::MAX, |s| s.byte_range());
            scope.contains(&cursor).then_some((scope.start, t.name))
        })
        .max_by_key(|(scope_start, name)| (*scope_start, name.start_byte()))
        .map(|(_, name)| name)
}

/// The block or function a binding is visible in; `None` means the whole file. Grammars
/// name these nodes differently, so this goes by the common vocabulary of node kinds.
fn scope_of(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(n) = current {
        let kind = n.kind();
        let block = kind.contains("block") || kind == "compound_statement" || kind.ends_with("body");
        let function =
            ["func", "closure", "lambda", "method"].iter().any(|f| kind.contains(f)) && !kind.contains("declarator");
        if block || function {
            return Some(n);
        }
        current = n.parent();
    }
    None
}
