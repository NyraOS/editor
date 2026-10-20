// SPDX-License-Identifier: MIT
//! Saving a file: replaced in one step (rename), or written in place after a copy.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{self, FallocateFlags, Mode, OFlags, RenameFlags, Statx, StatxFlags};
use rustix::io::Errno;

use super::sys::{self, step};
use super::{Meta, Target, backup, resolve};

/// How to save, from the options of nano: `-B` (`backup`), `-C` (`backup_dir`) and our
/// `set inplace` (`in_place`), which always writes into the file itself.
#[derive(Clone, Debug, Default)]
pub struct SaveOptions {
    pub backup: bool,
    pub backup_dir: Option<PathBuf>,
    pub in_place: bool,
}

/// How the file was saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// A new file took the name of the old one (or the file is new).
    Replaced,
    /// The new contents were written into the existing file.
    InPlace,
}

#[derive(Debug)]
pub struct Saved {
    pub method: Method,
    /// The backup made (`-B`), if any.
    pub backup: Option<PathBuf>,
    /// The file as saved, to notice later changes on disk.
    pub meta: Meta,
}

#[derive(Debug)]
pub enum SaveError {
    /// The backup could not be made; nothing was written. nano asks whether to save without it.
    Backup(io::Error),
    /// The file could not be saved. `copy` is a copy of the old contents kept because the write
    /// into the file itself failed halfway.
    Write {
        error: io::Error,
        copy: Option<PathBuf>,
    },
}

impl SaveError {
    fn write(error: impl Into<io::Error>) -> Self {
        SaveError::Write {
            error: error.into(),
            copy: None,
        }
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Backup(e) => write!(f, "Cannot make backup: {e}"),
            SaveError::Write { error, copy: None } => write!(f, "{error}"),
            SaveError::Write {
                error,
                copy: Some(copy),
            } => write!(f, "{error} (the old contents are in {})", copy.display()),
        }
    }
}

impl std::error::Error for SaveError {}

/// Saves `bytes` as the file at `path`; see the module documentation for the guarantees.
pub fn save(path: &Path, bytes: &[u8], opts: &SaveOptions) -> Result<Saved, SaveError> {
    let target = resolve(path).map_err(SaveError::write)?;
    let old = match fs::openat(
        &target.dir,
        &target.name,
        sys::READ | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return save_new(&target, bytes),
        // A file we may write but not read: open it only to look at it.
        Err(Errno::ACCESS) => fs::openat(
            &target.dir,
            &target.name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(SaveError::write)?,
        Err(e) => return Err(SaveError::write(e)),
    };
    let st = sys::stat(&old).map_err(SaveError::write)?;
    super::regular(&st).map_err(SaveError::write)?;

    let backup = if opts.backup {
        let made = backup::make(&target, &old, &st, opts.backup_dir.as_deref());
        Some(made.map_err(|e| SaveError::Backup(e.into()))?)
    } else {
        None
    };
    let saved = |method, meta| Saved {
        method,
        backup: backup.clone(),
        meta,
    };
    if !opts.in_place && st.stx_nlink == 1 && !mount_point(&target, &st) {
        match replace(&target, &old, &st, bytes) {
            Ok(meta) => return Ok(saved(Method::Replaced, meta)),
            Err(Replace::InPlace) => {}
            Err(Replace::Failed(e)) => return Err(SaveError::write(e)),
        }
    }
    let meta = in_place(&target, &old, &st, bytes, backup.is_some())?;
    Ok(saved(Method::InPlace, meta))
}

/// The file is mounted on its own (a bind mount): renaming over it would fail (`EBUSY`), or would
/// change the file under the mount and not the one mounted.
fn mount_point(target: &Target, st: &Statx) -> bool {
    let Ok(dir) = sys::stat(&target.dir) else {
        return false;
    };
    let both = |flag| st.stx_mask & dir.stx_mask & flag != 0;
    let other_mount = both(StatxFlags::MNT_ID.bits()) && st.stx_mnt_id != dir.stx_mnt_id;
    let other_dev = (st.stx_dev_major, st.stx_dev_minor) != (dir.stx_dev_major, dir.stx_dev_minor);
    other_mount || other_dev
}

/// Why a replacement did not happen.
enum Replace {
    /// Renaming would lose something (owner, attributes) or is not possible: write in place.
    InPlace,
    Failed(Errno),
}

/// Writes the new contents to a temporary file next to the original, gives it the original's
/// metadata and renames it over the original.
fn replace(target: &Target, old: &OwnedFd, st: &Statx, bytes: &[u8]) -> Result<Meta, Replace> {
    let (tmp_fd, tmp) = match sys::create_temp(&target.dir, &target.name, 0o600) {
        Ok(created) => created,
        // No file can be created in the directory.
        Err(Errno::ACCESS | Errno::PERM | Errno::ROFS) => return Err(Replace::InPlace),
        Err(e) => return Err(Replace::Failed(e)),
    };
    step("temporary file created");
    let result = fill_and_rename(target, old, st, bytes, &tmp_fd, &tmp);
    if result.is_err() {
        let _ = fs::unlinkat(&target.dir, &tmp, fs::AtFlags::empty());
    }
    result
}

fn fill_and_rename(
    target: &Target,
    old: &OwnedFd,
    st: &Statx,
    bytes: &[u8],
    tmp_fd: &OwnedFd,
    tmp: &OsString,
) -> Result<Meta, Replace> {
    sys::write_all(tmp_fd, bytes).map_err(Replace::Failed)?;
    step("temporary file written");
    // The metadata comes after the data: a write by a user other than root clears the
    // set-user-ID and set-group-ID bits.
    if !copy_metadata(old, st, tmp_fd) {
        return Err(Replace::InPlace);
    }
    step("metadata copied");
    fs::fsync(tmp_fd).map_err(Replace::Failed)?;
    step("temporary file synced");
    match fs::renameat(&target.dir, tmp, &target.dir, &target.name) {
        Ok(()) => {}
        // A mount point the check above did not see, or a sticky directory where only the
        // owner of the file may replace it.
        Err(Errno::BUSY | Errno::PERM | Errno::ACCESS) => return Err(Replace::InPlace),
        Err(e) => return Err(Replace::Failed(e)),
    }
    step("renamed");
    // From here the new file is in place: an error is reported, nothing is undone.
    sys::sync_dir(&target.dir).map_err(Replace::Failed)?;
    step("directory synced");
    sys::stat(tmp_fd)
        .map(|st| Meta::new(&st))
        .map_err(Replace::Failed)
}

/// Gives the new file the owner, group, permissions, extended attributes (ACLs, security labels,
/// capabilities) and inode flags of the original; false when one of them cannot be had.
fn copy_metadata(old: &OwnedFd, st: &Statx, tmp: &OwnedFd) -> bool {
    let uid = fs::Uid::from_raw(st.stx_uid);
    let gid = fs::Gid::from_raw(st.stx_gid);
    fs::fchown(tmp, Some(uid), Some(gid)).is_ok()
        && fs::fchmod(tmp, Mode::from_raw_mode(sys::mode(st))).is_ok()
        && sys::copy_xattrs(old, tmp).is_ok()
        && sys::same_inode_flags(old, tmp)
}

/// Writes the new contents into the file itself, after a copy of the old contents (the backup, or
/// a hidden copy next to the file, removed at the end). When the directory is not writable no copy
/// can be made there; the space is reserved first, so a full disk stops the save before anything
/// is written.
fn in_place(
    target: &Target,
    old: &OwnedFd,
    st: &Statx,
    bytes: &[u8],
    backed_up: bool,
) -> Result<Meta, SaveError> {
    let fd = sys::open_write(&target.dir, &target.name).map_err(SaveError::write)?;
    let now = sys::stat(&fd).map_err(SaveError::write)?;
    if !sys::same_file(&now, st) {
        // Another file took the name since it was opened.
        return Err(SaveError::write(Errno::AGAIN));
    }
    let copy = if backed_up {
        None
    } else {
        safety_copy(target, old, st).map_err(SaveError::write)?
    };
    let copy_path = copy.as_ref().map(|name| target.dir_path.join(name));
    let fail = |e: Errno, keep: bool| {
        if let (false, Some(name)) = (keep, &copy) {
            let _ = fs::unlinkat(&target.dir, name, fs::AtFlags::empty());
        }
        SaveError::Write {
            error: e.into(),
            copy: if keep { copy_path.clone() } else { None },
        }
    };
    step("copy made");
    // Not every file system can reserve space; there the write finds out.
    let reserved = fs::fallocate(&fd, FallocateFlags::KEEP_SIZE, 0, bytes.len() as u64);
    if let Err(e @ (Errno::NOSPC | Errno::DQUOT)) = reserved {
        return Err(fail(e, false));
    }
    step("space reserved");
    sys::write_all(&fd, bytes).map_err(|e| fail(e, true))?;
    step("written in place");
    let size = sys::stat(&fd).map_err(|e| fail(e, true))?.stx_size;
    if size > bytes.len() as u64 {
        fs::ftruncate(&fd, bytes.len() as u64).map_err(|e| fail(e, true))?;
    }
    step("truncated");
    fs::fsync(&fd).map_err(|e| fail(e, true))?;
    step("synced in place");
    if let Some(name) = &copy {
        let _ = fs::unlinkat(&target.dir, name, fs::AtFlags::empty());
    }
    sys::stat(&fd)
        .map(|st| Meta::new(&st))
        .map_err(SaveError::write)
}

/// A hidden copy of the old contents next to the file, synced with its directory; `None` when the
/// directory is not writable or the file cannot be read.
fn safety_copy(target: &Target, old: &OwnedFd, st: &Statx) -> Result<Option<OsString>, Errno> {
    let data = match sys::read_all(old) {
        Ok(data) => data,
        // Opened only as a path: a file we may write but not read.
        Err(Errno::BADF) => return Ok(None),
        Err(e) => return Err(e),
    };
    let (fd, name) = match sys::create_temp(&target.dir, &target.name, 0o600) {
        Ok(created) => created,
        Err(Errno::ACCESS | Errno::PERM | Errno::ROFS) => return Ok(None),
        Err(e) => return Err(e),
    };
    let result = (|| {
        sys::write_all(&fd, &data)?;
        sys::copy_owner_and_mode(&fd, st)?;
        fs::fsync(&fd)?;
        sys::sync_dir(&target.dir)
    })();
    match result {
        Ok(()) => Ok(Some(name)),
        Err(e) => {
            let _ = fs::unlinkat(&target.dir, &name, fs::AtFlags::empty());
            Err(e)
        }
    }
}

/// A new file: written under a temporary name, then renamed without replacing anything, so a
/// crash never leaves half a file under its name. Mode `0666` goes through the umask and the
/// directory's default ACL, as for any new file.
fn save_new(target: &Target, bytes: &[u8]) -> Result<Saved, SaveError> {
    let (fd, tmp) = sys::create_temp(&target.dir, &target.name, 0o666).map_err(SaveError::write)?;
    step("temporary file created");
    let result = (|| {
        sys::write_all(&fd, bytes)?;
        step("temporary file written");
        fs::fsync(&fd)?;
        step("temporary file synced");
        let flags = RenameFlags::NOREPLACE;
        match fs::renameat_with(&target.dir, &tmp, &target.dir, &target.name, flags) {
            // A file system without RENAME_NOREPLACE: the name was free a moment ago.
            Err(Errno::INVAL) => fs::renameat(&target.dir, &tmp, &target.dir, &target.name),
            r => r,
        }
    })();
    if let Err(e) = result {
        let _ = fs::unlinkat(&target.dir, &tmp, fs::AtFlags::empty());
        return Err(SaveError::write(e));
    }
    step("renamed");
    sys::sync_dir(&target.dir).map_err(SaveError::write)?;
    step("directory synced");
    let st = sys::stat(fd.as_fd()).map_err(SaveError::write)?;
    Ok(Saved {
        method: Method::Replaced,
        backup: None,
        meta: Meta::new(&st),
    })
}
