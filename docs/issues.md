
# Issues

## Features

- [ ] **Project awareness everywhere.** Most work happens inside a git repository, so file
  search, text search and other commands should default to the current project and rank
  its results first.
- [ ] **Modernize generated buffers.** Review directory listings, git and other generated
  buffers, and replace legacy Emacs conventions where a simpler interface works better.
- [ ] **Remote hosts over SSH.** Work on a machine over SSH as if it were local: files,
  directory listings, search, builds, git, terminals and language servers, all fast enough
  that it doesn't feel remote.
- [x] **More language server features.** Completion, hover, rename and formatting.
- [x] Package a few high-quality fonts inside the editor?
- [x] I need a way to ripgrep search relative to wherever I am. Lik C-c C-s searches the project, but a lot of times I want to like search downstream of where I currently am
- [x] When commiting, it'd be nice to show syntax highlighting on the commit message
- [x] My position is constantly lost in files it feels like, for example I'll have a dired buffer and I just want to go through all the documents to look for one thing, every time i close the doc the dired buffer has my cursor in the wrong place. the same thing happens when a buffer goes away and I open it again. my cursor goes back to the start

## Bugs

- [x] Bold and italic text in the terminal sometimes has uneven spacing, possibly from
  measuring with the wrong font. It may already be fixed; this needs confirming.

## Architecture

- [ ] Review the core abstractions and make sure new features build on them easily.
- [ ] Keep rebinding keys trivial.
- [ ] Keep writing plugins trivial.

## Performance

- [x] Very long lines are laid out in full on every frame.
- [x] External file changes are detected by polling modification times every 500ms. A file
  watcher would be event-driven.
- [x] Decorations shift in O(n) per edit and are queried linearly. An interval tree would
  scale to the thousands a language server can produce.
- [x] Language server documents sync as full text. Incremental sync needs an edit journal on
  `Buffer`.
- [x] The tags backend re-reads the project on every query. A per-file tag cache keyed by
  modification time would make large repositories instant.
- [x] `Project::files` re-lists the project on every call. A cached list refreshed in the
  background would open the file switcher fully populated.

## Smaller items

- [ ] Stopping a build (killing its process group) only works on Unix.
- [ ] Project search results could be exported to a location list for `next-error`.
