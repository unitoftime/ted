use ted_core::frame::{DrawCmd, Frame};
use ted_core::fuzzy::{filter, filter_within, fuzzy_score, helm_score, MatchKeys};
use ted_core::key::{KeyCode, KeyEvent};
use ted_core::view::RenderCtx;
use ted_core::{Editor, Faces, Metrics, Modal, Picker, PickerItem, StartupOptions, Theme};

fn picker(ed: &Editor) -> &Picker {
    ed.modal::<Picker>().expect("picker is open")
}

fn type_keys(ed: &mut Editor, text: &str) {
    for ch in text.chars() {
        ed.handle_key(KeyEvent::plain_char(ch));
    }
}

#[test]
fn test_fuzzy_score() {
    let score1 = fuzzy_score("git", "pkg/git/git_view.go");
    let score2 = fuzzy_score("git", "pkg/view/text_view.go");
    assert!(score1.is_some());
    if let (Some(s1), Some(s2)) = (score1, score2) {
        assert!(s1 > s2, "Expected git_view ({}) > text_view ({})", s1, s2);
    }
}

#[test]
fn test_helm_multi_token_search() {
    assert!(helm_score("ted edit", "editor.rs", "/home/user/src/ted/crates/ted_core/src/editor.rs").is_some());
    assert!(helm_score("ted edit", "buffer.rs", "/home/user/src/ted/crates/ted_core/src/buffer.rs").is_none());

    let title_match = helm_score("editor", "editor.rs", "/home/repo/src/editor.rs").unwrap();
    let path_match = helm_score("editor", "other.rs", "/home/repo/editor/src/other.rs").unwrap();
    assert!(title_match > path_match, "Expected title match ({}) > path match ({})", title_match, path_match);
}

/// The picker narrows a growing query by rescanning only the previous matches; that must
/// give exactly what a full scan gives.
#[test]
fn test_narrowing_matches_full_scan() {
    let paths = [
        "src/editor/mod.rs",
        "src/editor/buffers.rs",
        "src/buffer/mod.rs",
        "src/ui/picker.rs",
        "tests/buffer_tests.rs",
        "README.md",
    ];
    let keys: Vec<MatchKeys> = paths.iter().map(|p| MatchKeys::new(p.rsplit('/').next().unwrap(), p)).collect();
    let query = "buf mod rs";
    let mut matches = filter("", &keys);
    for end in 1..=query.len() {
        let prefix = &query[..end];
        matches = filter_within(prefix, &keys, matches.into_iter());
        assert_eq!(matches, filter(prefix, &keys), "query {:?}", prefix);
    }
}

#[test]
fn test_picker_helm_filtering_and_navigation() {
    let mut editor = Editor::new(&[]);
    let items = vec![
        PickerItem::new("buffer.rs", "crates/ted_core/src/buffer.rs"),
        PickerItem::new("editor.rs", "crates/ted_core/src/editor.rs"),
        PickerItem::new("main.rs", "crates/ted_gui/src/main.rs"),
    ];
    editor.pick("test", "Recent Files", items, |_, _| {});
    assert_eq!(picker(&editor).filtered.len(), 3);
    assert_eq!(picker(&editor).selected, 0);

    // Multi-token query
    type_keys(&mut editor, "ted edit");
    assert_eq!(picker(&editor).filtered.len(), 1);
    assert_eq!(picker(&editor).selected_item().unwrap().title, "editor.rs");

    // Clear the query
    editor.handle_key(KeyEvent::ctrl('a'));
    editor.handle_key(KeyEvent::ctrl('k'));
    assert_eq!(picker(&editor).filtered.len(), 3);

    editor.handle_key(KeyEvent::ctrl('n'));
    assert_eq!(picker(&editor).selected, 1);
    editor.handle_key(KeyEvent::ctrl('p'));
    assert_eq!(picker(&editor).selected, 0);

    // Tab cycles, Backtab cycles back
    for expected in [1, 2, 0] {
        editor.handle_key(KeyEvent::plain(KeyCode::Tab));
        assert_eq!(picker(&editor).selected, expected);
    }
    editor.handle_key(KeyEvent::plain(KeyCode::Backtab));
    assert_eq!(picker(&editor).selected, 2);

    // C-v / M-v page and clamp
    editor.handle_key(KeyEvent::ctrl('v'));
    assert_eq!(picker(&editor).selected, 2);
    editor.handle_key(KeyEvent::alt('v'));
    assert_eq!(picker(&editor).selected, 0);
}

#[test]
fn test_editor_recentf_workflow() {
    let temp_dir = std::env::temp_dir().join(format!("ted_recentf_editor_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();

    let f1 = temp_dir.join("alpha.txt");
    let f2 = temp_dir.join("beta.txt");
    let f3 = temp_dir.join("gamma.txt");
    for f in [&f1, &f2, &f3] {
        std::fs::write(f, "content\n").unwrap();
    }

    let options = StartupOptions { recent_files: Some(temp_dir.join("recentf_db")), ..Default::default() };
    let mut editor = Editor::with_options(&[f1.clone(), f2.clone()], options);

    // Opened f1 then f2: most recent first
    assert_eq!(editor.recent_files.entries(), &[f2.clone(), f1.clone()]);

    editor.open_file(&f3).unwrap();
    assert_eq!(editor.recent_files.entries().len(), 3);
    assert_eq!(editor.recent_files.entries()[0], f3);

    editor.execute("recentf-open-files");
    assert_eq!(picker(&editor).id(), "recentf");

    type_keys(&mut editor, "alpha");
    assert_eq!(picker(&editor).filtered.len(), 1);

    editor.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert!(!editor.has_modal());
    assert_eq!(editor.active_buffer().path(), Some(f1.as_path()));
    assert_eq!(editor.recent_files.entries()[0], f1);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_command_palette_helper_popup() {
    let mut editor = Editor::new(&[]);
    let initial_theme = editor.settings.get(ted_core::settings::THEME).to_string();

    editor.execute("execute-extended-command");
    assert_eq!(picker(&editor).id(), "execute-extended-command");
    assert!(picker(&editor).items.len() >= 40);

    type_keys(&mut editor, "theme");
    assert_eq!(picker(&editor).selected_item().unwrap().title, "toggle-theme");

    editor.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert!(!editor.has_modal());
    assert_ne!(editor.settings.get(ted_core::settings::THEME), initial_theme);
}

#[test]
fn test_command_palette_keys_and_prefix_help() {
    let mut editor = Editor::new(&[]);

    // M-x opens the palette
    editor.handle_key(KeyEvent::alt('x'));
    assert_eq!(picker(&editor).id(), "execute-extended-command");
    type_keys(&mut editor, "scratch");
    editor.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert!(editor.active_buffer().is_scratch());

    // F1 opens it too; ESC closes
    editor.handle_key(KeyEvent::plain(KeyCode::F(1)));
    assert!(editor.modal::<Picker>().is_some());
    editor.handle_key(KeyEvent::plain(KeyCode::Escape));
    assert!(!editor.has_modal());

    // A pending prefix hints at help
    editor.handle_key(KeyEvent::ctrl('x'));
    assert!(editor.status.contains("C-x- (type ? or C-h for help)"));

    // '?' lists the C-x bindings, derived from the keymap
    editor.handle_key(KeyEvent::plain_char('?'));
    let help = picker(&editor);
    assert!(help.title.contains("C-x"));
    assert!(!help.items.is_empty());
    for item in &help.items {
        assert!(item.subtitle.contains("C-x"), "{:?} should show its C-x key", item);
    }

    // Filter for "2" (split-window-below) and run it
    editor.handle_key(KeyEvent::plain_char('2'));
    editor.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert!(!editor.has_modal());
    assert_eq!(editor.layout.leaf_ids().len(), 2);
}

#[test]
fn test_help_reflects_rebinding() {
    let mut editor = Editor::new(&[]);
    editor.bind("global", "C-x 9", "split-window-below").unwrap();
    editor.handle_key(KeyEvent::ctrl('x'));
    editor.handle_key(KeyEvent::plain_char('?'));
    let item = picker(&editor).items.iter().find(|i| i.title == "split-window-below").unwrap();
    assert!(item.subtitle.contains("C-x 9"), "help must show new bindings: {}", item.subtitle);
}

#[test]
fn test_modal_text_overflow_clipped_and_truncated() {
    let mut editor = Editor::new(&[]);
    editor.pick(
        "help",
        "Help Menu",
        vec![PickerItem::new(
            "a_very_long_command_name_that_should_not_overflow_beyond_modal",
            "This is an extremely long description of a command explaining in tremendous detail all of the functionality that could possibly exist in the universe",
        )],
        |_, _| {},
    );

    let faces = Faces::new(&Theme::default());
    let cx = RenderCtx { faces: &faces, metrics: Metrics::new(9.0, 20.0), cursor_w: 9.0 };
    let mut frame = Frame::new(1000.0, 800.0, ted_core::Color::BLACK);
    picker(&editor).render(&mut frame, &cx);

    let mut found_item = false;
    let mut truncated = false;
    for cmd in &frame.commands {
        if let DrawCmd::Text(span) = cmd {
            assert!(span.clip.is_some(), "All modal text must be clipped: {:?}", span.text);
            found_item |= span.text.contains("a_very_long_command");
            truncated |= span.text.ends_with('…');
        }
    }
    assert!(found_item);
    assert!(truncated, "Long rows must be truncated with an ellipsis");
}
