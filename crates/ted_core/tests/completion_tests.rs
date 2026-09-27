use std::path::PathBuf;

use ted_core::commands::files::{anchor_directory, complete_path};
use ted_core::{Editor, KeyCode, KeyEvent, Modal, Picker};

fn modal_id(ed: &Editor) -> Option<String> {
    ed.top_modal().map(|m| m.id().to_string())
}

fn prompt_text(ed: &Editor) -> String {
    ed.modal::<ted_core::Prompt>().expect("prompt").input.text().to_string()
}

fn set_prompt_text(ed: &mut Editor, text: &str) {
    ed.modal_mut::<ted_core::Prompt>().expect("prompt").input.set(text);
}

fn open_find_file(ed: &mut Editor) {
    ed.handle_key(KeyEvent::ctrl('x'));
    ed.handle_key(KeyEvent::ctrl('f'));
}

fn cwd_anchor() -> String {
    let cwd = std::env::current_dir().unwrap();
    let mut expected = ted_core::collapse_tilde(cwd.canonicalize().unwrap_or(cwd));
    if !expected.ends_with('/') {
        expected.push('/');
    }
    expected
}

#[test]
fn test_anchor_directory_default() {
    let editor = Editor::new(&[]);
    assert_eq!(anchor_directory(&editor.active_buffer().directory()), cwd_anchor());
}

#[test]
fn test_anchor_directory_from_open_file() {
    let editor = Editor::new(&[PathBuf::from("src/lib.rs")]);
    let cwd = std::env::current_dir().unwrap();
    let mut expected = ted_core::collapse_tilde(cwd.canonicalize().unwrap_or(cwd).join("src"));
    if !expected.ends_with('/') {
        expected.push('/');
    }
    assert_eq!(anchor_directory(&editor.active_buffer().directory()), expected);
}

#[test]
fn test_path_completion() {
    // Tests run in crates/ted_core
    let c = complete_path("Car");
    assert_eq!(c.text, "Cargo.toml");
    assert_eq!(c.candidates, vec!["Cargo.toml"]);

    let c = complete_path("src/li");
    assert_eq!(c.text, "src/lib.rs");
    assert_eq!(c.candidates, vec!["lib.rs"]);
}

#[test]
fn test_c_x_c_f_opens_absolute_path() {
    let mut editor = Editor::new(&[]);
    open_find_file(&mut editor);
    assert_eq!(modal_id(&editor).as_deref(), Some("find-file"));
    assert_eq!(prompt_text(&editor), cwd_anchor());
}

#[test]
fn test_collapse_tilde_shorter_paths() {
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = std::path::Path::new(&home);
        let sub = home_path.join("projects").join("ted");
        let sub_with_slash = format!("{}/", sub.to_string_lossy());

        assert_eq!(ted_core::collapse_tilde(&sub), "~/projects/ted");
        assert_eq!(ted_core::collapse_tilde(&sub_with_slash), "~/projects/ted/");
        assert_eq!(ted_core::collapse_tilde(home_path), "~");
        assert_eq!(ted_core::collapse_tilde(format!("{}/", home_path.to_string_lossy())), "~/");
        assert_eq!(ted_core::collapse_tilde(std::path::Path::new("/tmp/some_dir/")), "/tmp/some_dir/");
    }
}

#[test]
fn test_space_key_in_buffer() {
    let mut editor = Editor::new(&[]);
    editor.handle_key(KeyEvent::plain_char(' '));
    assert_eq!(editor.active_buffer().to_string(), " ");
}

#[test]
fn test_find_file_double_tab_does_not_leak_tabs_to_buffer() {
    let mut editor = Editor::new(&[]);
    open_find_file(&mut editor);
    assert_eq!(modal_id(&editor).as_deref(), Some("find-file"));

    // "src/" has many entries
    set_prompt_text(&mut editor, "src/");
    let tab = KeyEvent::plain(KeyCode::Tab);

    // First tab stages the listing
    editor.handle_key(tab);
    assert_eq!(modal_id(&editor).as_deref(), Some("find-file"));
    assert_eq!(editor.active_buffer().to_string(), "", "Tab must not reach the document");

    // Second tab lists completions
    editor.handle_key(tab);
    assert_eq!(editor.active_buffer().to_string(), "");
    let picker = editor.modal::<Picker>().expect("second tab opens completions");
    assert_eq!(picker.id(), "completions");
    assert_eq!(picker.selected, 0);

    // Third tab cycles the selection
    editor.handle_key(tab);
    let picker = editor.modal::<Picker>().unwrap();
    if picker.filtered().len() > 1 {
        assert_eq!(picker.selected, 1);
    }
    assert_eq!(editor.active_buffer().to_string(), "");
}

#[test]
fn test_find_file_single_match_completes_directly() {
    let mut editor = Editor::new(&[]);
    open_find_file(&mut editor);
    set_prompt_text(&mut editor, "src/li");
    editor.handle_key(KeyEvent::plain(KeyCode::Tab));

    assert_eq!(prompt_text(&editor), "src/lib.rs");
    assert_eq!(modal_id(&editor).as_deref(), Some("find-file"));
    assert!(editor.modal::<Picker>().is_none(), "A single match completes without a popup");
}

#[test]
fn test_find_file_enter_on_directory_opens_dired() {
    let mut editor = Editor::new(&[]);
    open_find_file(&mut editor);
    set_prompt_text(&mut editor, "src");
    editor.handle_key(KeyEvent::plain(KeyCode::Enter));

    // Lists src/ in a dired buffer instead of a completions popup
    assert!(!editor.has_modal());
    assert_eq!(editor.active_buffer().mode().name, "Dired");
    assert_eq!(editor.active_buffer().name(), "src/");
}
