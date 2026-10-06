//! Linux: one inotify instance, read by a thread that sends each burst of events to the UI
//! thread as a single batch. An eventfd tells the thread to stop when the watcher drops.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use super::{Change, Changes, Event, OnChange};
use crate::jobs::JobContext;

/// A file in the directory was written, created, renamed into place or touched, a
/// directory was added to it, or either was deleted or renamed away.
const EVENTS: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_CREATE
    | libc::IN_ATTRIB
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_ONLYDIR;

const HEADER: usize = std::mem::size_of::<libc::inotify_event>();

type Dirs = Arc<Mutex<HashMap<i32, PathBuf>>>;

pub struct Inotify {
    fd: Arc<OwnedFd>,
    stop: Arc<OwnedFd>,
    /// Watched directory per watch descriptor, to name the files events are about.
    dirs: Dirs,
}

impl Inotify {
    pub fn start(ctx: JobContext, on_change: OnChange) -> Option<Self> {
        let fd = Arc::new(owned(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?);
        let stop = Arc::new(owned(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) })?);
        let dirs = Dirs::default();
        let (thread_fd, thread_stop, thread_dirs) = (fd.clone(), stop.clone(), dirs.clone());
        std::thread::Builder::new()
            .name("watch".into())
            .spawn(move || read_events(&thread_fd, &thread_stop, &thread_dirs, &ctx, on_change))
            .ok()?;
        Some(Self { fd, stop, dirs })
    }

    /// Watches `dir`, returning its watch descriptor.
    pub fn add(&self, dir: &Path) -> Option<i32> {
        let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
        let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), EVENTS) };
        if wd < 0 {
            return None;
        }
        self.dirs.lock().insert(wd, dir.to_path_buf());
        Some(wd)
    }

    pub fn remove(&self, wd: i32) {
        self.dirs.lock().remove(&wd);
        unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) };
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        let one = 1u64.to_ne_bytes();
        unsafe { libc::write(self.stop.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }
}

fn owned(fd: i32) -> Option<OwnedFd> {
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Waits for events until told to stop or the editor is gone.
fn read_events(fd: &OwnedFd, stop: &OwnedFd, dirs: &Dirs, ctx: &JobContext, on_change: OnChange) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut fds = [fd, stop].map(|f| libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            return;
        }

        // Drain everything queued so a burst (a build, a checkout) arrives as one batch.
        let (mut changes, mut overflowed) = (Vec::new(), false);
        loop {
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            overflowed |= parse(&buf[..n as usize], &dirs.lock(), &mut changes);
        }
        let changes = if overflowed {
            Changes::Unknown
        } else if changes.is_empty() {
            continue;
        } else {
            // In the order they happened within a path, whose last event is the one kept.
            changes.sort_by(|a, b| a.path.cmp(&b.path));
            changes.dedup_by(|later, kept| {
                let same = later.path == kept.path;
                if same {
                    kept.event = later.event;
                }
                same
            });
            Changes::Paths(changes)
        };
        if !ctx.send(move |ed| {
            on_change(ed, changes);
        }) {
            return;
        }
    }
}

/// Adds what `events` report to `changes`. Returns whether the kernel dropped events.
fn parse(mut events: &[u8], dirs: &HashMap<i32, PathBuf>, changes: &mut Vec<Change>) -> bool {
    let mut overflowed = false;
    while events.len() >= HEADER {
        let field = |at: usize| u32::from_ne_bytes(events[at..at + 4].try_into().unwrap());
        let (wd, mask, len) = (field(0) as i32, field(4), field(12) as usize);
        let name = &events[HEADER..HEADER + len];
        events = &events[HEADER + len..];
        overflowed |= mask & libc::IN_Q_OVERFLOW != 0;
        // The name is padded with NULs; events about the directory itself have none.
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(len)];
        if let (Some(dir), Some(event)) = (dirs.get(&wd).filter(|_| !name.is_empty()), event_of(mask)) {
            changes.push(Change { path: dir.join(OsStr::from_bytes(name)), event });
        }
    }
    overflowed
}

/// What an event with `mask` reports, if anything: a directory that was only touched is
/// of no interest.
fn event_of(mask: u32) -> Option<Event> {
    let is_dir = mask & libc::IN_ISDIR != 0;
    if mask & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0 {
        Some(Event::Removed)
    } else if !is_dir {
        Some(Event::Written)
    } else if mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
        Some(Event::NewDir)
    } else {
        None
    }
}
