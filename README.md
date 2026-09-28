# ted

## Motivation
I love emacs, and have used it for a very long time, but emacs has a few problems that really bother me: Its slow and bulky, lags on LSP, lisp is slow and hard to configure, bloated hotkeys. I don't say these things to take away from what emacs is, or what other people have created, but I decided it was time for me to move on. So I decided to vibe-code my own editor. Anyways, here it is.

## What is it?
A fast, Emacs-flavored text editor written in Rust. It keeps the Emacs keys and the
"everything is a command" model, drops the Lisp, and builds in the tools you would
otherwise assemble from packages.

## Features

- Emacs-style editing: kill ring, mark and region, undo tree, incremental search,
  `M-x` with fuzzy completion, and keyboard-driven window splits with saved layouts
- Tree-sitter syntax highlighting for Rust, Go, C, Python, JavaScript, TypeScript, HTML,
  CSS, JSON, TOML, YAML, Markdown, Make and shell, with syntax-aware indentation for most
- Language servers (definitions, references, diagnostics, completion, hover, rename and
  formatting), with tree-sitter tags and buffer words as fallbacks when no server is
  running
- Built-in git porcelain: status, staging, commits, log, blame, branches and stash
- Built-in terminal emulator
- Compilation buffers with clickable errors and `next-error`
- Directory editor, recent files, project-wide file and text search
- Help generated from the live keymaps: `describe-key`, `describe-bindings`, and a key
  menu after any prefix (press `?` or `C-h`)
- Configuration in [Rhai](https://rhai.rs), reloadable without restarting

## Building

ted builds with stable Rust. The GUI currently targets Linux (X11 or Wayland).

```sh
make            # release build, copied to ./bin/ted
make run        # build and launch
make test       # run the test suite
make install    # install to ~/.local (binary, icon, desktop entry)
```

For a system-wide install, run `sudo make install PREFIX=/usr/local`.

## Configuration

ted reads `~/.config/ted/init.rhai` (or `$XDG_CONFIG_HOME/ted/init.rhai`) at startup.
`M-x reload-init` reapplies it, first restoring the defaults, so deleting a line undoes it.

```rhai
set("theme", "tango-dark");
set("tab_width", 4);
set("completion.auto", "trigger");   // "off" (default), "trigger" (after `.`), "typing"
face("keyword", #{ fg: "#c586c0", bold: true });

bind("M-o", "other-window");
bind(["C-x j", "C-x C-j"], "switch-to-buffer");
bind("markdown", "C-c C-t", "toggle-theme");   // in a mode's (or any other) keymap
unbind("C-z");

// A mode's own properties, and settings that apply only to its buffers.
mode("markdown", #{ line_numbers: false });
mode("go", #{ format_on_save: true, "lsp.settings": #{ staticcheck: true } });
```

The global `C-c <key>` space is left to the user, as in Emacs. `describe-key` shows what a
key does and prints the `bind` line that would change it, and `describe-setting` lists
every setting, including plugin settings. Mistakes are reported with their line, and the
rest of the file still applies.

## Architecture

The editor is split into a frontend-independent engine (`ted_core`), plugins (`ted_git`,
`ted_lsp`, `ted_term`) built only on its public API, and a thin GUI frontend (`ted_gui`).
See [docs/architecture.md](docs/architecture.md) for the core model, key dispatch and the
plugin API.
