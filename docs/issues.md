# Issues

## Features

- [ ] **Project awareness everywhere.** Most work happens inside a git repository, so file
  search, text search and other commands should default to the current project and rank
  its results first.
- [ ] **Modernize generated buffers.** Review directory listings, git and other generated
  buffers, and replace legacy Emacs conventions where a simpler interface works better.
- [ ] **Remote editing over SSH.** Open `/ssh:host:path` paths directly instead of running
  an editor inside the terminal.
- [x] **More language server features.** Completion, hover, rename and formatting.

## Bugs

- [ ] Bold and italic text in the terminal sometimes has uneven spacing, possibly from
  measuring with the wrong font. It may already be fixed; this needs confirming.

## Architecture

- [ ] Review the core abstractions and make sure new features build on them easily.
- [ ] Keep rebinding keys trivial.
- [ ] Keep writing plugins trivial.

## Performance

- [ ] Very long lines are laid out in full on every frame.
- [ ] External file changes are detected by polling modification times every 500ms. A file
  watcher would be event-driven.
- [ ] Decorations shift in O(n) per edit and are queried linearly. An interval tree would
  scale to the thousands a language server can produce.
- [ ] Language server documents sync as full text. Incremental sync needs an edit journal on
  `Buffer`.
- [ ] The tags backend re-reads the project on every query. A per-file tag cache keyed by
  modification time would make large repositories instant.
- [ ] `Project::files` re-lists the project on every call. A cached list refreshed in the
  background would open the file switcher fully populated.

## Smaller items

- [ ] Stopping a build (killing its process group) only works on Unix.
- [ ] Project search results could be exported to a location list for `next-error`.
