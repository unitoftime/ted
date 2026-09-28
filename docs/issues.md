
# Issues

## Features

- [x] **Project awareness everywhere.** Most work happens inside a git repository, so file
  search, text search and other commands should default to the current project and rank
  its results first.
- [ ] **Modernize generated buffers.** Review directory listings, git and other generated
  buffers, and replace legacy Emacs conventions where a simpler interface works better.
- [ ] **Remote hosts over SSH.** Work on a machine over SSH as if it were local: files,
  directory listings, search, builds, git, terminals and language servers, all fast enough
  that it doesn't feel remote.
- [x] Workspaces, I'm not sure how to conceptualize this, but it'd be nice to be able to like switch between workspaces, and persist workspaces. because mostly when I open ted i want to like get to a workspace, and also I have several ted instances each like "setup" for a workspace, but instead it'd be nice to just have one instance and swap my workspace directly inside ted. Lets writeup a plan first


## Smaller items

- [ ] Stopping a build (killing its process group) only works on Unix.
- [ ] Project search results could be exported to a location list for `next-error`.
