use std::path::PathBuf;

use ted_core::session::Session;
use ted_core::{Editor, SplitType};

/// A restart must bring back the same files, splits, active window, cursors, scratch text
/// and layout slots in the new process.
#[test]
fn test_session_round_trips_through_a_file() {
    let cargo = PathBuf::from("Cargo.toml").canonicalize().unwrap();
    let lib = PathBuf::from("src/lib.rs").canonicalize().unwrap();
    let mut editor = Editor::new(std::slice::from_ref(&cargo));
    let scratch = editor.ensure_scratch();
    editor.buffers[scratch].set_text("notes\\with\nlines\r\n");
    editor.active_view_mut().goto(5);
    editor.execute_with("save-window-layout", ted_core::Arg::parse("2"));
    editor.layout.split(SplitType::Vertical);
    let right = editor.layout.split(SplitType::Horizontal);
    editor.layout.set_active(right);
    editor.open_file(&lib).unwrap();
    editor.active_view_mut().goto(42);

    let path = std::env::temp_dir().join(format!("ted_session_test_{}", std::process::id()));
    Session::capture(&editor).write(&path).unwrap();
    let mut restored = Editor::new(&[]);
    ted_core::session::resume(&mut restored, &path);
    assert!(!path.exists(), "the handoff file is consumed");

    let shown: Vec<_> = restored
        .layout
        .views()
        .iter()
        .map(|v| (restored.buffers[v.buffer].path().map(PathBuf::from), v.cursor.pos))
        .collect();
    assert_eq!(shown, [(Some(cargo.clone()), 5), (Some(lib), 42), (Some(cargo), 5)]);
    assert_eq!(restored.layout.active().cursor.pos, 42, "the active window stays active");
    let scratch = restored.ensure_scratch();
    assert_eq!(restored.buffers[scratch].text().to_string(), "notes\\with\nlines\r\n");
    assert_eq!(restored.saved_layouts[&2].views().len(), 1);
}
