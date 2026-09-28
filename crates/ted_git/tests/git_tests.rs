use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ted_core::{Arg, Editor, KeyCode, KeyEvent, StartupOptions};
use ted_git::GitPlugin;

fn sh_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn temp_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ted_git_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
        &["config", "commit.gpgsign", "false"],
    ] {
        sh_git(&dir, args);
    }
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    sh_git(&dir, &["add", "a.txt"]);
    sh_git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

fn editor_in(repo: &Path) -> Editor {
    let options = StartupOptions { plugins: vec![Box::new(GitPlugin)], ..Default::default() };
    Editor::with_options(&[repo.join("a.txt")], options)
}

fn text(ed: &Editor) -> String {
    ed.active_buffer().to_string()
}

fn wait_until(ed: &mut Editor, what: &str, done: impl Fn(&Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(ed) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}; buffer:\n{}\nstatus: {}",
            what,
            text(ed),
            ed.status
        );
        ed.poll(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Puts point on the first line containing `needle`.
fn goto_line(ed: &mut Editor, needle: &str) {
    let buf = ed.active_buffer();
    let line = (0..buf.len_lines())
        .find(|&l| buf.line_content(l).to_string().contains(needle))
        .unwrap_or_else(|| panic!("no line containing {:?} in:\n{}", needle, text(ed)));
    let pos = buf.line_to_char(line);
    ed.doc().set_cursor(pos);
}

fn key(ed: &mut Editor, ch: char) {
    ed.handle_key(KeyEvent::plain_char(ch));
}

#[test]
fn status_stage_unstage_commit_and_discard() {
    let repo = temp_repo("flow");
    std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();

    let mut ed = editor_in(&repo);
    ed.handle_key(KeyEvent::ctrl('x'));
    ed.handle_key(KeyEvent::plain_char('g'));
    wait_until(&mut ed, "status", |ed| text(ed).contains("Unstaged changes (1)"));
    assert!(text(&ed).contains("Head:     main  init"));
    assert!(text(&ed).contains("Untracked files (1)"));
    assert!(text(&ed).contains("new.txt"));

    // Stage the modified file
    goto_line(&mut ed, "modified   a.txt");
    key(&mut ed, 's');
    wait_until(&mut ed, "staging", |ed| text(ed).contains("Staged changes (1)") && !text(ed).contains("Unstaged"));
    assert!(text(&ed).contains("modified   a.txt"), "point stays with the file as it moves sections");

    // Expand it and unstage just the hunk
    goto_line(&mut ed, "modified   a.txt");
    ed.handle_key(KeyEvent::plain(KeyCode::Tab));
    assert!(text(&ed).contains("+TWO") && text(&ed).contains("-two"));
    goto_line(&mut ed, "@@");
    key(&mut ed, 'u');
    wait_until(&mut ed, "hunk unstage", |ed| text(ed).contains("Unstaged changes (1)") && !text(ed).contains("Staged"));

    // Stage the hunk back, then commit via the message buffer
    goto_line(&mut ed, "modified   a.txt");
    ed.handle_key(KeyEvent::plain(KeyCode::Tab));
    goto_line(&mut ed, "@@");
    key(&mut ed, 's');
    wait_until(&mut ed, "hunk stage", |ed| text(ed).contains("Staged changes (1)"));
    key(&mut ed, 'c');
    key(&mut ed, 'c');
    assert_eq!(ed.active_buffer().name(), "COMMIT_EDITMSG");
    for ch in "Capitalize two".chars() {
        ed.execute_with("self-insert-command", Arg::Char(ch));
    }
    ed.handle_key(KeyEvent::ctrl('c'));
    ed.handle_key(KeyEvent::ctrl('c'));
    wait_until(&mut ed, "commit", |ed| text(ed).contains("Head:     main  Capitalize two"));
    assert!(ed.active_buffer().name().starts_with("git status: "), "returns to the status buffer");
    assert_eq!(sh_git(&repo, &["log", "-1", "--format=%s"]).trim(), "Capitalize two");
    assert!(!text(&ed).contains("Staged changes"));

    // Discard the untracked file
    goto_line(&mut ed, "new.txt");
    key(&mut ed, 'k');
    key(&mut ed, 'y');
    wait_until(&mut ed, "discard", |ed| !text(ed).contains("new.txt"));
    assert!(!repo.join("new.txt").exists());

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn diff_menu_and_help_popup() {
    let repo = temp_repo("diff");
    std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    let mut ed = editor_in(&repo);
    ed.execute("git-status");
    wait_until(&mut ed, "status", |ed| text(ed).contains("Unstaged changes (1)"));

    // d u: unstaged diff; RET on a changed line visits it in the file
    key(&mut ed, 'd');
    key(&mut ed, 'u');
    wait_until(&mut ed, "unstaged diff", |ed| text(ed).contains("+TWO"));
    assert!(ed.active_buffer().name().starts_with("git diff: "));
    goto_line(&mut ed, "+TWO");
    ed.handle_key(KeyEvent::plain(KeyCode::Enter));
    assert_eq!(ed.active_buffer().name(), "a.txt");
    assert_eq!(ed.active_buffer().char_to_point(ed.active_view().cursor.pos).0, 1);

    // d s: nothing staged yet
    ed.execute("git-status");
    wait_until(&mut ed, "status", |ed| text(ed).contains("Unstaged changes (1)"));
    key(&mut ed, 'd');
    key(&mut ed, 's');
    wait_until(&mut ed, "staged diff", |ed| text(ed).contains("No changes"));

    // h lists commands from the keymap, and its keys run them
    key(&mut ed, 'h');
    assert_eq!(ed.top_modal().map(|m| m.id().to_string()).as_deref(), Some("mode-help"));
    key(&mut ed, 'd');
    assert_eq!(ed.top_modal().map(|m| m.id().to_string()).as_deref(), Some("git-diff"));

    let _ = std::fs::remove_dir_all(&repo);
}
