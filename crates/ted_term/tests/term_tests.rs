use std::time::{Duration, Instant};

use ted_core::frame::DrawCmd;
use ted_core::{Color, Editor, Frame, KeyCode, KeyEvent, Metrics, StartupOptions};
use ted_term::TermPlugin;

fn editor() -> Editor {
    let options = StartupOptions {
        plugins: vec![Box::new(TermPlugin::with_shell("/bin/bash", &["--norc", "--noprofile"]))],
        ..Default::default()
    };
    Editor::with_options(&[], options)
}

/// Text currently drawn for the terminal (the live screen in terminal mode).
fn screen(ed: &mut Editor) -> String {
    let mut frame = Frame::new(800.0, 600.0, Color::BLACK);
    ed.render(&mut frame, Metrics::new(8.0, 16.0));
    frame
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCmd::Text(span) => Some(span.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_until(ed: &mut Editor, what: &str, done: impl Fn(&mut Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(ed) {
        assert!(Instant::now() < deadline, "timed out waiting for {}:\n{}", what, screen(ed));
        ed.poll(Instant::now());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn type_line(ed: &mut Editor, line: &str) {
    for ch in line.chars() {
        ed.handle_key(KeyEvent::plain_char(ch));
    }
    ed.handle_key(KeyEvent::plain(KeyCode::Enter));
}

#[test]
fn terminal_runs_commands_and_switches_to_view_mode() {
    let mut ed = editor();
    ed.execute("term");
    assert_eq!(ed.active_buffer().name(), "terminal");
    assert_eq!(ed.active_buffer().mode().name, "Terminal");

    // Keys go to the shell; output shows up on the live screen
    type_line(&mut ed, "echo ted_$((20 + 22))");
    wait_until(&mut ed, "echo output", |ed| screen(ed).contains("ted_42"));

    // Editor C-x commands and M-x work directly from terminal mode
    ed.handle_key(KeyEvent::ctrl('x'));
    ed.handle_key(KeyEvent::plain_char('b'));
    assert_eq!(ed.top_modal().map(|m| m.id().to_string()).as_deref(), Some("switch-to-buffer"));
    ed.handle_key(KeyEvent::plain(KeyCode::Escape));
    ed.handle_key(KeyEvent::alt('x'));
    assert_eq!(ed.top_modal().map(|m| m.id().to_string()).as_deref(), Some("execute-extended-command"));
    ed.handle_key(KeyEvent::plain(KeyCode::Escape));
    assert_eq!(ed.active_buffer().name(), "terminal");

    // Keys bound in the terminal keymap (as init.rhai does) win over passing them through
    ed.bind("terminal", "M-]", "other-window").unwrap();
    ed.bind("terminal", "M-[", "previous-window").unwrap();
    ed.handle_key(KeyEvent::ctrl('x'));
    ed.handle_key(KeyEvent::plain_char('2'));
    let terminal_window = ed.layout.active_id();
    ed.handle_key(KeyEvent::alt(']'));
    assert_ne!(ed.layout.active_id(), terminal_window);
    ed.handle_key(KeyEvent::alt('['));
    assert_eq!(ed.layout.active_id(), terminal_window);
    ed.handle_key(KeyEvent::ctrl('x'));
    ed.handle_key(KeyEvent::plain_char('1'));

    // A C-x sequence the editor doesn't bind goes on to the program: bash's C-x C-v
    // prints its version.
    ed.handle_key(KeyEvent::ctrl('x'));
    ed.handle_key(KeyEvent::ctrl('v'));
    wait_until(&mut ed, "C-x C-v forwarded", |ed| screen(ed).contains("GNU bash"));
    ed.handle_key(KeyEvent::ctrl('u'));

    // C-] freezes it into an ordinary read-only buffer
    ed.handle_key(KeyEvent::ctrl(']'));
    assert_eq!(ed.active_buffer().mode().name, "Terminal View");
    assert!(ed.active_buffer().to_string().contains("ted_42"));
    ed.execute("search-backward");
    for ch in "ted_42".chars() {
        ed.handle_key(KeyEvent::plain_char(ch));
    }
    ed.handle_key(KeyEvent::plain(KeyCode::Enter));
    let (line, _) = ed.active_buffer().char_to_point(ed.active_view().cursor.pos);
    assert!(ed.active_buffer().line_content(line).to_string().starts_with("ted_42"));

    // q returns to the live terminal
    ed.handle_key(KeyEvent::plain_char('q'));
    assert_eq!(ed.active_buffer().mode().name, "Terminal");

    // C-s reaches the program instead of freezing output (flow control is off)
    ed.handle_key(KeyEvent::ctrl('s'));
    ed.handle_key(KeyEvent::ctrl('g'));
    type_line(&mut ed, "echo still_$((1 + 1))");
    wait_until(&mut ed, "output after C-s", |ed| screen(ed).contains("still_2"));

    // C-d exits the shell, closing the terminal and returning to the previous buffer
    ed.handle_key(KeyEvent::ctrl('d'));
    wait_until(&mut ed, "exit", |ed| ed.active_buffer().is_scratch());
    assert!(ed.buffers.find(|b| b.name() == "terminal").is_none());
    assert_eq!(ed.status, "terminal exited");
}
