
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

## Architecture

- [ ] Review the core abstractions and make sure new features build on them easily.
- [ ] Keep rebinding keys trivial.
- [ ] Keep writing plugins trivial.


## Smaller items

- [ ] Stopping a build (killing its process group) only works on Unix.
- [ ] Project search results could be exported to a location list for `next-error`.
