//! Built-in major modes. Language modes are plain data; markdown, dired and compilation
//! also register their own commands and keymaps, exactly as a plugin mode would.

pub mod compilation;
pub mod dired;
pub mod markdown;

use crate::editor::Editor;
use crate::mode::{IndentStyle, Mode};
use crate::syntax;

pub(crate) fn register_builtin(ed: &mut Editor) {
    let modes = [
        Mode::new("Rust").comment("// ").tab_width(4).extensions(&["rs"]).grammar(syntax::rust),
        Mode::new("Go").comment("// ").indent(IndentStyle::Tabs).extensions(&["go"]).grammar(syntax::go),
        Mode::new("C/C++").comment("// ").extensions(&["c", "h", "cpp", "hpp", "cc", "cxx"]).grammar(syntax::c),
        Mode::new("JavaScript").extensions(&["js", "jsx", "mjs", "cjs"]).grammar(syntax::javascript),
        Mode::new("TypeScript").extensions(&["ts", "mts", "cts"]).grammar(syntax::typescript),
        Mode::new("TSX").extensions(&["tsx"]).grammar(syntax::tsx),
        Mode::new("Python").comment("# ").tab_width(4).extensions(&["py"]).grammar(syntax::python),
        Mode::new("JSON").extensions(&["json", "jsonc"]).grammar(syntax::json),
        Mode::new("TOML").comment("# ").extensions(&["toml"]).grammar(syntax::toml),
        Mode::new("YAML").comment("# ").extensions(&["yaml", "yml"]).grammar(syntax::yaml),
        Mode::new("HTML").extensions(&["html", "htm"]).grammar(syntax::html),
        Mode::new("CSS").extensions(&["css"]).grammar(syntax::css),
        Mode::new("Shell").comment("# ").extensions(&["sh", "bash", "zsh"]).grammar(syntax::bash),
        Mode::new("Lisp").comment(";; ").extensions(&["el", "lisp"]),
        Mode::new("Makefile")
            .comment("# ")
            .tab_width(8)
            .indent(IndentStyle::Tabs)
            .extensions(&["mk"])
            .file_names(&["makefile", "gnumakefile"])
            .grammar(syntax::make),
    ];
    for mode in modes {
        ed.define_mode(mode);
    }
    markdown::register(ed);
    dired::register(ed);
    compilation::register(ed);
}
