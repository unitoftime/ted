//! Markdown: heading/task highlighting, dimmed completed tasks, list editing on S-RET
//! (continue the list) and TAB (indent the list item), and checking off tasks
//! (`markdown-toggle-checkbox`).

use crate::commands::edit;
use crate::editor::Editor;
use crate::face::FaceId;
use crate::mode::Mode;
use crate::syntax;

const BULLETS: [&str; 3] = ["- ", "* ", "+ "];

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("markdown-insert-list-item", "Newline continuing the current list item or checkbox", |ed, _| {
        edit(ed, |d| {
            let line = d.buf.char_to_line(d.pos());
            let content = d.buf.line_content(line).to_string();
            let indent: String = content.chars().take_while(|c| c.is_whitespace()).collect();
            match continuation(content.trim_start()) {
                Continuation::EndList => {
                    let start = d.buf.line_to_char(line);
                    let end = start + content.chars().count();
                    d.replace_range(start..end, "\n");
                }
                Continuation::Item(marker) => {
                    d.insert_text(&format!("\n{}{}", indent, marker));
                }
                Continuation::Plain => d.newline(),
            }
        });
    });
    c.register("markdown-toggle-checkbox", "Check or uncheck the task on this line", |ed, _| {
        let toggled = edit(ed, |d| {
            let line = d.buf.char_to_line(d.pos());
            let (col, checked) = checkbox(&d.buf.line_content(line).to_string())?;
            let (pos, at) = (d.pos(), d.buf.line_to_char(line) + col);
            d.replace_range(at..at + 1, if checked { " " } else { "x" });
            d.set_cursor(pos);
            Some(())
        });
        if toggled == Some(None) {
            ed.set_status("No checkbox on this line");
        }
    });
    c.register("markdown-indent", "Indent a list item from its start, else indent at point", |ed, _| {
        edit(ed, |d| {
            let line = d.buf.char_to_line(d.pos());
            let content = d.buf.line_content(line).to_string();
            if BULLETS.iter().any(|b| content.trim_start().starts_with(b)) {
                d.indent_line(&" ".repeat(d.buf.tab_width()));
            } else {
                d.indent();
            }
        });
    });

    ed.define_mode(
        Mode::new("Markdown")
            .comment("<!-- ")
            .extensions(&["md", "markdown"])
            .grammar(syntax::markdown)
            .line_face(|_, line| is_done_task(line).then_some(FaceId::SHADOW))
            .keys(&[("S-RET", "markdown-insert-list-item"), ("TAB", "markdown-indent")]),
    );
}

fn is_done_task(line: &str) -> bool {
    checkbox(line).is_some_and(|(_, checked)| checked)
}

/// A task item's check mark: its char column in `line` (the space or `x` inside `[ ]`)
/// and whether it is checked.
fn checkbox(line: &str) -> Option<(usize, bool)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let bullet = BULLETS.iter().find(|b| trimmed.starts_with(*b))?;
    let checked = match trimmed[bullet.len()..].get(..3)? {
        "[ ]" => false,
        "[x]" | "[X]" => true,
        _ => return None,
    };
    let indent_chars = line[..indent].chars().count();
    Some((indent_chars + bullet.len() + 1, checked))
}

enum Continuation {
    /// An empty item: remove it and end the list.
    EndList,
    /// Continue with this marker.
    Item(&'static str),
    Plain,
}

fn continuation(trimmed: &str) -> Continuation {
    if trimmed == "- [ ]" || BULLETS.contains(&trimmed) {
        return Continuation::EndList;
    }
    if ["- [ ] ", "- [x] ", "- [X] "].iter().any(|p| trimmed.starts_with(p)) {
        return Continuation::Item("- [ ] ");
    }
    match BULLETS.iter().find(|b| trimmed.starts_with(*b)) {
        Some(bullet) => Continuation::Item(bullet),
        None => Continuation::Plain,
    }
}
