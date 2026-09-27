//! The `ls -l` columns of a listing: permissions, owner, group, size and modification
//! time, read with one `lstat` per entry (plus a `readlink` per symlink).
//!
//! Formatting happens once per render into `Cells`, whose widths set the columns, so a
//! listing lines up however long its owner names or sizes are.

use std::fs::DirEntry;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// An entry's metadata, as `lstat` reports it (a symlink's own, not its target's).
#[derive(Debug, Clone)]
pub struct Details {
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    /// Seconds since the epoch.
    mtime: i64,
    /// Where a symlink points.
    target: Option<PathBuf>,
}

impl Details {
    pub fn read(entry: &DirEntry) -> Option<Details> {
        let (mode, uid, gid, size, mtime) = sys::stat(&entry.metadata().ok()?)?;
        let is_link = mode & sys::S_IFMT == sys::S_IFLNK;
        let target = is_link.then(|| std::fs::read_link(entry.path()).ok()).flatten();
        Some(Details { mode, uid, gid, size, mtime, target })
    }

    pub fn target(&self) -> Option<&PathBuf> {
        self.target.as_ref()
    }
}

/// The formatted columns of one entry.
#[derive(Default)]
pub struct Cells {
    pub permissions: String,
    pub owner: String,
    pub group: String,
    /// Empty for directories, whose size says nothing useful.
    pub size: String,
    pub modified: String,
}

/// Formats entries' details; one per render, so it caches owner and group names.
pub struct Formatter {
    now: i64,
    users: Vec<(u32, String)>,
    groups: Vec<(u32, String)>,
}

impl Formatter {
    pub fn new() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        Self { now, users: Vec::new(), groups: Vec::new() }
    }

    pub fn cells(&mut self, details: &Details, is_dir: bool) -> Cells {
        Cells {
            permissions: permissions(details.mode),
            owner: cached_name(&mut self.users, details.uid, sys::user_name),
            group: cached_name(&mut self.groups, details.gid, sys::group_name),
            size: if is_dir { String::new() } else { human_size(details.size) },
            modified: self.date(details.mtime),
        }
    }

    /// `Sep 27 14:02` for the last six months, else `Sep 27  2025`, as `ls` shows them.
    fn date(&self, mtime: i64) -> String {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        const SIX_MONTHS: i64 = 15_778_476;
        let Some(t) = sys::local_time(mtime) else { return String::new() };
        let month = MONTHS[t.month.min(11) as usize];
        if (self.now - SIX_MONTHS..=self.now + 3600).contains(&mtime) {
            format!("{} {:>2} {:02}:{:02}", month, t.day, t.hour, t.minute)
        } else {
            format!("{} {:>2}  {:>4}", month, t.day, t.year)
        }
    }
}

/// The name of user or group `id`, looked up once per render; the number when unknown.
fn cached_name(cache: &mut Vec<(u32, String)>, id: u32, lookup: fn(u32) -> Option<String>) -> String {
    if let Some((_, name)) = cache.iter().find(|(cached, _)| *cached == id) {
        return name.clone();
    }
    let name = lookup(id).unwrap_or_else(|| id.to_string());
    cache.push((id, name.clone()));
    name
}

/// `drwxr-xr-x`: the file type, then read/write/execute for owner, group and others, with
/// setuid, setgid and sticky shown in the execute slots.
fn permissions(mode: u32) -> String {
    let kind = match mode & sys::S_IFMT {
        sys::S_IFDIR => 'd',
        sys::S_IFLNK => 'l',
        sys::S_IFCHR => 'c',
        sys::S_IFBLK => 'b',
        sys::S_IFIFO => 'p',
        sys::S_IFSOCK => 's',
        _ => '-',
    };
    let mut out = [kind, '-', '-', '-', '-', '-', '-', '-', '-', '-'];
    for (i, ch) in ['r', 'w', 'x'].into_iter().cycle().take(9).enumerate() {
        if mode & (0o400 >> i) != 0 {
            out[i + 1] = ch;
        }
    }
    for (bit, slot, set) in [(0o4000, 3, 's'), (0o2000, 6, 's'), (0o1000, 9, 't')] {
        if mode & bit != 0 {
            out[slot] = if out[slot] == 'x' { set } else { set.to_ascii_uppercase() };
        }
    }
    out.iter().collect()
}

/// `size` as `ls -h` shows it: bytes below 1K, then one decimal below 10 of a unit (`4.0K`)
/// and whole units above (`12K`), rounded up.
fn human_size(size: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if size < 1024 {
        return size.to_string();
    }
    let mut value = size as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let tenths = (value * 10.0).ceil() as u64;
    if tenths < 100 {
        return format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit]);
    }
    match value.ceil() as u64 {
        1024 if unit + 1 < UNITS.len() => format!("1.0{}", UNITS[unit + 1]),
        whole => format!("{}{}", whole, UNITS[unit]),
    }
}

struct LocalTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // `mode_t` is u32 on Linux but u16 on macOS.
mod sys {
    use std::ffi::CStr;
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;

    use super::LocalTime;

    pub const S_IFMT: u32 = libc::S_IFMT as u32;
    pub const S_IFDIR: u32 = libc::S_IFDIR as u32;
    pub const S_IFLNK: u32 = libc::S_IFLNK as u32;
    pub const S_IFCHR: u32 = libc::S_IFCHR as u32;
    pub const S_IFBLK: u32 = libc::S_IFBLK as u32;
    pub const S_IFIFO: u32 = libc::S_IFIFO as u32;
    pub const S_IFSOCK: u32 = libc::S_IFSOCK as u32;

    /// (mode, uid, gid, size, mtime)
    pub fn stat(meta: &Metadata) -> Option<(u32, u32, u32, u64, i64)> {
        Some((meta.mode(), meta.uid(), meta.gid(), meta.size(), meta.mtime()))
    }

    pub fn user_name(uid: u32) -> Option<String> {
        // SAFETY: getpwuid_r writes only into `pwd` and `buf`, which outlive the call, and
        // `pw_name` points into `buf` when it succeeds.
        with_buffer(|buf, result: &mut *mut libc::passwd| unsafe {
            let mut pwd: libc::passwd = std::mem::zeroed();
            let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), result);
            (rc, (!result.is_null()).then(|| CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned()))
        })
    }

    pub fn group_name(gid: u32) -> Option<String> {
        // SAFETY: as in `user_name`, for getgrgid_r.
        with_buffer(|buf, result: &mut *mut libc::group| unsafe {
            let mut grp: libc::group = std::mem::zeroed();
            let rc = libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), result);
            (rc, (!result.is_null()).then(|| CStr::from_ptr(grp.gr_name).to_string_lossy().into_owned()))
        })
    }

    /// Runs a `get*_r` lookup, growing its string buffer while it reports ERANGE.
    fn with_buffer<T>(
        mut lookup: impl FnMut(&mut [libc::c_char], &mut *mut T) -> (i32, Option<String>),
    ) -> Option<String> {
        let mut buf = vec![0; 1024];
        loop {
            let mut result = std::ptr::null_mut();
            match lookup(&mut buf, &mut result) {
                (libc::ERANGE, _) if buf.len() < 1 << 20 => buf.resize(buf.len() * 2, 0),
                (_, name) => return name,
            }
        }
    }

    pub fn local_time(secs: i64) -> Option<LocalTime> {
        let time = secs as libc::time_t;
        // SAFETY: localtime_r reads `time` and fills `tm`, both owned here.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&time, &mut tm).is_null() {
                return None;
            }
            tm
        };
        Some(LocalTime {
            year: tm.tm_year + 1900,
            month: tm.tm_mon as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
        })
    }
}

/// Elsewhere listings show names only: there are no Unix modes, owners or local time.
#[cfg(not(unix))]
mod sys {
    use std::fs::Metadata;

    use super::LocalTime;

    pub const S_IFMT: u32 = 0o170000;
    pub const S_IFDIR: u32 = 0o040000;
    pub const S_IFLNK: u32 = 0o120000;
    pub const S_IFCHR: u32 = 0o020000;
    pub const S_IFBLK: u32 = 0o060000;
    pub const S_IFIFO: u32 = 0o010000;
    pub const S_IFSOCK: u32 = 0o140000;

    pub fn stat(_: &Metadata) -> Option<(u32, u32, u32, u64, i64)> {
        None
    }

    pub fn user_name(_: u32) -> Option<String> {
        None
    }

    pub fn group_name(_: u32) -> Option<String> {
        None
    }

    pub fn local_time(_: i64) -> Option<LocalTime> {
        None
    }
}
