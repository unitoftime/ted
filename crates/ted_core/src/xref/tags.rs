//! The tree-sitter backend: definitions and references from each grammar's tags query,
//! with no language server and no index.
//!
//! The current buffer is read from its live tree, so unsaved edits count, and a local
//! binding (`let`, a parameter) in scope at point wins outright. The rest of the project
//! is searched on a job thread: the project's files of the buffer's language (from git,
//! so ignored files stay out) are read in parallel, and only those that mention the symbol
//! are parsed. Without scope or type information, a common name can yield several
//! candidates; a language server, when attached, answers first.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

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
        ed.spawn(move |ctx| {
            let files: Vec<PathBuf> = project
                .files()
                .into_iter()
                .map(|f| project.root.join(f))
                .filter(|f| *f != path && mode.matches(f))
                .collect();
            let mut items = local;
            items.extend(scan_files(&files, &grammar, kind, &symbol));
            ctx.send(move |ed| reply.send(ed, Ok(items)));
        });
        true
    }
}

/// Scans `files` on all cores; results come back sorted by file and position.
fn scan_files(files: &[PathBuf], grammar: &Arc<Grammar>, kind: Kind, symbol: &str) -> Vec<Item> {
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism().map_or(4, |n| n.get()).min(8).min(files.len());
    let mut items: Vec<Item> = thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut parser = grammar.parser();
                    let mut found = Vec::new();
                    while let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        scan_file(&mut parser, grammar, path, kind, symbol, &mut found);
                    }
                    found
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    items.sort_by(|a, b| {
        let (a, b) = (&a.location, &b.location);
        (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col))
    });
    items
}

/// Adds the hits in the file at `path`, which is only parsed if it mentions `symbol`.
fn scan_file(parser: &mut Parser, grammar: &Grammar, path: &Path, kind: Kind, symbol: &str, out: &mut Vec<Item>) {
    if fs::metadata(path).map_or(true, |m| m.len() > MAX_FILE_BYTES) {
        return;
    }
    let Ok(src) = fs::read_to_string(path) else {
        return;
    };
    if !src.contains(symbol) {
        return;
    }
    let Some(tree) = parser.parse(&src, None) else {
        return;
    };
    if let Found::Items(items) = Source::new(path, &src).scan(grammar, &tree, kind, symbol, None) {
        out.extend(items);
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
