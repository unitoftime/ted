# Architecture

## Crates

```
          ┌──> ted_git ──┐
ted_gui ──┼──> ted_lsp ──┼──> ted_core
          ├──> ted_term ─┘       ^
          └──────────────────────┘
```

- **`ted_core`**: the editor engine. Buffers, windows, commands, keymaps, modes, syntax,
  rendering to a display list. No windowing or platform code.
- **`ted_git`, `ted_lsp`, `ted_term`**: the git client, language server client and terminal.
  They are plugins built only on `ted_core`'s public API, which keeps that API honest.
- **`ted_gui`**: a thin frontend. It turns window events into keys and mouse input, and
  rasterizes the frames the core produces.

## Core model

| Concept | What it is |
|---|---|
| Buffer | Text (a rope), its file, mode, undo tree and syntax tree, and the `Place` the last view to leave it was at. Plain data, addressed by `BufferId`. |
| View | A window onto a buffer: cursor, scroll, wrapping. Showing a buffer starts at its place. Views are the leaves of the split `Layout`. |
| Doc | A view and its buffer borrowed together. Motion and editing primitives live here. |
| Command | Every user-facing action is a named command, whether built in or from a plugin. |
| Keymap | A trie of key sequences to commands. Keymaps have parents and stack in layers. |
| Mode | Per-language or per-buffer-kind data (grammar, indentation, comments) plus its own keymap. |
| Modal | Anything that takes over the keyboard for a moment: prompts, pickers, search, menus. |
| Setting / Face | Named, typed, documented values and named styles. Themes and `init.rhai` override them by name. |
| Job | Background work on a thread that sends results back to the UI thread as closures. |

Generated buffers (git status, directory listings, compilation output, search results) are
ordinary read-only buffers written from styled text. Their lines can map back to the items
they show, so the same navigation keys work in all of them.

## Input

A key is resolved against layers, from the top down: the active modal's keymap alone, or
else the buffer's minor keymaps, its mode's keymap, then the global keymap. The bound
command runs through `Editor::execute_command`. The previous command stays visible to the
next one, which is how consecutive kills merge and repeated keys cycle.

Help (`M-x`, `describe-key`, the key menu after a prefix) is generated from the live
keymaps and command registry, so it always matches the actual bindings.

## Rendering

The core lays out each view once per frame (tabs, wrapping, wide characters) and uses the
same layout for motion, hit-testing and drawing. Syntax highlighting, decorations, the
selection and search matches all become face spans over that layout. The result is a
`Frame`: rectangles and styled text in pixels. The GUI draws text on a monospace cell grid
from a cache of rasterized glyphs.

## Threading

The core is single-threaded. Slow work (project search, builds, git, language servers)
runs through `Editor::spawn` and reports back with `ctx.send(|ed| ...)`. The frontend
wakes up, runs the queued closures and redraws. Jobs can be cancelled; their late results
are dropped.

## Extending

- **Configuration** is `~/.config/ted/init.rhai`, a [Rhai](https://rhai.rs) script that
  sets settings, faces and mode options and binds keys. `M-x reload-init` reapplies it
  from the defaults.
- **Plugins** are Rust types implementing `plugin::Plugin`. They register commands, modes,
  settings, faces and hooks through the same registries the built-ins use. See the
  example at the top of `crates/ted_core/src/plugin.rs`.

The built-in modes (`modes/dired`, `modes/compilation`, `modes/markdown.rs`) are written
the same way as plugins.

Defaults hold what every user should get; personal choices belong in `init.rhai`. Defaults
never bind `C-c <key>` in the global keymap, because that space is left to the user.

## Backend chains

Features that need outside knowledge ask a chain of backends in priority order
(`ted_core::chain`): language servers first, then a built-in fallback. A backend that
declines or finds nothing hands the request on to the next, and answers the user has moved
on from are dropped.

- **Cross-references** (`xref`): definitions and references; the fallback is a
  tree-sitter tags index built from each grammar's tag queries.
- **Completion** (`completion`): candidates for the word at point, shown in a popup under
  it; the fallback is the words of the open buffers.
- **Formatting** (`format`): the edits that format a buffer or region, also run before
  saving with `format_on_save`. Saving waits for `before_save` hooks like this one, with a
  timeout.

Hover and rename are language server commands. Every multi-edit change (formatting,
rename, completion with an import) goes through `Editor::apply_edits`, one undo step per
buffer.
