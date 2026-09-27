//! Tree-sitter syntax highlighting and symbol tags.
//!
//! A `Grammar` (language + compiled highlight and tags queries) is built once per process
//! and shared, across threads too. Each buffer owns a `Syntax`: its parser and
//! incrementally edited tree.

use std::ops::Range;
use std::sync::{Arc, OnceLock};

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Query, QueryCursor, TextProvider, Tree};

use crate::face::FaceId;
use crate::indent::Indents;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxKind {
    Plain,
    Keyword,
    Type,
    Function,
    StringLiteral,
    Comment,
    Number,
    Operator,
    Punctuation,
    Preprocessor,
    Constant,
    Heading,
    Link,
}

impl SyntaxKind {
    pub fn face(self) -> FaceId {
        match self {
            SyntaxKind::Plain => FaceId::DEFAULT,
            SyntaxKind::Keyword => FaceId::KEYWORD,
            SyntaxKind::Type => FaceId::TYPE,
            SyntaxKind::Function => FaceId::FUNCTION,
            SyntaxKind::StringLiteral => FaceId::STRING,
            SyntaxKind::Comment => FaceId::COMMENT,
            SyntaxKind::Number => FaceId::NUMBER,
            SyntaxKind::Operator => FaceId::OPERATOR,
            SyntaxKind::Punctuation => FaceId::PUNCTUATION,
            SyntaxKind::Preprocessor => FaceId::PREPROCESSOR,
            SyntaxKind::Constant => FaceId::CONSTANT,
            SyntaxKind::Heading => FaceId::HEADING,
            SyntaxKind::Link => FaceId::LINK,
        }
    }
}

/// A highlighted span within one line, in char columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxToken {
    pub start_col: usize,
    pub end_col: usize,
    pub kind: SyntaxKind,
}

pub fn capture_to_kind(name: &str) -> SyntaxKind {
    const TABLE: &[(&str, SyntaxKind)] = &[
        ("keyword", SyntaxKind::Keyword),
        ("conditional", SyntaxKind::Keyword),
        ("repeat", SyntaxKind::Keyword),
        ("exception", SyntaxKind::Keyword),
        ("tag", SyntaxKind::Keyword),
        ("type", SyntaxKind::Type),
        ("function", SyntaxKind::Function),
        ("method", SyntaxKind::Function),
        ("string", SyntaxKind::StringLiteral),
        ("character", SyntaxKind::StringLiteral),
        ("comment", SyntaxKind::Comment),
        ("number", SyntaxKind::Number),
        ("float", SyntaxKind::Number),
        ("constant.numeric", SyntaxKind::Number),
        ("operator", SyntaxKind::Operator),
        ("punctuation", SyntaxKind::Punctuation),
        ("attribute", SyntaxKind::Preprocessor),
        ("preproc", SyntaxKind::Preprocessor),
        ("include", SyntaxKind::Preprocessor),
        ("text.title", SyntaxKind::Heading),
        ("heading", SyntaxKind::Heading),
        ("markup.heading", SyntaxKind::Heading),
        ("text.literal", SyntaxKind::StringLiteral),
        ("markup.raw", SyntaxKind::StringLiteral),
        ("text.uri", SyntaxKind::Link),
        ("text.reference", SyntaxKind::Link),
        ("markup.link", SyntaxKind::Link),
        ("constant", SyntaxKind::Constant),
        ("boolean", SyntaxKind::Constant),
        ("variable.parameter", SyntaxKind::Constant),
    ];
    TABLE.iter().find(|(prefix, _)| name.starts_with(prefix)).map_or(SyntaxKind::Plain, |(_, kind)| *kind)
}

pub struct Grammar {
    pub language: Language,
    pub query: Query,
    /// Capture index -> highlight kind, resolved once at load.
    kinds: Vec<SyntaxKind>,
    /// Definitions and references, for navigating without a language server.
    pub tags: Option<Tags>,
    /// Structural indentation, for languages whose layout their syntax determines.
    pub indents: Option<Indents>,
}

impl Grammar {
    pub fn new(language: Language, query_source: &str) -> Option<Self> {
        let query = Query::new(&language, query_source).ok()?;
        let kinds = query.capture_names().iter().map(|n| capture_to_kind(n)).collect();
        Some(Self { language, query, kinds, tags: None, indents: None })
    }

    /// Adds a tags query. One that fails to compile only disables navigation.
    pub fn with_tags(mut self, source: &str) -> Self {
        self.tags = Tags::new(&self.language, source);
        self
    }

    /// Enables reindenting: brackets indent everywhere; `indent` names the other node kinds
    /// whose later lines go one level deeper, `close` those that line up with their parent.
    pub fn with_indents(mut self, indent: &[&str], close: &[&str]) -> Self {
        self.indents = Some(Indents::new(&self.language, indent, close));
        self
    }

    /// A parser for this grammar, for parsing text outside any buffer.
    pub fn parser(&self) -> Parser {
        let mut parser = Parser::new();
        // Grammars are built from the same tree-sitter version we link, so this can't fail.
        let _ = parser.set_language(&self.language);
        parser
    }
}

/// What a tag says about the symbol it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagRole {
    /// A definition visible outside its file (function, type, constant, ...).
    Definition,
    /// A binding visible only in its enclosing scope (`let`, parameters).
    LocalDefinition,
    Reference,
}

/// A symbol occurrence: `name` is the identifier, `node` the whole tagged construct.
#[derive(Debug, Clone, Copy)]
pub struct Tag<'tree> {
    pub role: TagRole,
    pub name: Node<'tree>,
    pub node: Node<'tree>,
}

/// A compiled tags query, following the `tags.scm` conventions of tree-sitter grammars:
/// `@name` inside a `@definition.*` or `@reference.*` capture. ted adds
/// `@local.definition` for scoped bindings.
pub struct Tags {
    query: Query,
    name: u32,
    /// Capture index -> role, for the captures that carry one.
    roles: Vec<Option<TagRole>>,
}

impl Tags {
    fn new(language: &Language, source: &str) -> Option<Self> {
        let query = Query::new(language, source).ok()?;
        let name = query.capture_index_for_name("name")?;
        let roles = query
            .capture_names()
            .iter()
            .map(|capture| match *capture {
                "local.definition" => Some(TagRole::LocalDefinition),
                c if c.starts_with("definition.") => Some(TagRole::Definition),
                c if c.starts_with("reference.") => Some(TagRole::Reference),
                _ => None,
            })
            .collect();
        Some(Self { query, name, roles })
    }

    /// Every tag in `tree`, in document order.
    pub fn collect<'t, I: AsRef<[u8]>>(&self, tree: &'t Tree, text: impl TextProvider<I>) -> Vec<Tag<'t>> {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), text);
        let mut tags = Vec::new();
        while let Some(m) = matches.next() {
            let name = m.captures.iter().find(|c| c.index == self.name);
            let role = m.captures.iter().find_map(|c| Some((self.roles[c.index as usize]?, c.node)));
            if let (Some(name), Some((role, node))) = (name, role) {
                tags.push(Tag { role, name: name.node, node });
            }
        }
        tags
    }
}

/// Whether `byte` lies inside a comment or string literal.
pub fn in_comment_or_string(tree: &Tree, byte: usize) -> bool {
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte);
    while let Some(n) = node {
        let kind = n.kind();
        if kind.contains("comment") || kind.contains("string") {
            return true;
        }
        node = n.parent();
    }
    false
}

/// Serves a rope's text to tree-sitter queries without copying it.
pub struct RopeText<'a>(pub &'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeText<'a> {
    type I = std::iter::Map<ropey::iter::Chunks<'a>, fn(&'a str) -> &'a [u8]>;

    fn text(&mut self, node: Node) -> Self::I {
        self.0.byte_slice(node.byte_range()).chunks().map(str::as_bytes)
    }
}

/// A buffer's current tree, with the grammar and text it was parsed from.
pub struct Parsed<'a> {
    pub grammar: &'a Grammar,
    pub tree: &'a Tree,
    pub text: &'a Rope,
}

type GrammarCell = OnceLock<Option<Arc<Grammar>>>;

fn cached(cell: &'static GrammarCell, build: impl FnOnce() -> Option<Grammar>) -> Option<Arc<Grammar>> {
    cell.get_or_init(|| build().map(Arc::new)).clone()
}

// Tags the grammars' own `tags.scm` leave out: constants, fields and scoped bindings.

const RUST_TAGS: &str = r#"
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant
(enum_variant name: (identifier) @name) @definition.constant
(field_declaration name: (field_identifier) @name) @definition.field
(let_declaration pattern: (identifier) @name) @local.definition
(parameter pattern: (identifier) @name) @local.definition
(closure_parameters (identifier) @name) @local.definition
"#;

const GO_TAGS: &str = r#"
(var_spec name: (identifier) @name) @definition.variable
(const_spec name: (identifier) @name) @definition.constant
(field_declaration name: (field_identifier) @name) @definition.field
(short_var_declaration left: (expression_list (identifier) @name)) @local.definition
(parameter_declaration name: (identifier) @name) @local.definition
"#;

const C_TAGS: &str = r#"
(preproc_def name: (identifier) @name) @definition.macro
(preproc_function_def name: (identifier) @name) @definition.macro
(enumerator name: (identifier) @name) @definition.constant
(field_declaration declarator: (field_identifier) @name) @definition.field
(init_declarator declarator: (identifier) @name) @local.definition
(parameter_declaration declarator: (identifier) @name) @local.definition
"#;

const PYTHON_TAGS: &str = r#"
(assignment left: (identifier) @name) @local.definition
(parameters (identifier) @name) @local.definition
"#;

// Indentation beyond brackets: node kinds whose later lines are one level deeper, and kinds
// that line up with the node they close.

const RUST_INDENTS: &[&str] =
    &["let_declaration", "match_arm", "call_expression", "field_expression", "binary_expression", "where_clause"];
const GO_INDENTS: &[&str] =
    &["expression_case", "type_case", "default_case", "communication_case", "const_declaration", "var_declaration"];
const C_INDENTS: &[&str] = &["case_statement"];
const PYTHON_INDENTS: &[&str] = &[
    "function_definition",
    "class_definition",
    "if_statement",
    "for_statement",
    "while_statement",
    "with_statement",
    "try_statement",
    "match_statement",
    "case_clause",
];
const PYTHON_CLOSES: &[&str] =
    &["elif_clause", "else_clause", "except_clause", "except_group_clause", "finally_clause"];
const JS_INDENTS: &[&str] = &[
    "switch_case",
    "switch_default",
    "call_expression",
    "member_expression",
    "variable_declarator",
    "jsx_element",
    "jsx_opening_element",
    "jsx_self_closing_element",
];
const JS_CLOSES: &[&str] = &["jsx_closing_element"];
const BASH_INDENTS: &[&str] = &["if_statement", "do_group", "case_statement", "case_item"];
const BASH_CLOSES: &[&str] = &["elif_clause", "else_clause", "then", "fi", "done", "esac"];

fn with_tags(grammar: Option<Grammar>, base: &str, extra: &str) -> Option<Grammar> {
    grammar.map(|g| g.with_tags(&format!("{}\n{}", base, extra)))
}

/// Defines a lazily built, process-wide grammar loader.
macro_rules! grammar {
    ($name:ident, $build:expr) => {
        pub fn $name() -> Option<Arc<Grammar>> {
            static CELL: GrammarCell = OnceLock::new();
            cached(&CELL, || $build)
        }
    };
}

grammar!(rust, {
    let grammar = Grammar::new(tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY);
    with_tags(grammar, tree_sitter_rust::TAGS_QUERY, RUST_TAGS).map(|g| g.with_indents(RUST_INDENTS, &[]))
});

grammar!(go, {
    let grammar = Grammar::new(tree_sitter_go::LANGUAGE.into(), tree_sitter_go::HIGHLIGHTS_QUERY);
    with_tags(grammar, tree_sitter_go::TAGS_QUERY, GO_TAGS).map(|g| g.with_indents(GO_INDENTS, &[]))
});

grammar!(c, {
    let grammar = Grammar::new(tree_sitter_c::LANGUAGE.into(), tree_sitter_c::HIGHLIGHT_QUERY);
    with_tags(grammar, tree_sitter_c::TAGS_QUERY, C_TAGS).map(|g| g.with_indents(C_INDENTS, &[]))
});

grammar!(python, {
    let grammar = Grammar::new(tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY);
    with_tags(grammar, tree_sitter_python::TAGS_QUERY, PYTHON_TAGS)
        .map(|g| g.with_indents(PYTHON_INDENTS, PYTHON_CLOSES))
});

// JavaScript parses JSX natively. TypeScript's queries only add to JavaScript's, which they
// take precedence over; TSX adds the JSX ones too.

grammar!(javascript, {
    let query = [tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_javascript::JSX_HIGHLIGHT_QUERY].join("\n");
    Grammar::new(tree_sitter_javascript::LANGUAGE.into(), &query)
        .map(|g| g.with_tags(tree_sitter_javascript::TAGS_QUERY).with_indents(JS_INDENTS, JS_CLOSES))
});

grammar!(typescript, typescript_grammar(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), ""));

grammar!(
    tsx,
    typescript_grammar(tree_sitter_typescript::LANGUAGE_TSX.into(), tree_sitter_javascript::JSX_HIGHLIGHT_QUERY)
);

fn typescript_grammar(language: Language, extra_highlights: &str) -> Option<Grammar> {
    let query = [tree_sitter_typescript::HIGHLIGHTS_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY, extra_highlights]
        .join("\n");
    let grammar = Grammar::new(language, &query);
    with_tags(grammar, tree_sitter_javascript::TAGS_QUERY, tree_sitter_typescript::TAGS_QUERY)
        .map(|g| g.with_indents(JS_INDENTS, JS_CLOSES))
}

grammar!(
    bash,
    Grammar::new(tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY)
        .map(|g| g.with_indents(BASH_INDENTS, BASH_CLOSES))
);
grammar!(make, Grammar::new(tree_sitter_make::LANGUAGE.into(), tree_sitter_make::HIGHLIGHTS_QUERY));
grammar!(
    json,
    Grammar::new(tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY)
        .map(|g| g.with_indents(&[], &[]))
);
grammar!(
    toml,
    Grammar::new(tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY)
        .map(|g| g.with_indents(&[], &[]))
);
grammar!(yaml, Grammar::new(tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY));
grammar!(html, Grammar::new(tree_sitter_html::LANGUAGE.into(), tree_sitter_html::HIGHLIGHTS_QUERY));
grammar!(
    css,
    Grammar::new(tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY).map(|g| g.with_indents(&[], &[]))
);

grammar!(markdown, {
    let query = format!(
        "{}\n(atx_heading) @text.title\n(setext_heading) @text.title\n(task_list_marker_checked) @constant\n(task_list_marker_unchecked) @punctuation.special\n(info_string) @keyword\n",
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK
    );
    Grammar::new(tree_sitter_md::LANGUAGE.into(), &query)
});

/// Per-buffer parse state.
pub struct Syntax {
    grammar: Arc<Grammar>,
    parser: Parser,
    tree: Option<Tree>,
}

impl Syntax {
    pub fn new(grammar: Arc<Grammar>) -> Self {
        Self { parser: grammar.parser(), grammar, tree: None }
    }

    /// Drops the tree; the next highlight reparses from scratch.
    pub fn invalidate(&mut self) {
        self.tree = None;
    }

    /// Records an insertion (before it is applied to `rope`) for incremental reparsing.
    pub fn edit_insert(&mut self, rope: &Rope, at: usize, text: &str) {
        let Some(tree) = &mut self.tree else { return };
        let start_byte = rope.char_to_byte(at);
        let start = byte_point(rope, start_byte);
        let newlines = text.matches('\n').count();
        let new_end = match text.rfind('\n') {
            Some(nl) => Point { row: start.row + newlines, column: text.len() - nl - 1 },
            None => Point { row: start.row, column: start.column + text.len() },
        };
        tree.edit(&InputEdit {
            start_byte,
            old_end_byte: start_byte,
            new_end_byte: start_byte + text.len(),
            start_position: start,
            old_end_position: start,
            new_end_position: new_end,
        });
    }

    /// Records a removal (before it is applied to `rope`) for incremental reparsing.
    pub fn edit_remove(&mut self, rope: &Rope, range: Range<usize>) {
        let Some(tree) = &mut self.tree else { return };
        let start_byte = rope.char_to_byte(range.start);
        let old_end_byte = rope.char_to_byte(range.end);
        let start = byte_point(rope, start_byte);
        tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte,
            start_position: start,
            old_end_position: byte_point(rope, old_end_byte),
            new_end_position: start,
        });
    }

    pub fn highlight(&mut self, rope: &Rope, lines: Range<usize>) -> Vec<Vec<SyntaxToken>> {
        self.tree = parse_rope(&mut self.parser, rope, self.tree.as_ref());
        match &self.tree {
            Some(tree) => highlight_lines(tree, &self.grammar, rope, lines),
            None => Vec::new(),
        }
    }

    /// The tree as of the last parse (highlighting parses, so during rendering it is current).
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Brings the tree up to date with `rope` and returns it.
    pub fn parsed<'a>(&'a mut self, rope: &'a Rope) -> Option<Parsed<'a>> {
        self.tree = parse_rope(&mut self.parser, rope, self.tree.as_ref());
        Some(Parsed { grammar: &self.grammar, tree: self.tree.as_ref()?, text: rope })
    }
}

fn byte_point(rope: &Rope, byte: usize) -> Point {
    let row = rope.byte_to_line(byte);
    Point { row, column: byte - rope.line_to_byte(row) }
}

pub fn parse_rope(parser: &mut Parser, rope: &Rope, old_tree: Option<&Tree>) -> Option<Tree> {
    parser.parse_with_options(
        &mut |byte_offset, _| {
            if byte_offset >= rope.len_bytes() {
                return &[] as &[u8];
            }
            let (chunk, chunk_byte_idx, _, _) = rope.chunk_at_byte(byte_offset);
            &chunk.as_bytes()[byte_offset - chunk_byte_idx..]
        },
        old_tree,
        None,
    )
}

/// Highlight tokens for `lines`, indexed by `line - lines.start`.
pub fn highlight_lines(tree: &Tree, grammar: &Grammar, rope: &Rope, lines: Range<usize>) -> Vec<Vec<SyntaxToken>> {
    let end_line = lines.end.min(rope.len_lines());
    let mut result: Vec<Vec<SyntaxToken>> = vec![Vec::new(); end_line.saturating_sub(lines.start)];
    if result.is_empty() {
        return result;
    }

    let mut cursor = QueryCursor::new();
    cursor.set_point_range(Point { row: lines.start, column: 0 }..Point { row: end_line, column: 0 });
    let mut captures = cursor.captures(&grammar.query, tree.root_node(), RopeText(rope));

    while let Some((mat, capture_idx)) = captures.next() {
        let cap = mat.captures[*capture_idx];
        let kind = grammar.kinds[cap.index as usize];
        if kind == SyntaxKind::Plain {
            continue;
        }
        let (start, end) = (cap.node.start_position(), cap.node.end_position());
        for row in start.row.max(lines.start)..=end.row.min(end_line - 1) {
            let line = rope.line(row);
            let line_bytes = line.len_bytes();
            let byte_start = if row == start.row { start.column.min(line_bytes) } else { 0 };
            let byte_end = if row == end.row { end.column.min(line_bytes) } else { line_bytes };
            let (start_col, end_col) = (line.byte_to_char(byte_start), line.byte_to_char(byte_end));
            if start_col < end_col {
                result[row - lines.start].push(SyntaxToken { start_col, end_col, kind });
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::GrammarLoader;

    /// A highlight query that fails to compile leaves its buffers unhighlighted, silently.
    #[test]
    fn every_grammar_loads() {
        let loaders: [(&str, GrammarLoader); 15] = [
            ("rust", rust),
            ("go", go),
            ("c", c),
            ("python", python),
            ("javascript", javascript),
            ("typescript", typescript),
            ("tsx", tsx),
            ("bash", bash),
            ("make", make),
            ("json", json),
            ("toml", toml),
            ("yaml", yaml),
            ("html", html),
            ("css", css),
            ("markdown", markdown),
        ];
        for (name, loader) in loaders {
            assert!(loader().is_some(), "{} grammar failed to load", name);
        }
    }

    /// The extra tag patterns are hand-written against each grammar's node names; a typo
    /// would silently disable navigation for that language.
    #[test]
    fn every_grammar_compiles_its_tags() {
        for (name, grammar) in [
            ("rust", rust()),
            ("go", go()),
            ("c", c()),
            ("python", python()),
            ("javascript", javascript()),
            ("typescript", typescript()),
            ("tsx", tsx()),
        ] {
            let grammar = grammar.unwrap_or_else(|| panic!("{} grammar failed to load", name));
            assert!(grammar.tags.is_some(), "{} tags query failed to compile", name);
        }
    }

    #[test]
    fn tags_find_definitions_and_local_bindings() {
        let grammar = rust().unwrap();
        let src = "const LIMIT: u32 = 3;\nfn run(n: u32) { let total = n; helper(total); }\n";
        let tree = grammar.parser().parse(src, None).unwrap();
        let tags = grammar.tags.as_ref().unwrap().collect(&tree, src.as_bytes());
        let found: Vec<(&str, TagRole)> = tags.iter().map(|t| (&src[t.name.byte_range()], t.role)).collect();
        for expected in [
            ("LIMIT", TagRole::Definition),
            ("run", TagRole::Definition),
            ("n", TagRole::LocalDefinition),
            ("total", TagRole::LocalDefinition),
            ("helper", TagRole::Reference),
        ] {
            assert!(found.contains(&expected), "missing {:?} in {:?}", expected, found);
        }
    }
}
