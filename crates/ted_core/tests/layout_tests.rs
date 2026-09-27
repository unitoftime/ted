use std::path::PathBuf;

use ted_core::{Editor, Frame, KeyCode, KeyEvent, Metrics, Modifiers, SplitType};

fn ctrl(ch: char) -> KeyEvent {
    KeyEvent::ctrl(ch)
}

fn plain(ch: char) -> KeyEvent {
    KeyEvent::plain_char(ch)
}

fn modal_id(ed: &Editor) -> Option<String> {
    ed.top_modal().map(|m| m.id().to_string())
}

#[test]
fn test_split_horizontal_clones_view() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    assert_eq!(editor.layout.leaf_ids().len(), 1);

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('2'));
    assert_eq!(editor.layout.leaf_ids().len(), 2, "C-x 2 must create a second pane");

    let views = editor.layout.views();
    assert_eq!(views[0].buffer, views[1].buffer, "Both panes show the same buffer");
    let buf = &editor.buffers[views[0].buffer];
    assert_eq!(buf.name(), "Cargo.toml");
    assert!(!buf.to_string().is_empty());
}

#[test]
fn test_split_vertical_with_ctrl_held() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    // C-x C-3 (Ctrl held throughout)
    editor.handle_key(ctrl('x'));
    editor.handle_key(KeyEvent::new(KeyCode::Char('3'), Modifiers::CTRL));
    assert_eq!(editor.layout.leaf_ids().len(), 2);
    let views = editor.layout.views();
    assert_eq!(views[0].buffer, views[1].buffer);
}

#[test]
fn test_other_window_cycling() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    let first = editor.layout.active_id();
    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('2'));
    assert_eq!(editor.layout.active_id(), first, "Splitting keeps the original window active");

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('o'));
    let second = editor.layout.active_id();
    assert_ne!(first, second, "C-x o must switch windows");

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('o'));
    assert_eq!(editor.layout.active_id(), first);
}

#[test]
fn test_maximize_active_window() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    editor.layout.split(SplitType::Horizontal);
    editor.layout.split(SplitType::Vertical);
    assert_eq!(editor.layout.leaf_ids().len(), 3);

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('1'));
    assert_eq!(editor.layout.leaf_ids().len(), 1, "C-x 1 must keep only the current window");
}

#[test]
fn test_close_active_window() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    editor.layout.split(SplitType::Horizontal);

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('0'));
    assert_eq!(editor.layout.leaf_ids().len(), 1, "C-x 0 must close the current window");

    // Closing the last window exits
    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('0'));
    assert!(!editor.running);
}

#[test]
fn test_close_active_window_dirty_buffer_prompt() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    editor.active_buffer_mut().insert(0, "# dirty edit\n");
    assert!(editor.active_buffer().is_dirty());

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('0'));
    assert_eq!(modal_id(&editor).as_deref(), Some("save-some-buffers"));
    assert!(editor.running);

    // q cancels the exit.
    editor.handle_key(plain('q'));
    assert!(!editor.has_modal());
    assert!(editor.running);

    // n leaves the file unsaved and exits.
    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('0'));
    editor.handle_key(plain('n'));
    assert!(!editor.running);
    assert!(editor.active_buffer().is_dirty());
}

#[test]
fn test_kill_buffer_keybinding_c_x_k() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);

    // Clean buffer: killed immediately, falling back to *scratch*
    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('k'));
    assert_eq!(editor.layout.leaf_ids().len(), 1);
    assert_eq!(editor.active_buffer().name(), "*scratch*");
    assert!(editor.status.contains("Killed buffer Cargo.toml"));

    // Dirty buffer: asks first
    editor.active_buffer_mut().insert(0, "dirty text");
    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('k'));
    assert_eq!(modal_id(&editor).as_deref(), Some("kill-buffer"));

    editor.handle_key(plain('n'));
    assert!(!editor.has_modal());
    assert_eq!(editor.active_buffer().to_string(), "dirty text");

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('k'));
    editor.handle_key(plain('y'));
    assert!(!editor.has_modal());
    // The last scratch buffer is cleared rather than removed
    assert_eq!(editor.active_buffer().to_string(), "");
    assert_eq!(editor.active_buffer().name(), "*scratch*");
}

#[test]
fn test_open_file_in_active_window_does_not_split() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    editor.layout.split(SplitType::Horizontal);
    let active = editor.layout.active_id();

    editor.open_file("src/lib.rs").unwrap();
    assert_eq!(editor.layout.leaf_ids().len(), 2, "Opening a file must not split");
    assert_eq!(editor.layout.active_id(), active);
    let expected = PathBuf::from("src/lib.rs").canonicalize().unwrap();
    assert_eq!(editor.active_buffer().path(), Some(expected.as_path()));
    assert_eq!(editor.active_buffer().name(), "lib.rs");
}

#[test]
fn test_layout_slots_save_and_restore() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    editor.layout.split(SplitType::Horizontal);
    editor.layout.split(SplitType::Vertical);
    assert_eq!(editor.layout.leaf_ids().len(), 3);

    editor.execute_with("save-window-layout", ted_core::Arg::parse("1"));

    editor.handle_key(ctrl('x'));
    editor.handle_key(plain('1'));
    assert_eq!(editor.layout.leaf_ids().len(), 1);

    editor.execute_with("restore-window-layout", ted_core::Arg::parse("1"));
    assert_eq!(editor.layout.leaf_ids().len(), 3, "restoring must bring back the saved layout");
}

#[test]
fn test_mouse_click_selects_window_and_scroll() {
    let mut editor = Editor::new(&[PathBuf::from("Cargo.toml")]);
    let first = editor.layout.active_id();
    let second = editor.layout.split(SplitType::Vertical);
    assert_ne!(first, second);

    let mut frame = Frame::new(1000.0, 800.0, ted_core::Color::BLACK);
    editor.render(&mut frame, Metrics::new(8.0, 16.0));

    editor.handle_mouse_press(100.0, 100.0, 1);
    assert_eq!(editor.layout.active_id(), first, "Clicking the left window focuses it");
    editor.handle_mouse_press(800.0, 100.0, 1);
    assert_eq!(editor.layout.active_id(), second, "Clicking the right window focuses it");

    editor.handle_scroll(800.0, 100.0, 5);
}
