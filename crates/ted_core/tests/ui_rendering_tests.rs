use std::path::Path;

use ted_core::frame::DrawCmd;
use ted_core::{Buffer, Color, Editor, FaceId, Frame, KeyCode, KeyEvent, Metrics, Modifiers, PickerItem, SplitType};

/// An editor showing a fresh buffer with `text` (and `path`, which selects the mode).
fn editor_with(text: &str, path: Option<&str>) -> Editor {
    let mut ed = Editor::new(&[]);
    let mut buf = Buffer::from_str(text);
    if let Some(path) = path {
        buf.set_path(path);
        buf.set_mode(ed.modes.for_path(Some(Path::new(path))));
    }
    let id = ed.add_buffer(buf);
    ed.show_buffer(id);
    ed
}

fn text_spans(frame: &Frame) -> impl Iterator<Item = &ted_core::frame::TextSpan> {
    frame.commands.iter().filter_map(|cmd| match cmd {
        DrawCmd::Text(span) => Some(span),
        _ => None,
    })
}

#[test]
fn test_delete_word_backward_all_buffers() {
    let _clip_lock = ted_core::kill_ring::TEST_CLIPBOARD_LOCK.lock();

    // 1. Buffer
    let mut editor = editor_with("one two three", None);
    editor.doc().set_cursor(13);
    editor.handle_key(KeyEvent::new(KeyCode::Backspace, Modifiers::CTRL));
    assert_eq!(editor.active_buffer().to_string(), "one two ");
    assert_eq!(editor.active_view().cursor.pos, 8);
    editor.handle_key(KeyEvent::new(KeyCode::Backspace, Modifiers::ALT));
    assert_eq!(editor.active_buffer().to_string(), "one ");
    assert_eq!(editor.active_view().cursor.pos, 4);
    // Consecutive backward kills merge into one kill-ring entry, in text order
    assert_eq!(editor.kill_ring.current(), Some("two three"));

    // 2. Minibuffer
    editor.execute("find-file");
    editor.modal_mut::<ted_core::Prompt>().unwrap().input.set("src/core/main.rs");
    editor.handle_key(KeyEvent::new(KeyCode::Backspace, Modifiers::CTRL));
    assert_eq!(editor.input().unwrap().text(), "src/core/main.");
    editor.handle_key(KeyEvent::new(KeyCode::Backspace, Modifiers::ALT));
    assert_eq!(editor.input().unwrap().text(), "src/core/");
    editor.handle_key(KeyEvent::ctrl('w'));
    assert_eq!(editor.input().unwrap().text(), "src/");
    editor.handle_key(KeyEvent::plain(KeyCode::Escape));

    // 3. Picker query
    editor.execute("recentf-open-files");
    for ch in "foo bar".chars() {
        editor.handle_key(KeyEvent::plain_char(ch));
    }
    assert_eq!(editor.input().unwrap().text(), "foo bar");
    editor.handle_key(KeyEvent::new(KeyCode::Backspace, Modifiers::CTRL));
    assert_eq!(editor.input().unwrap().text(), "foo ");
    editor.handle_key(KeyEvent::new(KeyCode::Backspace, Modifiers::ALT));
    assert_eq!(editor.input().unwrap().text(), "");
}

#[test]
fn test_minibuffer_cursor_placement() {
    let mut editor = Editor::new(&[]);
    let metrics = Metrics::new(9.0, 22.0);
    let mut frame = Frame::new(800.0, 600.0, Color::BLACK);

    editor.render(&mut frame, metrics);
    let view_cursor = frame.cursor.clone().expect("view cursor");
    assert_eq!(view_cursor.w, 9.0, "Box cursor is one cell wide by default");
    assert_eq!(editor.settings.get(ted_core::settings::CURSOR_SHAPE), "box");
    assert!(view_cursor.y < 500.0);

    editor.execute("find-file");
    frame.clear(800.0, 600.0, Color::BLACK);
    editor.render(&mut frame, metrics);
    let mb_cursor = frame.cursor.expect("minibuffer cursor");
    assert!(mb_cursor.y >= 600.0 - 26.0, "Cursor y ({}) must be in the minibuffer", mb_cursor.y);
    assert_eq!(mb_cursor.color, editor.faces.bg(FaceId::MINIBUFFER_CURSOR));
}

#[test]
fn test_overflow_clipping_and_wrapping() {
    let mut editor = editor_with(
        "This is an extremely long line of text that exceeds the bounds of a 400px panel and would otherwise bleed over into another window without clipping.\nShort line\n",
        None,
    );
    let metrics = Metrics::new(9.0, 22.0);
    assert!(editor.active_view().wrap, "Word wrap is on by default");

    // Unwrapped: long lines are clipped to the content area
    editor.active_view_mut().wrap = false;
    let mut frame = Frame::new(400.0, 300.0, Color::BLACK);
    editor.render(&mut frame, metrics);
    let span = text_spans(&frame).find(|s| s.text.contains("This is an extremely")).expect("long line drawn");
    let clip = span.clip.expect("clip rect");
    assert!(clip.w <= 400.0);
    assert_eq!(clip.x, span.x, "Clip starts where the text starts (after the gutter)");

    // Wrapped: the modeline shows [Wrap]
    editor.execute("toggle-word-wrap");
    assert!(editor.active_view().wrap);
    frame.clear(400.0, 300.0, Color::BLACK);
    editor.render(&mut frame, metrics);
    assert!(text_spans(&frame).any(|s| s.text.contains("[Wrap]")));
}

#[test]
fn test_overlay_z_ordering_and_background() {
    let mut editor = editor_with("Background text line 1\nBackground text line 2\nBackground text line 3\n", None);
    editor.pick(
        "test",
        "Completions",
        vec![PickerItem::new("alpha.rs", "File"), PickerItem::new("beta.rs", "File")],
        |_, _| {},
    );

    let mut frame = Frame::new(800.0, 600.0, Color::BLACK);
    editor.render(&mut frame, Metrics::new(9.0, 22.0));

    let last_text = frame
        .commands
        .iter()
        .rposition(|cmd| matches!(cmd, DrawCmd::Text(s) if s.text.contains("Background text")))
        .expect("buffer text drawn");
    let overlay_bg = frame
        .commands
        .iter()
        .position(|cmd| matches!(cmd, DrawCmd::Rect { rect, color } if *color == editor.faces.bg(FaceId::POPUP) && rect.w > 300.0))
        .expect("overlay background drawn");
    assert!(overlay_bg > last_text, "The popup must be painted over the buffer text");
}

#[test]
fn test_makefile_tab_rendering_and_cursor_alignment() {
    let mut editor = editor_with("all: build\n\tcargo build\n", Some("Makefile"));
    assert_eq!(editor.active_buffer().tab_width(), 8);
    // Line 1 starts at 11 with '\t'; 'c' is 12
    editor.doc().set_cursor(12);

    let (char_w, line_h) = (9.0, 22.0);
    let mut frame = Frame::new(800.0, 600.0, Color::BLACK);
    editor.render(&mut frame, Metrics::new(char_w, line_h));

    let cargo = text_spans(&frame).find(|s| s.text.contains("cargo build")).expect("cargo line drawn");
    assert!(cargo.text.starts_with("        cargo build"), "Tabs expand to the tab stop");

    let gutter_w = editor.doc().gutter_cols() as f32 * char_w;
    let expected_x = gutter_w + 8.0 * char_w;
    let cursor = frame.cursor.expect("cursor");
    assert_eq!(cursor.x, expected_x, "Cursor sits at visual column 8");
    assert_eq!(cursor.y, line_h, "Cursor is on row 1");

    // Click after the tab lands on 'c'; inside the tab lands on the tab
    editor.handle_mouse_press(expected_x + 2.0, line_h + 5.0, 1);
    assert_eq!(editor.active_view().cursor.pos, 12);
    editor.handle_mouse_press(gutter_w + 3.0 * char_w, line_h + 5.0, 1);
    assert_eq!(editor.active_view().cursor.pos, 11);
}

#[test]
fn test_gutter_simplified_and_thinner() {
    let mut editor = editor_with("one\ntwo\nthree\n", None);
    assert!(editor.active_view().line_numbers);
    // 4 lines -> 2 digits minimum + 1 separator column
    assert_eq!(editor.doc().gutter_cols(), 3);

    let mut frame = Frame::new(400.0, 300.0, Color::BLACK);
    editor.render(&mut frame, Metrics::new(8.0, 20.0));
    assert!(text_spans(&frame).any(|s| s.text == " 1 " || s.text == " 2 " || s.text == " 3 "));
    assert!(!text_spans(&frame).any(|s| s.text.contains('│') || s.text.contains('|')), "No gutter divider");
}

#[test]
fn test_unfocused_window_shows_unfilled_cursor_box() {
    let mut editor = editor_with("line 1\nline 2\n", None);
    editor.doc().set_cursor(3);
    let metrics = Metrics::new(8.0, 20.0);
    let cursor_color = editor.faces.bg(FaceId::CURSOR);
    let outline_rects = |frame: &Frame| {
        frame.commands.iter().filter(|cmd| matches!(cmd, DrawCmd::Rect { color, .. } if *color == cursor_color)).count()
    };

    let mut frame = Frame::new(400.0, 300.0, Color::BLACK);
    editor.render(&mut frame, metrics);
    assert!(frame.cursor.is_some(), "The focused view owns the primary cursor");
    assert_eq!(outline_rects(&frame), 0);

    // After a split, the unfocused view draws a hollow box (4 edges)
    editor.layout.split(SplitType::Vertical);
    let mut frame = Frame::new(400.0, 300.0, Color::BLACK);
    editor.render(&mut frame, metrics);
    assert!(frame.cursor.is_some());
    assert_eq!(outline_rects(&frame), 4, "Unfocused cursor draws 4 outline edges");
}

/// The wheel scrolls the view away from the cursor; the view follows the cursor again
/// once it moves.
#[test]
fn test_wheel_scroll_leaves_the_cursor_until_it_moves() {
    let text: String = (0..200).map(|i| format!("line {}\n", i)).collect();
    let mut editor = editor_with(&text, None);
    let mut frame = Frame::new(800.0, 400.0, Color::BLACK);
    editor.render(&mut frame, Metrics::new(8.0, 20.0));
    let (x, y) = (100.0, 100.0);

    for expected_top in [3, 6, 9] {
        editor.handle_scroll(x, y, 3);
        editor.render(&mut frame, Metrics::new(8.0, 20.0));
        assert_eq!(editor.active_view().top_line, expected_top);
    }
    assert_eq!(editor.active_view().cursor.pos, 0);

    editor.handle_key(KeyEvent::ctrl('n'));
    editor.render(&mut frame, Metrics::new(8.0, 20.0));
    assert_eq!(editor.active_view().top_line, 1);
}
