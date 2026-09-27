use std::fs;
use std::time::Duration;

use ted_core::{Arg, Buffer, Editor, KeyCode, KeyEvent, Modal, Modifiers, Picker};

fn type_text(ed: &mut Editor, text: &str) {
    for ch in text.chars() {
        ed.execute_with("self-insert-command", Arg::Char(ch));
    }
}

fn text(ed: &Editor) -> String {
    ed.active_buffer().to_string()
}

fn modal_id(ed: &Editor) -> Option<String> {
    ed.top_modal().map(|m| m.id().to_string())
}

#[test]
fn test_buffer_insert_delete_undo_redo() {
    let mut buf = Buffer::from_str("");
    assert_eq!(buf.len_chars(), 0);
    assert_eq!(buf.len_lines(), 1);

    buf.snapshot(0);
    buf.insert(0, "Hello World");
    assert_eq!(buf.slice_to_string(0..11), "Hello World");
    assert_eq!(buf.len_lines(), 1);

    buf.snapshot(5);
    buf.insert(5, " Beautiful");
    assert_eq!(buf.slice_to_string(0..21), "Hello Beautiful World");

    // Undo back to "Hello World"
    assert_eq!(buf.undo(21), Some(5));
    assert_eq!(buf.slice_to_string(0..11), "Hello World");

    // Redo back to "Hello Beautiful World"
    assert_eq!(buf.redo(5), Some(21));
    assert_eq!(buf.slice_to_string(0..21), "Hello Beautiful World");
}

#[test]
fn test_buffer_lines_and_points() {
    let buf = Buffer::from_str("alpha\nbeta\ngamma\ndelta");
    assert_eq!(buf.len_lines(), 4);
    assert_eq!(buf.char_to_point(7), (1, 1)); // "alpha\nb[e]ta"
    assert_eq!(buf.point_to_char(1, 1), 7);
}

#[test]
fn test_default_scratch_buffer_on_startup() {
    let editor = Editor::new(&[]);
    let buf = editor.active_buffer();
    assert!(buf.is_scratch());
    assert_eq!(buf.path(), None);
    assert_eq!(buf.name(), "*scratch*");
    assert_eq!(editor.buffers.len(), 1);
}

#[test]
fn test_scratch_buffer_always_exists_with_files_opened() {
    let temp_dir = std::env::temp_dir().join(format!("ted_scratch_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let f1 = temp_dir.join("hello.rs");
    fs::write(&f1, "fn main() {}\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&f1));
    assert_eq!(editor.active_buffer().path(), Some(f1.as_path()));
    assert!(editor.buffers.iter().any(|(_, b)| b.is_scratch()), "Scratch buffer must always be present");

    editor.execute("switch-to-scratch");
    assert!(editor.active_buffer().is_scratch());
    assert_eq!(editor.active_buffer().name(), "*scratch*");

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_scratch_buffer_switcher_and_keybindings() {
    let temp_dir = std::env::temp_dir().join(format!("ted_scratch_kb_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let f1 = temp_dir.join("test.txt");
    fs::write(&f1, "some text\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&f1));

    // Buffer switcher lists other buffers first
    editor.execute("switch-to-buffer");
    let picker = editor.modal::<Picker>().expect("switcher picker");
    assert_eq!(picker.id(), "switch-to-buffer");
    let top = &picker.items[picker.filtered()[0]];
    assert_eq!(top.title, "*scratch*");
    assert_eq!(top.subtitle, "scratch buffer");

    // Enter switches to *scratch*
    editor.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert!(!editor.has_modal());
    assert!(editor.active_buffer().is_scratch());

    // And back to test.txt
    editor.execute("switch-to-buffer");
    let picker = editor.modal::<Picker>().unwrap();
    assert_eq!(picker.items[picker.filtered()[0]].title, "test.txt");
    editor.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert_eq!(editor.active_buffer().path(), Some(f1.as_path()));

    editor.execute("switch-to-scratch");
    assert!(editor.active_buffer().is_scratch());

    editor.open_file(&f1).unwrap();
    assert_eq!(editor.active_buffer().path(), Some(f1.as_path()));

    // C-x * switches to scratch directly
    editor.handle_key(KeyEvent::ctrl('x'));
    editor.handle_key(KeyEvent::plain_char('*'));
    assert!(editor.active_buffer().is_scratch());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_scratch_buffer_save_as_spawns_new_scratch() {
    let temp_dir = std::env::temp_dir().join(format!("ted_scratch_save_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let mut editor = Editor::new(&[]);
    assert!(editor.active_buffer().is_scratch());
    type_text(&mut editor, "Hi");

    let save_path = temp_dir.join("saved_scratch.txt");
    editor.execute("save-buffer-as");
    for ch in save_path.to_string_lossy().chars() {
        editor.handle_key(KeyEvent::plain_char(ch));
    }
    editor.handle_key(KeyEvent::plain(KeyCode::Enter));

    // The former scratch buffer now visits save_path
    let buf = editor.active_buffer();
    assert_eq!(buf.path(), Some(save_path.as_path()));
    assert!(!buf.is_scratch());
    assert_eq!(buf.name(), "saved_scratch.txt");

    // A fresh scratch buffer still exists
    let scratch = editor.ensure_scratch();
    assert!(editor.buffers[scratch].is_scratch());
    assert_eq!(editor.buffers[scratch].path(), None);

    editor.execute("switch-to-scratch");
    assert!(editor.active_buffer().is_scratch());
    assert_eq!(editor.active_buffer().name(), "*scratch*");

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_undo_tree_branching_and_no_history_loss() {
    let mut buf = Buffer::from_str("A");

    buf.snapshot(1);
    buf.insert(1, "B");
    buf.snapshot(2);
    buf.insert(2, "C");
    assert_eq!(buf.to_string(), "ABC");

    // Undo back to "AB"
    assert_eq!(buf.undo(3), Some(2));
    assert_eq!(buf.to_string(), "AB");

    // Branch off from "AB" with "X"
    buf.snapshot(2);
    buf.insert(2, "X");
    assert_eq!(buf.to_string(), "ABX");

    // "ABC" is still reachable: undo X, then undo the undo.
    assert_eq!(buf.undo(3), Some(2));
    assert_eq!(buf.to_string(), "AB");
    assert_eq!(buf.undo(2), Some(3));
    assert_eq!(buf.to_string(), "ABC");
}

#[test]
fn test_undo_tree_keyboard_up_and_down() {
    let mut editor = Editor::new(&[]);
    type_text(&mut editor, "Hello World");
    assert_eq!(text(&editor), "Hello World");

    // Undo via C-/ and C-_
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "Hello ");
    editor.handle_key(KeyEvent::ctrl('_'));
    assert_eq!(text(&editor), "");

    // Redo via C-? (Ctrl+Shift+/)
    editor.handle_key(KeyEvent::new(KeyCode::Char('?'), Modifiers { ctrl: true, alt: false, shift: true }));
    assert_eq!(text(&editor), "Hello ");

    // Redo via M-_ (Alt+Shift+-)
    editor.handle_key(KeyEvent::new(KeyCode::Char('_'), Modifiers { ctrl: false, alt: true, shift: true }));
    assert_eq!(text(&editor), "Hello World");
}

#[test]
fn test_undo_tree_visualizer_c_x_u() {
    let mut editor = Editor::new(&[]);
    type_text(&mut editor, "A B C");
    assert_eq!(text(&editor), "A B C");

    editor.handle_key(KeyEvent::ctrl('x'));
    editor.handle_key(KeyEvent::plain_char('u'));
    assert_eq!(modal_id(&editor).as_deref(), Some("undo-tree"), "C-x u must open the visualizer");

    editor.handle_key(KeyEvent::plain(KeyCode::Up));
    assert_eq!(text(&editor), "A B ");
    editor.handle_key(KeyEvent::plain_char('p'));
    assert_eq!(text(&editor), "A ");

    editor.handle_key(KeyEvent::plain(KeyCode::Down));
    assert_eq!(text(&editor), "A B ");
    editor.handle_key(KeyEvent::plain_char('n'));
    assert_eq!(text(&editor), "A B C");

    editor.handle_key(KeyEvent::plain_char('q'));
    assert!(!editor.has_modal(), "'q' must close the visualizer");
    assert_eq!(text(&editor), "A B C");
}

#[test]
fn test_emacs_undo_the_undos() {
    let mut editor = Editor::new(&[]);
    type_text(&mut editor, "A B C");

    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "A B ");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "A ");

    type_text(&mut editor, "X");
    assert_eq!(text(&editor), "A X");

    // Undo X, then undo the earlier undos
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "A ");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "A B ");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "A B C");
}

#[test]
fn test_typing_and_backspace_batching() {
    let mut editor = Editor::new(&[]);
    type_text(&mut editor, "Hello World");

    // Each word undoes in a single step
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "Hello ");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "");

    editor.execute("redo");
    assert_eq!(text(&editor), "Hello ");
    editor.execute("redo");
    assert_eq!(text(&editor), "Hello World");

    // Three backspaces restore in one undo
    for _ in 0..3 {
        editor.handle_key(KeyEvent::plain(KeyCode::Backspace));
    }
    assert_eq!(text(&editor), "Hello Wo");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "Hello World");
}

#[test]
fn test_emacs_c_g_redo() {
    let mut editor = Editor::new(&[]);
    type_text(&mut editor, "first second");

    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "first ");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "");

    // C-g breaks the undo chain, so undo now reverses direction
    editor.handle_key(KeyEvent::ctrl('g'));
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "first ");
    editor.handle_key(KeyEvent::ctrl('/'));
    assert_eq!(text(&editor), "first second");
}

#[test]
fn test_external_edit_autoreload_clean_buffer() {
    let dir = std::env::temp_dir().join(format!("ted_test_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let file_path = dir.join("clean_autoreload.txt");
    fs::write(&file_path, "Original clean text\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&file_path));
    assert_eq!(text(&editor), "Original clean text\n");
    assert!(!editor.active_buffer().is_dirty());

    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file_path, "Updated text from disk!\n").unwrap();

    assert!(editor.check_external_changes(), "must detect and reload a modified clean buffer");
    assert_eq!(text(&editor), "Updated text from disk!\n");
    assert!(!editor.active_buffer().is_dirty());
    assert!(editor.status.contains("Auto-reloaded"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_external_edit_conflict_prompt_and_reload_choice() {
    let dir = std::env::temp_dir().join(format!("ted_test_conflict_r_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let file_path = dir.join("conflict_r.txt");
    fs::write(&file_path, "Initial disk text\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&file_path));
    editor.active_buffer_mut().insert(0, "Local edit: ");
    assert!(editor.active_buffer().is_dirty());

    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file_path, "External disk update!\n").unwrap();

    assert!(editor.check_external_changes());
    assert_eq!(modal_id(&editor).as_deref(), Some("resolve-conflict"));

    // 'r' reloads the disk version, discarding local edits
    editor.handle_key(KeyEvent::plain_char('r'));
    assert!(!editor.has_modal());
    assert_eq!(text(&editor), "External disk update!\n");
    assert!(!editor.active_buffer().is_dirty());
    assert!(editor.status.contains("Reloaded"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_external_edit_conflict_prompt_and_keep_choice() {
    let dir = std::env::temp_dir().join(format!("ted_test_conflict_k_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let file_path = dir.join("conflict_k.txt");
    fs::write(&file_path, "Initial disk text\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&file_path));
    editor.active_buffer_mut().insert(0, "Local edit: ");

    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file_path, "External disk update!\n").unwrap();

    assert!(editor.check_external_changes());
    assert!(editor.has_modal());

    // 'k' keeps local edits
    editor.handle_key(KeyEvent::plain_char('k'));
    assert!(!editor.has_modal());
    assert!(text(&editor).starts_with("Local edit: "));
    assert!(editor.active_buffer().is_dirty());
    assert!(editor.status.contains("Kept local edits"));

    // No repeated prompt for the same disk version
    assert!(!editor.check_external_changes());
    assert!(!editor.has_modal());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_save_external_edit_conflict_protection() {
    let dir = std::env::temp_dir().join(format!("ted_test_save_protect_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let file_path = dir.join("save_protect.txt");
    fs::write(&file_path, "Base content\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&file_path));
    editor.active_buffer_mut().insert(0, "My buffer changes\n");

    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file_path, "External overwrite content\n").unwrap();

    editor.execute("save-buffer");
    assert_eq!(modal_id(&editor).as_deref(), Some("overwrite-disk"));

    // 'n' cancels; disk untouched
    editor.handle_key(KeyEvent::plain_char('n'));
    assert!(!editor.has_modal());
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "External overwrite content\n");

    // 'y' overwrites
    editor.execute("save-buffer");
    assert!(editor.has_modal());
    editor.handle_key(KeyEvent::plain_char('y'));
    assert!(!editor.has_modal());
    assert!(fs::read_to_string(&file_path).unwrap().starts_with("My buffer changes\n"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_undo_to_oldest_change_clears_dirty_status() {
    let mut editor = Editor::new(&[]);
    assert!(!editor.active_buffer().is_dirty(), "Initial buffer should be clean");

    type_text(&mut editor, "Hi");
    assert!(editor.active_buffer().is_dirty());

    editor.execute("undo");
    assert_eq!(text(&editor), "");
    assert!(!editor.active_buffer().is_dirty(), "Undoing back to the initial state must clear dirty");

    editor.execute("redo");
    assert_eq!(text(&editor), "Hi");
    assert!(editor.active_buffer().is_dirty(), "Redo must re-mark the buffer dirty");
}

#[test]
fn test_undo_to_save_point_clears_dirty_status() {
    let dir = std::env::temp_dir().join(format!("ted_test_undo_save_{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let file_path = dir.join("test_save_undo.txt");
    fs::write(&file_path, "Initial file\n").unwrap();

    let mut editor = Editor::new(std::slice::from_ref(&file_path));
    assert!(!editor.active_buffer().is_dirty());

    type_text(&mut editor, "A");
    assert!(editor.active_buffer().is_dirty());

    editor.execute("save-buffer");
    assert!(!editor.active_buffer().is_dirty(), "Saved buffer should be clean");

    type_text(&mut editor, "B");
    assert!(editor.active_buffer().is_dirty());

    editor.execute("undo");
    assert_eq!(text(&editor), "AInitial file\n");
    assert!(!editor.active_buffer().is_dirty(), "Undoing back to the save point must clear dirty");

    editor.execute("undo");
    assert_eq!(text(&editor), "Initial file\n");
    assert!(editor.active_buffer().is_dirty(), "Undoing past the save point is dirty");

    editor.execute("redo");
    assert_eq!(text(&editor), "AInitial file\n");
    assert!(!editor.active_buffer().is_dirty(), "Redoing back to the save point must clear dirty");

    let _ = fs::remove_dir_all(&dir);
}
