// SPDX-License-Identifier: MIT
//! The system calls under the files module, through rustix (no unsafe code here).
//!
//! Everything works on directory file descriptors and names inside them, so a path is looked up
//! once and the directory cannot change under a save. Files the editor creates are always new:
//! `O_CREAT | O_EXCL | O_NOFOLLOW`, so an existing file or a symlink planted under that name is
//! never opened.

use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use rustix::fs::{self, AtFlags, CWD, FileType, Mode, OFlags, Statx, StatxFlags, XattrFlags};
use rustix::io::{self as rio, Errno};
use rustix::process::geteuid;

pub(super) type Result<T> = rio::Result<T>;

/// Opens an existing file to read it: never blocks on a FIFO, never becomes the terminal.
pub(super) const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOCTTY);

/// Opens an existing file to write it in place: no `O_CREAT`, no `O_TRUNC`, not through a symlink,
/// never blocking on a FIFO.
const WRITE: OFlags = OFlags::WRONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOCTTY);

/// Creates a file that did not exist; never an existing file, never through a symlink.
const CREATE: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOCTTY);

/// Writes go out in chunks of this size; the tests stop the process between two of them.
const CHUNK: usize = if cfg!(test) { 4096 } else { 1 << 20 };

/// A point of a save where the tests kill the process (`SIGKILL`); nothing outside the tests.
#[cfg(not(test))]
#[inline(always)]
pub(super) fn step(_what: &str) {}

#[cfg(test)]
pub(super) fn step(what: &str) {
    super::tests::step(what);
}

/// The metadata of an open file (statx: the same types on every architecture).
pub(super) fn stat(fd: impl AsFd) -> Result<Statx> {
    fs::statx(
        fd,
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
}

/// The metadata of `name` in `dir`, without following a symlink.
pub(super) fn stat_at(dir: impl AsFd, name: &OsStr) -> Result<Statx> {
    fs::statx(
        dir,
        name,
        AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
}

pub(super) fn file_type(st: &Statx) -> FileType {
    FileType::from_raw_mode(u32::from(st.stx_mode))
}

pub(super) fn mode(st: &Statx) -> u32 {
    u32::from(st.stx_mode) & 0o7777
}

pub(super) fn same_file(a: &Statx, b: &Statx) -> bool {
    a.stx_ino == b.stx_ino
        && a.stx_dev_major == b.stx_dev_major
        && a.stx_dev_minor == b.stx_dev_minor
}

pub(super) fn euid() -> u32 {
    geteuid().as_raw()
}

/// Opens a directory; with only search permission (no read), opens it as a path.
pub(super) fn open_dir(path: &Path) -> Result<OwnedFd> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    let flags = OFlags::DIRECTORY | OFlags::CLOEXEC;
    match fs::openat(CWD, path, flags | OFlags::RDONLY, Mode::empty()) {
        Err(Errno::ACCESS) => fs::openat(CWD, path, flags | OFlags::PATH, Mode::empty()),
        r => r,
    }
}

pub(super) fn open_write(dir: impl AsFd, name: &OsStr) -> Result<OwnedFd> {
    fs::openat(dir, name, WRITE, Mode::empty())
}

/// Creates `name` in `dir`, which must not exist yet; `mode` goes through the umask.
pub(super) fn create_new(dir: impl AsFd, name: &OsStr, mode: u32) -> Result<OwnedFd> {
    fs::openat(dir, name, CREATE, Mode::from_raw_mode(mode))
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Creates a new hidden temporary file next to `name`: `.NAME.PID-N.tmp`.
pub(super) fn create_temp(dir: impl AsFd, name: &OsStr, mode: u32) -> Result<(OwnedFd, OsString)> {
    let pid = rustix::process::getpid().as_raw_nonzero();
    // Long names are cut so the temporary name stays under the 255-byte limit.
    let short = &name.as_bytes()[..name.len().min(200)];
    for _ in 0..100 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut tmp = b".".to_vec();
        tmp.extend_from_slice(short);
        tmp.extend_from_slice(format!(".{pid}-{n}.tmp").as_bytes());
        let tmp = OsString::from_vec(tmp);
        match create_new(dir.as_fd(), &tmp, mode) {
            Ok(fd) => return Ok((fd, tmp)),
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Errno::EXIST)
}

/// Writes all of `bytes` from offset 0, retrying interrupted and short writes.
pub(super) fn write_all(fd: impl AsFd, bytes: &[u8]) -> Result<()> {
    let mut offset = 0u64;
    for chunk in bytes.chunks(CHUNK) {
        let mut rest = chunk;
        while !rest.is_empty() {
            match rio::pwrite(fd.as_fd(), rest, offset) {
                Ok(0) => return Err(Errno::IO),
                Ok(n) => {
                    rest = &rest[n..];
                    offset += n as u64;
                }
                Err(Errno::INTR) => {}
                Err(e) => return Err(e),
            }
        }
        if offset < bytes.len() as u64 {
            step("between two chunks");
        }
    }
    Ok(())
}

/// Reads a whole file from offset 0.
pub(super) fn read_all(fd: impl AsFd) -> Result<Vec<u8>> {
    let size = stat(fd.as_fd())?.stx_size;
    let mut buf = Vec::with_capacity(usize::try_from(size).unwrap_or(0).saturating_add(1));
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match rio::pread(fd.as_fd(), &mut chunk[..], buf.len() as u64) {
            Ok(0) => return Ok(buf),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(Errno::INTR) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Syncs a directory, so a new name in it survives a power cut. A directory opened only as a path
/// cannot be synced (`EBADF`), and some file systems do not sync directories (`EINVAL`).
pub(super) fn sync_dir(dir: impl AsFd) -> Result<()> {
    match fs::fsync(dir) {
        Err(Errno::BADF | Errno::INVAL) => Ok(()),
        r => r,
    }
}

/// Gives a copy the owner of the original if it can (root), and its permissions, but never more
/// than `0600` when the copy cannot have the original's owner and group. No set-user-ID bits.
pub(super) fn copy_owner_and_mode(fd: impl AsFd, st: &Statx) -> Result<()> {
    let uid = rustix::fs::Uid::from_raw(st.stx_uid);
    let gid = rustix::fs::Gid::from_raw(st.stx_gid);
    let mode = if fs::fchown(fd.as_fd(), Some(uid), Some(gid)).is_ok() {
        mode(st) & 0o777
    } else {
        mode(st) & 0o600
    };
    fs::fchmod(fd, Mode::from_raw_mode(mode))
}

fn list_xattrs(fd: impl AsFd) -> Result<Vec<OsString>> {
    loop {
        let size = match fs::flistxattr(fd.as_fd(), &mut [0u8; 0]) {
            Err(Errno::NOTSUP) => return Ok(Vec::new()),
            r => r?,
        };
        let mut buf = vec![0u8; size];
        match fs::flistxattr(fd.as_fd(), &mut buf[..]) {
            Ok(n) => {
                buf.truncate(n);
                let names = buf.split(|&b| b == 0).filter(|n| !n.is_empty());
                return Ok(names.map(|n| OsString::from_vec(n.to_vec())).collect());
            }
            // The list grew between the two calls.
            Err(Errno::RANGE) => {}
            Err(e) => return Err(e),
        }
    }
}

fn get_xattr(fd: impl AsFd, name: &OsStr) -> Result<Vec<u8>> {
    loop {
        let size = fs::fgetxattr(fd.as_fd(), name, &mut [0u8; 0])?;
        let mut buf = vec![0u8; size];
        match fs::fgetxattr(fd.as_fd(), name, &mut buf[..]) {
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf);
            }
            Err(Errno::RANGE) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Makes the extended attributes of `to` (ACLs, security labels, capabilities, user attributes)
/// the same as those of `from`, removing any `to` got on its own (such as a default ACL).
pub(super) fn copy_xattrs(from: impl AsFd, to: impl AsFd) -> Result<()> {
    let names = list_xattrs(from.as_fd())?;
    for name in &names {
        let value = get_xattr(from.as_fd(), name)?;
        fs::fsetxattr(to.as_fd(), name, &value, XattrFlags::empty())?;
    }
    for name in list_xattrs(to.as_fd())? {
        if !names.contains(&name) {
            fs::fremovexattr(to.as_fd(), &name)?;
        }
    }
    Ok(())
}

/// The inode flags (`chattr`) of two files are the same, or cannot be read.
pub(super) fn same_inode_flags(a: impl AsFd, b: impl AsFd) -> bool {
    match (fs::ioctl_getflags(a), fs::ioctl_getflags(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => true,
    }
}

/// The name and home directory of a user, from `/etc/passwd`.
pub(super) fn passwd(uid: u32) -> Option<(OsString, PathBuf)> {
    let data = std::fs::read("/etc/passwd").ok()?;
    data.split(|&b| b == b'\n').find_map(|line| {
        let fields: Vec<&[u8]> = line.split(|&b| b == b':').collect();
        let entry_uid: u32 = std::str::from_utf8(fields.get(2)?).ok()?.parse().ok()?;
        (entry_uid == uid).then(|| {
            let name = OsString::from_vec(fields[0].to_vec());
            let home = PathBuf::from(OsString::from_vec(fields.get(5)?.to_vec()));
            Some((name, home))
        })?
    })
}

/// The home directory of the user running the editor (effective user ID): `$HOME` or the one in
/// `/etc/passwd`, whichever is a directory that user owns. Under `sudo`, `$HOME` may be the
/// invoking user's, so it is checked, not trusted.
pub(super) fn home_dir() -> Result<(OwnedFd, PathBuf)> {
    let euid = euid();
    let from_env = std::env::var_os("HOME").map(PathBuf::from);
    let from_passwd = passwd(euid).map(|(_, home)| home);
    for home in [from_env, from_passwd].into_iter().flatten() {
        if !home.is_absolute() {
            continue;
        }
        if let Ok(fd) = open_dir(&home) {
            if stat(&fd).is_ok_and(|st| st.stx_uid == euid) {
                return Ok((fd, home));
            }
        }
    }
    Err(Errno::NOENT)
}
