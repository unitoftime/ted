use std::path::Path;

use ted_core::frame::DrawCmd;
use ted_core::syntax::{self, SyntaxKind};
use ted_core::{Buffer, Color, Editor, FaceId, Frame, KeyCode, KeyEvent, Metrics, Modifiers, Plugin};

fn editor_with(text: &str) -> Editor {
    let mut ed = Editor::new(&[]);
    ed.active_buffer_mut().set_text(text);
    ed
}

fn text(ed: &Editor) -> String {
    ed.active_buffer().to_string()
}

fn pos(ed: &Editor) -> usize {
    ed.active_view().cursor.pos
}

fn label(ed: &Editor) -> String {
    ed.top_modal().map(|m| m.label().to_string()).unwrap_or_default()
}

fn press(ed: &mut Editor, keys: &[KeyEvent]) {
    for key in keys {
        ed.handle_key(*key);
    }
}

fn type_keys(ed: &mut Editor, text: &str) {
    for ch in text.chars() {
        ed.handle_key(KeyEvent::plain_char(ch));
    }
}

/// Runs background job results until `done` holds.
fn wait_until(ed: &mut Editor, done: impl Fn(&Editor) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(ed) {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for background job");
        ed.poll(std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn ctrl(ch: char) -> KeyEvent {
    KeyEvent::ctrl(ch)
}

fn alt(ch: char) -> KeyEvent {
    KeyEvent::alt(ch)
}

fn enter() -> KeyEvent {
    KeyEvent::plain(KeyCode::Enter)
}

#[test]
fn test_kill_ring_coalesce_and_yank_pop() {
    let _clip_lock = ted_core::kill_ring::TEST_CLIPBOARD_LOCK.lock();
    let mut editor = editor_with("Line 1\nLine 2\nLine 3\n");

    // Consecutive kills coalesce: "Line 1", "\n", "Line 2"
    press(&mut editor, &[ctrl('k'), ctrl('k'), ctrl('k')]);
    assert_eq!(text(&editor), "\nLine 3\n");

    // Another command breaks the sequence
    editor.handle_key(KeyEvent::new(KeyCode::Char('>'), Modifiers::ALT));
    editor.doc().set_cursor(1);
    editor.handle_key(ctrl('k'));

    editor.handle_key(ctrl('y'));
    assert!(text(&editor).contains("Line 3"));

    // Yank-pop swaps in the older, coalesced kill
    editor.handle_key(alt('y'));
    assert!(text(&editor).contains("Line 1\nLine 2"));
}

#[test]
fn test_search_and_replace_workflow() {
    let mut editor = editor_with("apple banana apple cherry apple");

    editor.handle_key(ctrl('s'));
    assert!(editor.has_modal());
    type_keys(&mut editor, "banana");
    editor.handle_key(enter());
    assert_eq!(pos(&editor), 6);
    assert!(editor.status.contains("Found 'banana'"));

    editor.handle_key(ctrl('r'));
    type_keys(&mut editor, "apple");
    editor.handle_key(enter());
    assert_eq!(pos(&editor), 0);

    editor.execute("replace-string");
    assert_eq!(label(&editor), "Replace string: ");
    type_keys(&mut editor, "apple");
    editor.handle_key(enter());
    assert_eq!(label(&editor), "Replace 'apple' with: ");
    type_keys(&mut editor, "orange");
    editor.handle_key(enter());

    assert_eq!(text(&editor), "orange banana orange cherry orange");
    assert!(editor.status.contains("Replaced 3 occurrence(s)"));
}

#[test]
fn test_toggle_theme() {
    let mut editor = Editor::new(&[]);
    let theme = |ed: &Editor| ed.settings.get(ted_core::settings::THEME).to_string();
    let initial = theme(&editor);
    editor.execute("toggle-theme");
    assert_ne!(theme(&editor), initial);
    editor.execute("toggle-theme");
    assert_eq!(theme(&editor), initial);
}

#[test]
fn test_language_mode_comments() {
    let mut editor = Editor::new(&[]);
    let mode = |name: &str| editor.modes.for_path(Some(Path::new(name)));

    assert_eq!(mode("Cargo.toml").name, "TOML");
    assert_eq!(mode("Cargo.toml").comment_prefix, "# ");
    assert_eq!(mode("README.md").name, "Markdown");
    assert_eq!(mode("README.md").comment_prefix, "<!-- ");
    assert_eq!(mode("Makefile").name, "Makefile");
    assert_eq!(mode("Makefile").comment_prefix, "# ");
    assert_eq!(mode("Makefile").tab_width, Some(8));
    assert_eq!(Buffer::from_str("# Header").mode().name, "Plain Text");

    // Comment and uncomment the current line
    editor.active_buffer_mut().set_text("line 1\nline 2\n");
    editor.execute("comment-region");
    assert_eq!(text(&editor), "// line 1\nline 2\n");
    editor.execute("comment-region");
    assert_eq!(text(&editor), "line 1\nline 2\n");
}

#[test]
fn test_split_windows_share_kill_ring() {
    let _clip_lock = ted_core::kill_ring::TEST_CLIPBOARD_LOCK.lock();
    let mut editor = editor_with("First Line\nSecond Line\n");

    press(&mut editor, &[ctrl('x'), KeyEvent::plain_char('2')]);
    assert_eq!(editor.layout.leaf_ids().len(), 2);
    editor.handle_key(ctrl('k'));

    press(&mut editor, &[ctrl('x'), KeyEvent::plain_char('o')]);
    let end = editor.active_buffer().len_chars();
    editor.doc().set_cursor(end);
    editor.handle_key(ctrl('y'));
    assert!(text(&editor).contains("First Line"), "Text killed in one pane can be yanked in another");
}

struct GreeterPlugin;

#[derive(Default)]
struct Greetings(usize);

impl Plugin for GreeterPlugin {
    fn name(&self) -> &str {
        "greeter"
    }

    fn init(&mut self, editor: &mut Editor) {
        editor.commands.register("greeter-hello", "Say hello", |ed, _| {
            let greetings = ed.ext_mut::<Greetings>();
            greetings.0 += 1;
            let count = greetings.0;
            ed.set_status(format!("Hello #{}", count));
        });
        editor.bind("global", "C-F", "greeter-hello").unwrap();
        editor.hooks.buffer_saved.push(std::rc::Rc::new(|ed, _| ed.set_status("greeter saw a save")));
    }
}

#[test]
fn test_plugin_commands_bindings_and_state() {
    let mut editor = Editor::new(&[]);
    editor.add_plugin(GreeterPlugin);

    // Ctrl+Shift+F reaches the plugin's binding
    editor.handle_key(KeyEvent::new(KeyCode::Char('f'), Modifiers { ctrl: true, alt: false, shift: true }));
    assert_eq!(editor.status, "Hello #1");
    editor.execute("greeter-hello");
    assert_eq!(editor.status, "Hello #2", "Plugin state persists across commands");

    // Plugin commands show up in M-x like built-ins
    assert!(editor.commands.id("greeter-hello").is_some());
}

#[test]
fn test_init_script_rebinds_keys_and_sets_options() {
    let ops = ted_core::config::run_script(
        r##"
        set("wrap_lines", false);
        set("tab_width", 3);
        set("compile.command", "cargo build");
        mode("rust", #{ tab_width: 2, indent: "tabs" });
        bind("C-c z", "split-window-right");
        bind("C-c 9", "restore-window-layout 2");
        unbind("C-o");
        bind_mode("markdown", "C-c C-t", "toggle-theme");
        face("keyword", #{ fg: "#ff0000", bold: true });
        "##,
    )
    .unwrap();
    let mut editor = Editor::new(&[]);
    assert!(editor.apply_config(&ops).is_empty());
    assert!(!editor.active_view().wrap);
    assert_eq!(editor.settings.entry("compile.command").unwrap().value, ted_core::Value::from("cargo build"));
    assert_eq!(editor.modes.get("Rust").unwrap().tab_width, Some(2));
    assert_eq!(editor.active_buffer().tab_width(), 3, "modes without a width follow tab_width");
    assert_eq!(editor.faces.fg(FaceId::KEYWORD), Color::rgb(255, 0, 0));
    editor.execute("toggle-theme");
    assert_eq!(editor.faces.fg(FaceId::KEYWORD), Color::rgb(255, 0, 0), "user faces survive theme switches");

    press(&mut editor, &[ctrl('c'), KeyEvent::plain_char('z')]);
    assert_eq!(editor.layout.leaf_ids().len(), 2);

    editor.active_buffer_mut().set_text("ab");
    editor.handle_key(ctrl('o'));
    assert_eq!(text(&editor), "ab", "Unbound keys do nothing");

    let errors = editor.apply_config(&ted_core::config::run_script(r#"bind("C-c q", "no-such-command");"#).unwrap());
    assert_eq!(errors.len(), 1);
}

#[test]
fn test_reload_init_restores_defaults() {
    let dir = std::env::temp_dir().join(format!("ted_reload_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("init.rhai");
    std::fs::write(&script, r#"bind("C-x f", "split-window-right"); set("wrap_lines", false);"#).unwrap();
    let options = ted_core::StartupOptions { init_script: Some(script.clone()), ..Default::default() };
    let mut editor = Editor::with_options(&[], options);

    press(&mut editor, &[ctrl('x'), KeyEvent::plain_char('f')]);
    assert_eq!(editor.layout.leaf_ids().len(), 2);
    assert!(!editor.active_view().wrap);

    std::fs::write(&script, "").unwrap();
    editor.execute("reload-init");
    assert!(editor.active_view().wrap, "settings the script no longer sets return to their defaults");
    press(&mut editor, &[ctrl('x'), KeyEvent::plain_char('f')]);
    assert!(editor.has_modal(), "bindings the script no longer makes return to their defaults (find-file)");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_tree_sitter_syntax_highlighting() {
    let editor = Editor::new(&[]);
    let buffer = |text: &str, path: &str| {
        let mut buf = Buffer::from_str(text);
        buf.set_mode(editor.modes.for_path(Some(Path::new(path))));
        buf
    };
    let has = |tokens: &[ted_core::SyntaxToken], kind: SyntaxKind, cols: Option<(usize, usize)>| {
        tokens.iter().any(|t| t.kind == kind && cols.is_none_or(|(s, e)| t.start_col == s && t.end_col == e))
    };

    // Rust
    let mut rust = buffer("fn main() {\n    let x = 42;\n}\n", "main.rs");
    let tokens = rust.highlight(0..3);
    assert!(has(&tokens[0], SyntaxKind::Keyword, Some((0, 2)))); // fn
    assert!(has(&tokens[0], SyntaxKind::Function, Some((3, 7)))); // main
    assert!(has(&tokens[1], SyntaxKind::Keyword, Some((4, 7)))); // let
    assert!(
        has(&tokens[1], SyntaxKind::Constant, Some((12, 14))) || has(&tokens[1], SyntaxKind::Number, Some((12, 14)))
    );

    // Incremental edits keep highlighting correct
    rust.insert(12, "    let s = \"hello\";\n");
    let tokens = rust.highlight(0..4);
    assert!(has(&tokens[1], SyntaxKind::StringLiteral, None));

    // Python, Go, C
    let tokens = buffer("def greet(name):\n    return 123\n", "script.py").highlight(0..2);
    assert!(has(&tokens[0], SyntaxKind::Keyword, None));
    assert!(has(&tokens[0], SyntaxKind::Function, None));
    let tokens = buffer("package main\n\nfunc add(a int) int {\n    return a + 1\n}\n", "main.go").highlight(0..5);
    assert!(has(&tokens[0], SyntaxKind::Keyword, None));
    let tokens = buffer("int main() {\n    return 0;\n}\n", "main.c").highlight(0..3);
    assert!(has(&tokens[0], SyntaxKind::Type, None));
    assert!(has(&tokens[0], SyntaxKind::Function, None));
}

#[test]
fn test_refresh_button_in_dired_and_compilation_buffers() {
    let temp_dir = std::env::temp_dir().join(format!("ted_test_refresh_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    std::fs::write(temp_dir.join("file1.txt"), "hello").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&temp_dir));
    assert_eq!(editor.active_buffer().mode().name, "Dired");
    assert!(editor.active_buffer().is_read_only());
    assert!(text(&editor).contains("file1.txt"));
    assert!(!text(&editor).contains("file2.txt"));

    // 'g' re-reads the directory
    std::fs::write(temp_dir.join("file2.txt"), "world").unwrap();
    editor.handle_key(KeyEvent::plain_char('g'));
    assert!(text(&editor).contains("file2.txt"), "'g' must refresh the listing");

    // Compilation prompts with the configured command, then streams from a background job
    // into a read-only buffer
    let set_command = |ed: &mut Editor, command: &str| ed.set_setting("compile.command", &command.into()).unwrap();
    set_command(&mut editor, "echo compilation_test_output_123; echo 'error: boom'");
    editor.execute("compile");
    editor.handle_key(enter());
    assert_eq!(editor.active_buffer().mode().name, "Compilation");
    assert!(editor.active_buffer().is_read_only());
    wait_until(&mut editor, |ed| ed.status.starts_with("Compilation finished"));
    assert!(text(&editor).contains("compilation_test_output_123"));
    let len = editor.active_buffer().len_chars();
    assert!(
        editor.active_buffer().decorations().overlapping(0..len).any(|d| d.face == FaceId::ERROR),
        "error lines are decorated"
    );

    // 'g' reruns the buffer's own command
    set_command(&mut editor, "echo something_else");
    editor.set_status("");
    editor.handle_key(KeyEvent::plain_char('g'));
    wait_until(&mut editor, |ed| ed.status.starts_with("Compilation finished"));
    assert!(text(&editor).contains("compilation_test_output_123"));
    assert!(!text(&editor).contains("something_else"));

    // In an ordinary file 'g' inserts itself
    let normal = temp_dir.join("normal.txt");
    std::fs::write(&normal, "start ").unwrap();
    editor.open_file(&normal).unwrap();
    assert!(!editor.active_buffer().is_read_only());
    editor.handle_key(KeyEvent::plain_char('g'));
    assert_eq!(text(&editor), "gstart ");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_compilation_errors_are_a_location_list() {
    let dir = std::env::temp_dir().join(format!("ted_test_next_error_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.c"), "int a;\nint b\nint c\n").unwrap();
    let mut editor = Editor::new(&[dir.join("a.c")]);

    let output = "printf 'a.c:2:6: error: expected semicolon\\nnoise\\na.c:3:1: warning: w\\n'; exit 2";
    editor.execute_with("compile", ted_core::Arg::Str(output.into()));
    wait_until(&mut editor, |ed| ed.status.starts_with("Compilation exited abnormally with code 2"));
    assert!(editor.status.ends_with("(1 error, 1 warning)"), "{}", editor.status);

    // M-g n visits each error in turn from the compilation buffer's window
    press(&mut editor, &[alt('g'), KeyEvent::plain_char('n')]);
    assert_eq!(editor.active_buffer().name(), "a.c");
    assert_eq!(editor.active_buffer().char_to_point(pos(&editor)), (1, 5));
    press(&mut editor, &[alt('g'), KeyEvent::plain_char('n')]);
    assert_eq!(editor.active_buffer().char_to_point(pos(&editor)), (2, 0));
    press(&mut editor, &[alt('g'), KeyEvent::plain_char('n')]);
    assert_eq!(editor.status, "No more errors");
    press(&mut editor, &[alt('g'), KeyEvent::plain_char('p')]);
    assert_eq!(editor.active_buffer().char_to_point(pos(&editor)), (1, 5));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_dired_enter_visits_entries() {
    let temp_dir = std::env::temp_dir().join(format!("ted_test_dired_open_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    std::fs::write(temp_dir.join("a.txt"), "inside a").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&temp_dir));
    let line = (0..editor.active_buffer().len_lines())
        .find(|&l| editor.active_buffer().line_content(l).to_string().ends_with(" a.txt"))
        .unwrap();
    let start = editor.active_buffer().line_to_char(line);
    editor.doc().set_cursor(start);
    editor.handle_key(enter());
    assert_eq!(text(&editor), "inside a");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_shift_enter_fallthrough_markdown_and_general() {
    let mut editor = Editor::new(&[]);
    let temp_dir = std::env::temp_dir().join(format!("ted_test_shift_enter_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let shift_enter = KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT);

    let mut check = |name: &str, content: &str, key: KeyEvent, expected: &str| {
        let file = temp_dir.join(name);
        std::fs::write(&file, content).unwrap();
        editor.open_file(&file).unwrap();
        let end = editor.active_buffer().len_chars();
        editor.doc().set_cursor(end);
        editor.handle_key(key);
        assert_eq!(text(&editor), expected, "{}", name);
    };

    // Plain RET never continues lists; S-RET does
    check("test1.md", "- [ ] First item", enter(), "- [ ] First item\n");
    check("test2.md", "- [ ] First item", shift_enter, "- [ ] First item\n- [ ] ");
    check("test3.md", "- Second item", enter(), "- Second item\n");
    check("test4.md", "- Second item", shift_enter, "- Second item\n- ");
    // Elsewhere S-RET falls through to RET (newline keeping indentation)
    check("test.rs", "fn main() {\n    let x = 1;", shift_enter, "fn main() {\n    let x = 1;\n    ");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_selection_marking_and_delete_selection() {
    let mut editor = editor_with("Hello World from ted");
    editor.doc().set_cursor(6);

    editor.handle_key(ctrl(' '));
    assert_eq!(editor.status, "Mark set");
    assert_eq!(editor.active_view().cursor.mark, Some(6));

    for _ in 0..5 {
        editor.handle_key(KeyEvent::plain(KeyCode::Right));
    }
    assert_eq!(pos(&editor), 11);
    assert!(editor.active_view().cursor.region().is_some());

    // Typing replaces the selection
    editor.handle_key(KeyEvent::plain_char('E'));
    assert_eq!(text(&editor), "Hello E from ted");
    assert_eq!(editor.active_view().cursor.mark, None);

    // Backspace deletes the selection
    editor.doc().set_cursor(0);
    editor.handle_key(ctrl(' '));
    for _ in 0..5 {
        editor.handle_key(KeyEvent::plain(KeyCode::Right));
    }
    editor.handle_key(KeyEvent::plain(KeyCode::Backspace));
    assert_eq!(text(&editor), " E from ted");
    assert!(editor.active_view().cursor.region().is_none());

    // C-g deactivates the mark
    editor.handle_key(ctrl(' '));
    assert!(editor.active_view().cursor.mark.is_some());
    editor.handle_key(ctrl('g'));
    assert_eq!(editor.active_view().cursor.mark, None);
    assert_eq!(editor.status, "Quit");
}

#[test]
fn test_emacs_copy_cut_paste_and_mark_exchange() {
    let _clip_lock = ted_core::kill_ring::TEST_CLIPBOARD_LOCK.lock();
    let mut editor = editor_with("Apple Banana Cherry");
    editor.doc().set_cursor(6);

    editor.handle_key(ctrl(' '));
    for _ in 0..6 {
        editor.handle_key(KeyEvent::plain(KeyCode::Right));
    }

    press(&mut editor, &[ctrl('x'), ctrl('x')]);
    assert_eq!(pos(&editor), 6);
    assert_eq!(editor.active_view().cursor.mark, Some(12));
    assert_eq!(editor.status, "Exchanged point and mark");

    editor.handle_key(alt('w'));
    assert_eq!(editor.status, "Copied region");
    assert!(editor.active_view().cursor.region().is_none());

    let end = editor.active_buffer().len_chars();
    editor.doc().set_cursor(end);
    editor.handle_key(ctrl('y'));
    assert_eq!(text(&editor), "Apple Banana CherryBanana");

    editor.doc().set_cursor(12);
    editor.handle_key(ctrl(' '));
    for _ in 0..7 {
        editor.handle_key(KeyEvent::plain(KeyCode::Right));
    }
    editor.handle_key(ctrl('w'));
    assert_eq!(editor.status, "Killed region");
    assert_eq!(text(&editor), "Apple BananaBanana");

    press(&mut editor, &[ctrl('x'), KeyEvent::plain_char('h')]);
    assert_eq!(editor.status, "Mark set (buffer)");
    assert_eq!(pos(&editor), 0);
    assert_eq!(editor.active_view().cursor.mark, Some(editor.active_buffer().len_chars()));

    editor.doc().clear_mark();
    editor.doc().set_cursor(0);
    editor.handle_key(alt('d'));
    assert_eq!(text(&editor), " BananaBanana");
}

#[test]
fn test_shift_selection_and_mouse_drag() {
    let mut editor = editor_with("ABCDEFGHIJ");
    editor.doc().set_cursor(3);

    let shift_right = KeyEvent::new(KeyCode::Right, Modifiers::SHIFT);
    press(&mut editor, &[shift_right, shift_right, shift_right]);
    let cursor = &editor.active_view().cursor;
    assert_eq!(cursor.mark, Some(3));
    assert_eq!(cursor.pos, 6);
    assert!(cursor.shift_selected);

    // An unshifted motion ends the shift selection
    editor.handle_key(KeyEvent::plain(KeyCode::Left));
    let cursor = &editor.active_view().cursor;
    assert_eq!(cursor.mark, None);
    assert!(!cursor.shift_selected);
    assert_eq!(cursor.pos, 5);

    let mut frame = Frame::new(800.0, 600.0, editor.faces.bg(FaceId::DEFAULT));
    editor.render(&mut frame, Metrics::new(8.0, 20.0));
    let gutter_w = editor.doc().gutter_cols() as f32 * 8.0;

    editor.handle_mouse_press(gutter_w + 2.0 * 8.0, 50.0, 1);
    let clicked = pos(&editor);
    assert_eq!(editor.active_view().cursor.mark, None);

    editor.handle_mouse_drag(gutter_w + 6.0 * 8.0, 50.0);
    assert_eq!(editor.active_view().cursor.mark, Some(clicked));
    assert!(pos(&editor) > clicked);

    // A double-click selects the whole line, and dragging keeps whole lines
    editor.handle_mouse_press(gutter_w + 2.0 * 8.0, 50.0, 2);
    assert_eq!(editor.active_view().cursor.mark, Some(0));
    assert_eq!(pos(&editor), 10);
    editor.handle_mouse_drag(gutter_w + 1.0 * 8.0, 50.0);
    assert_eq!((editor.active_view().cursor.mark, pos(&editor)), (Some(0), 10));
}

#[test]
fn test_selection_visual_rendering() {
    let mut editor = editor_with("Line 1 Alpha\nLine 2 Beta\nLine 3 Gamma\n");
    editor.handle_key(ctrl(' '));
    for _ in 0..10 {
        editor.handle_key(KeyEvent::plain(KeyCode::Right));
    }
    let sel_bg = editor.faces.bg(FaceId::REGION);
    let has_selection =
        |frame: &Frame| frame.commands.iter().any(|cmd| matches!(cmd, DrawCmd::Rect { color, .. } if *color == sel_bg));

    for wrap in [false, true] {
        editor.active_view_mut().wrap = wrap;
        let mut frame = Frame::new(800.0, 600.0, editor.faces.bg(FaceId::DEFAULT));
        editor.render(&mut frame, Metrics::new(8.0, 20.0));
        assert!(has_selection(&frame), "selection drawn (wrap = {})", wrap);
    }
}

#[test]
fn test_buffer_start_and_end_hotkeys() {
    let mut editor = editor_with("Line 1\nLine 2\nLine 3\nLine 4\nLine 5");
    editor.doc().set_cursor(15);
    let total = editor.active_buffer().len_chars();
    let alt_shift = Modifiers { ctrl: false, alt: true, shift: true };

    // Shift+Alt+, / Shift+Alt+. as reported by the physical keys
    editor.handle_key(KeyEvent::new(KeyCode::Char(','), alt_shift));
    assert_eq!(pos(&editor), 0);
    editor.handle_key(KeyEvent::new(KeyCode::Char('.'), alt_shift));
    assert_eq!(pos(&editor), total);

    // The shifted characters, with and without the shift flag
    for mods in [alt_shift, Modifiers::ALT] {
        editor.handle_key(KeyEvent::new(KeyCode::Char('<'), mods));
        assert_eq!(pos(&editor), 0);
        editor.handle_key(KeyEvent::new(KeyCode::Char('>'), mods));
        assert_eq!(pos(&editor), total);
    }

    // With a trailing newline, end-of-buffer stops on the last text line
    editor.active_buffer_mut().set_text("line 1\nline 2\nline 3\n");
    editor.doc().set_cursor(0);
    editor.handle_key(KeyEvent::new(KeyCode::Char('.'), alt_shift));
    assert_eq!(editor.active_buffer().char_to_point(pos(&editor)), (2, 6));
}

#[test]
fn test_search_incremental_typing_and_c_s_c_r_stepping() {
    let mut editor = editor_with("apple banana apple cherry apple dog");

    editor.handle_key(ctrl('s'));
    assert_eq!(label(&editor), "I-search: ");

    editor.handle_key(KeyEvent::plain_char('a'));
    assert_eq!(pos(&editor), 0);
    type_keys(&mut editor, "pple");
    assert_eq!(pos(&editor), 0);

    for expected in [13, 26, 0] {
        editor.handle_key(ctrl('s'));
        assert_eq!(pos(&editor), expected);
    }
    for expected in [26, 13, 0] {
        editor.handle_key(ctrl('r'));
        assert_eq!(pos(&editor), expected);
    }

    editor.handle_key(enter());
    assert!(!editor.has_modal());
    assert_eq!(pos(&editor), 0);
    assert!(editor.status.contains("Found 'apple'"));
}

#[test]
fn test_search_c_s_c_s_repeats_last_query() {
    let mut editor = editor_with("cat dog cat dog cat");

    editor.handle_key(ctrl('s'));
    type_keys(&mut editor, "dog");
    editor.handle_key(enter());
    assert_eq!(pos(&editor), 4);

    editor.doc().set_cursor(5);

    // C-s C-s recalls "dog"
    press(&mut editor, &[ctrl('s'), ctrl('s')]);
    assert_eq!(pos(&editor), 12);
    assert_eq!(editor.input().unwrap().text(), "dog");

    editor.handle_key(ctrl('r'));
    assert_eq!(pos(&editor), 4);

    // Escape restores the starting position
    editor.handle_key(KeyEvent::plain(KeyCode::Escape));
    assert!(!editor.has_modal());
    assert_eq!(pos(&editor), 5);
}

#[test]
fn test_search_at_start_of_buffer_and_backward_initial() {
    let mut editor = editor_with("target middle target end");
    editor.doc().set_cursor(20);

    editor.handle_key(ctrl('r'));
    assert_eq!(label(&editor), "I-search backward: ");
    type_keys(&mut editor, "target");
    assert_eq!(pos(&editor), 14);

    editor.handle_key(ctrl('r'));
    assert_eq!(pos(&editor), 0);

    editor.handle_key(enter());
    assert!(!editor.has_modal());
    assert_eq!(pos(&editor), 0);
}

/// Keys search doesn't use end it at the match and act from there; its own keys (typing,
/// line editing) still edit the query.
#[test]
fn test_search_ends_on_keys_it_does_not_use() {
    let mut editor = editor_with("one\ntwo target\nthree\nfour");
    editor.handle_key(ctrl('s'));
    type_keys(&mut editor, "targ");
    editor.handle_key(ctrl('a'));
    type_keys(&mut editor, "x");
    assert_eq!(editor.top_modal().and_then(|m| m.input()).map(|i| i.text().to_string()).as_deref(), Some("xtarg"));
    editor.handle_key(KeyEvent::plain(KeyCode::Backspace));
    editor.handle_key(ctrl('e'));
    type_keys(&mut editor, "et");
    assert_eq!(label(&editor), "I-search: ");
    assert_eq!(editor.active_buffer().char_to_point(pos(&editor)), (1, 4));

    // At the match after a C-s spree, C-n just moves on down from it.
    editor.handle_key(ctrl('s'));
    editor.handle_key(ctrl('n'));
    assert!(!editor.has_modal());
    assert_eq!(editor.active_buffer().char_to_point(pos(&editor)), (2, 4));
    assert_eq!(editor.active_view().highlight, None);
}

#[test]
fn test_c_o_open_line() {
    let mut editor = editor_with("helloworld");
    editor.doc().set_cursor(5);

    editor.handle_key(ctrl('o'));
    assert_eq!(text(&editor), "hello\nworld");
    assert_eq!(pos(&editor), 5);

    editor.handle_key(KeyEvent::plain_char('!'));
    assert_eq!(text(&editor), "hello!\nworld");
    assert_eq!(pos(&editor), 6);

    // With a selection, C-o replaces it
    editor.active_buffer_mut().set_text("abcdef");
    editor.doc().set_cursor(1);
    editor.doc().toggle_mark();
    editor.doc().set_cursor(4);
    editor.handle_key(ctrl('o'));
    assert_eq!(text(&editor), "a\nef");
    assert_eq!(pos(&editor), 1);
}

#[test]
fn test_c_l_recenter() {
    let lines: String = (0..100).map(|i| format!("line {}\n", i)).collect();
    let mut editor = editor_with(&lines);
    let line50 = editor.active_buffer().line_to_char(50);
    editor.doc().set_cursor(line50);

    let rows = editor.active_view().text_rows();
    let half = rows / 2;
    let top = |ed: &Editor| ed.active_view().top_line;

    editor.handle_key(ctrl('l'));
    assert_eq!(top(&editor), 50 - half, "1st C-l centers");
    editor.handle_key(ctrl('l'));
    assert_eq!(top(&editor), 50, "2nd C-l puts point at the top");
    editor.handle_key(ctrl('l'));
    assert_eq!(top(&editor), 50 - (rows - 1), "3rd C-l puts point at the bottom");
    editor.handle_key(ctrl('l'));
    assert_eq!(top(&editor), 50 - half, "4th C-l cycles back to center");

    // Any other command restarts the cycle
    editor.handle_key(KeyEvent::plain(KeyCode::Down));
    editor.handle_key(ctrl('l'));
    assert_eq!(top(&editor), 51 - half);

    // At the bottom position the cursor stays on screen, above the modeline
    press(&mut editor, &[ctrl('l'), ctrl('l')]);
    let mut frame = Frame::new(800.0, 524.0, Color::BLACK);
    editor.render(&mut frame, Metrics::new(9.0, 20.0));
    let c = frame.cursor.expect("cursor visible");
    assert!(c.y >= 0.0 && c.y + c.h <= 500.0 - 20.0, "cursor must not be under the modeline");
}

#[test]
fn test_c_d_delete_forward() {
    let mut editor = editor_with("abcdef\n12345");
    editor.doc().set_cursor(2);

    editor.handle_key(ctrl('d'));
    assert_eq!(text(&editor), "abdef\n12345");
    assert_eq!(pos(&editor), 2);

    editor.doc().set_cursor(5);
    editor.handle_key(ctrl('d'));
    assert_eq!(text(&editor), "abdef12345");
    assert_eq!(pos(&editor), 5);

    editor.doc().set_cursor(2);
    editor.doc().toggle_mark();
    editor.doc().set_cursor(5);
    editor.handle_key(ctrl('d'));
    assert_eq!(text(&editor), "ab12345");
    assert_eq!(pos(&editor), 2);
}

#[test]
fn test_markdown_heading_highlight() {
    let grammar = syntax::markdown().expect("markdown grammar");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar.language).unwrap();

    let rope = ropey::Rope::from_str(
        "# Heading 1\nParagraph text\n## Subheading\n- [x] Done\n- [ ] Todo\n```rust\nfn main() {}\n```\n",
    );
    let tree = syntax::parse_rope(&mut parser, &rope, None).unwrap();
    let tokens = syntax::highlight_lines(&tree, &grammar, &rope, 0..8);
    let has = |line: usize, kind: SyntaxKind, s: usize, e: usize| {
        tokens[line].iter().any(|t| t.kind == kind && t.start_col == s && t.end_col == e)
    };

    assert!(has(0, SyntaxKind::Punctuation, 0, 1));
    assert!(has(0, SyntaxKind::Heading, 2, 11));
    assert!(has(2, SyntaxKind::Punctuation, 0, 2));
    assert!(has(2, SyntaxKind::Heading, 3, 13));
    assert!(has(3, SyntaxKind::Punctuation, 0, 2));
    assert!(has(3, SyntaxKind::Constant, 2, 5));
    assert!(has(4, SyntaxKind::Punctuation, 2, 5));
    assert!(has(5, SyntaxKind::Keyword, 3, 7));
    assert!(tokens[6].iter().any(|t| t.kind == SyntaxKind::StringLiteral));
}

#[test]
fn test_wrapped_line_cursor_up_down_navigates_visual_chunks() {
    let text = format!("short\n{}\nend\n", "A".repeat(200));
    let mut editor = editor_with(&text);
    editor.active_view_mut().wrap = true;

    // An 800x600 window at 9px per column
    let mut frame = Frame::new(800.0, 624.0, Color::BLACK);
    editor.render(&mut frame, Metrics::new(9.0, 20.0));
    let cols = editor.doc().content_cols();
    assert!(cols > 10 && cols < 200, "the long line must wrap ({} cols)", cols);

    let line1 = editor.active_buffer().line_to_char(1);
    editor.doc().set_cursor(line1);
    let point = |ed: &Editor| ed.active_buffer().char_to_point(pos(ed));

    editor.execute("next-line");
    let (line, col) = point(&editor);
    assert_eq!(line, 1, "down stays on the wrapped line");
    assert!(col >= cols);

    editor.execute("next-line");
    let (line, col) = point(&editor);
    assert_eq!(line, 1);
    assert!(col >= cols * 2);

    editor.execute("previous-line");
    let (line, col) = point(&editor);
    assert_eq!(line, 1);
    assert!(col >= cols && col < cols * 2);

    editor.execute("previous-line");
    let (line, col) = point(&editor);
    assert_eq!(line, 1);
    assert!(col < cols);

    editor.execute("previous-line");
    assert_eq!(point(&editor).0, 0);

    // Down from the last visual row moves to the next line
    editor.doc().set_cursor(line1 + 199);
    editor.execute("next-line");
    assert_eq!(point(&editor).0, 2);
}
