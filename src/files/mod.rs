// SPDX-License-Identifier: MIT
//! Files: reading, saving without losing data, emergency copies, lock files and backups.
//!
//! [`save`] saves only over the file the editor expects (the one it read or saved, or none):
//! another file, or a symlink to one, swapped in under the name is refused. It replaces a file in
//! one step: it writes a new file next to it (`O_CREAT | O_EXCL`, mode `0600`), gives it the owner,
//! the ACLs and other extended attributes, then the permissions of the original, so it never lets
//! in anyone the original keeps out; syncs it, and only then gives it set-user-ID bits and file
//! capabilities, so a save killed earlier never leaves a privileged copy; renames it over the
//! original and syncs the directory. A crash or a `SIGKILL` at any point leaves the old file whole
//! or the new one whole.
//!
//! When renaming would break something, it writes into the file itself, as GNU nano does, after a
//! copy of the old contents (the backup, or a hidden copy next to the file that is removed once the
//! new contents are synced): the file has several hard links, it is mounted on its own (a bind
//! mount, as `/etc/resolv.conf` and `/etc/hosts` in containers), no file can be created in its
//! directory, or the new file could not get the original's owner, group, extended attributes or
//! inode flags (not running as root). The space is reserved before writing, so a full disk stops
//! the save before the file changes. In a directory where no file can be created, no copy can be
//! made: a crash in the middle of that write can leave a mixed file (the editor still has the
//! text). A symlink is followed: the file it points to is saved, and the symlink stays a symlink.
//! Unlike nano, root does not create a file through another user's symlink to a file that does
//! not exist (a user's dangling `~/.ssh/authorized_keys` link would let that user choose where
//! root's new file appears); saving over an existing file through it works as in nano.
//!
//! Files the editor creates (temporary files, copies, backups, emergency copies, lock files) are
//! always new files, created with `O_EXCL | O_NOFOLLOW` in a directory opened once, so no
//! symlink or file planted under their names is ever written through. Temporary names have 64
//! random bits. Copies never get an ACL, so when the original has one they keep only the owner's
//! permission bits (the group bits of a file with an ACL are the ACL's mask).

mod backup;
mod emergency;
mod lock;
mod save;
mod sys;
#[cfg(test)]
mod tests;

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rustix::fs::{self, CWD, FileType, Mode, Statx};
use rustix::io::Errno;

pub use emergency::{emergency_save, watch_signals};
pub use lock::{Lock, LockInfo, Locking, lock};
pub use save::{Expected, Method, SaveError, SaveOptions, Saved, save};

/// What the editor remembers about a file it read or saved, to notice later that it changed on
/// disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub dev: (u32, u32),
    pub ino: u64,
    pub size: u64,
    /// Seconds and nanoseconds.
    pub mtime: (i64, u32),
    /// The permission bits, with the set-user-ID, set-group-ID and sticky bits.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Meta {
    fn new(st: &Statx) -> Self {
        Meta {
            dev: (st.stx_dev_major, st.stx_dev_minor),
            ino: st.stx_ino,
            size: st.stx_size,
            mtime: (st.stx_mtime.tv_sec, st.stx_mtime.tv_nsec),
            mode: sys::mode(st),
            uid: st.stx_uid,
            gid: st.stx_gid,
        }
    }

    /// The file is not the one described by `earlier` any more: another file took its name, or
    /// its contents were written since.
    pub fn changed_since(&self, earlier: &Meta) -> bool {
        (self.dev, self.ino, self.size, self.mtime)
            != (earlier.dev, earlier.ino, earlier.size, earlier.mtime)
    }
}

/// A file read from disk.
#[derive(Debug)]
pub struct Loaded {
    pub bytes: Vec<u8>,
    pub meta: Meta,
}

/// Reads a whole file. A path that does not exist gives an error of kind `NotFound` (a new file);
/// a directory, a device or any other file that is not a regular file is refused, without
/// blocking on a FIFO.
pub fn read(path: &Path) -> io::Result<Loaded> {
    let fd = fs::openat(CWD, path, sys::READ, Mode::empty())?;
    let st = sys::stat(&fd)?;
    regular(&st)?;
    Ok(Loaded {
        bytes: sys::read_all(&fd)?,
        meta: Meta::new(&st),
    })
}

/// The metadata of a file now (following symlinks), or `None` when it does not exist.
pub fn stat(path: &Path) -> io::Result<Option<Meta>> {
    match fs::statx(CWD, path, fs::AtFlags::empty(), fs::StatxFlags::BASIC_STATS) {
        Ok(st) => Ok(Some(Meta::new(&st))),
        Err(Errno::NOENT) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The error for anything but a regular file, with the words nano uses.
fn regular(st: &Statx) -> io::Result<()> {
    match sys::file_type(st) {
        FileType::RegularFile => Ok(()),
        FileType::Directory => Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "is a directory",
        )),
        FileType::CharacterDevice | FileType::BlockDevice => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "is a device file",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "is not a normal file",
        )),
    }
}

/// A file to save, found once: its directory, opened, and its name in it, which is not a symlink
/// (or does not exist).
struct Target {
    dir: OwnedFd,
    /// The path of the directory, only to name backups in a backup directory.
    dir_path: PathBuf,
    name: OsString,
    /// A symlink of another user was followed to get here.
    foreign_link: bool,
}

/// Splits a path into its directory and its last component, which must be a name.
fn split(path: &Path) -> Result<(&Path, &OsStr), Errno> {
    let bytes = path.as_os_str().as_bytes();
    match path.file_name() {
        Some(name) if !bytes.ends_with(b"/") && !bytes.ends_with(b"/.") && bytes != b"." => {
            Ok((path.parent().unwrap_or(Path::new("")), name))
        }
        _ => Err(Errno::ISDIR),
    }
}

/// Finds the file a path names, following symlinks in the last component as the kernel does,
/// including its rule for sticky, world-writable directories such as `/tmp` (symlinks there are
/// followed only when they belong to the user or to the directory's owner), so another user's
/// symlink cannot send a save somewhere else.
fn resolve(path: &Path) -> Result<Target, Errno> {
    let mut path = path.to_path_buf();
    let mut foreign_link = false;
    // The kernel's limit on symlinks in one lookup.
    for _ in 0..40 {
        let (parent, name) = split(&path)?;
        let dir = sys::open_dir(parent)?;
        match sys::stat_at(&dir, name) {
            Ok(st) if sys::file_type(&st) == FileType::Symlink => {
                let d = sys::stat(&dir)?;
                let sticky_and_writable = sys::mode(&d) & 0o1002 == 0o1002;
                if sticky_and_writable && st.stx_uid != sys::euid() && st.stx_uid != d.stx_uid {
                    return Err(Errno::ACCESS);
                }
                foreign_link |= st.stx_uid != sys::euid();
                let link = fs::readlinkat(&dir, name, Vec::new())?.into_bytes();
                let link = PathBuf::from(OsString::from_vec(link));
                path = parent.join(link);
            }
            Ok(_) | Err(Errno::NOENT) => {
                return Ok(Target {
                    dir,
                    dir_path: parent.to_path_buf(),
                    name: name.to_os_string(),
                    foreign_link,
                });
            }
            Err(e) => return Err(e),
        }
    }
    Err(Errno::LOOP)
}
