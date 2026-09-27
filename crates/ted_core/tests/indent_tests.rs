//! TAB reindents from the syntax tree: each fixture is laid out the way its language's
//! formatter would, flattened, and must come back unchanged. Lines inside strings and
//! comments start at column 0, since reindenting leaves them alone.

use std::path::Path;

use ted_core::{Buffer, Editor};

fn editor_with(text: &str, path: &str) -> Editor {
    let mut ed = Editor::new(&[]);
    let mut buf = Buffer::from_str(text);
    buf.set_mode(ed.modes.for_path(Some(Path::new(path))));
    let id = ed.add_buffer(buf);
    ed.show_buffer(id);
    ed
}

fn text(ed: &Editor) -> String {
    ed.active_buffer().to_string()
}

#[track_caller]
fn assert_reindents(path: &str, formatted: &str) {
    let flat: String = formatted.lines().map(|l| format!("{}\n", l.trim_start())).collect();
    assert_reindents_to(path, &flat, formatted);
}

#[track_caller]
fn assert_reindents_to(path: &str, input: &str, expected: &str) {
    let mut ed = editor_with(input, path);
    ed.focused_doc().mark_whole_buffer();
    ed.execute("indent-for-tab-command");
    assert_eq!(text(&ed), expected, "reindenting {}", path);
}

#[test]
fn test_reindent_rust() {
    assert_reindents(
        "main.rs",
        r#"impl Foo {
    /// Docs.
    fn run<T>(&self, x: T) -> u32
    where
        T: Copy,
    {
        let total = self
            .items
            .iter()
            .map(|x| {
                x + 1
            })
            .sum();
        match x {
            A => {
                call(
                    1,
                    2,
                );
            }
            B =>
                other(),
        }
        if a
            && b
        {
            go();
        } else {
            stop();
        }
        let s = "multi
line string";
        /* block
comment */
        total
    }
}
"#,
    );
}

#[test]
fn test_reindent_go() {
    assert_reindents(
        "main.go",
        "package main

import (
\t\"fmt\"
)

const (
\tA = 1
\tB = 2
)

type T struct {
\tName string
}

func run(x int) {
\tswitch x {
\tcase 1:
\t\tfmt.Println(
\t\t\t\"one\",
\t\t)
\tdefault:
\t\treturn
\t}
\tgo func() {
\t\twork()
\t}()
}
",
    );
}

/// Python's indentation is its block structure, so only what that leaves open is fixed:
/// continuation lines and branches that must line up with their statement.
#[test]
fn test_reindent_python() {
    let formatted = r#"class A:
    def f(self, x):
        if x:
            return [
                1,
                2,
            ]
        elif y:
            pass
        else:
            try:
                g()
            except E:
                h()
            finally:
                done()
        for i in x:
            print(i)
        return 2
"#;
    assert_reindents_to("main.py", formatted, formatted);
    let input = "def f(x):\n    if x:\n        g(\n1,\n            )\n    else:\n        pass\n";
    let expected = "def f(x):\n    if x:\n        g(\n            1,\n        )\n    else:\n        pass\n";
    assert_reindents_to("main.py", input, expected);
}

#[test]
fn test_reindent_c_js_and_bash() {
    assert_reindents(
        "main.c",
        r#"int main(void) {
    switch (x) {
        case 1:
            run();
            break;
    }
    struct point p = {
        .x = 1,
    };
    return 0;
}
"#,
    );
    assert_reindents(
        "app.tsx",
        r#"function App(props: Props) {
    const items = props.list
        .filter((x) => {
            return x.ok;
        })
        .map(render);
    return (
        <div>
            <span />
        </div>
    );
}
"#,
    );
    assert_reindents(
        "run.sh",
        r#"for f in *; do
    if [ -d "$f" ]; then
        echo dir
    elif [ -f "$f" ]; then
        echo file
    else
        echo other
    fi
done
"#,
    );
}

#[test]
fn test_outdent_shifts_the_region_and_keeps_it() {
    let mut ed = editor_with("a:\n        b: 1\n    c: 2\nd: 3\n", "config.yaml");
    ed.focused_doc().select_lines(1, 2);
    ed.execute("outdent");
    assert_eq!(text(&ed), "a:\n    b: 1\nc: 2\nd: 3\n");
    ed.execute("outdent");
    assert_eq!(text(&ed), "a:\nb: 1\nc: 2\nd: 3\n");
}

#[test]
fn test_tab_on_a_line_moves_past_its_new_indentation() {
    let mut ed = editor_with("fn main() {\nfoo();\n}\n", "main.rs");
    ed.focused_doc().set_cursor(12);
    ed.execute("indent-for-tab-command");
    assert_eq!(text(&ed), "fn main() {\n    foo();\n}\n");
    assert_eq!(ed.active_view().cursor.pos, 16);
}
