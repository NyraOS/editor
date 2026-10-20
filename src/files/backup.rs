// SPDX-License-Identifier: MIT
//! Backups (`-B`, `-C`), as GNU nano makes them: `NAME~` next to the file, replaced at every
//! save; or, with a backup directory, a new numbered file there at every save, named after the
//! file's full path with `/` written as `!` (`!etc!hosts~`, `!etc!hosts~.1`, ...). A backup has
//! the owner, permissions and times of the old file, but never more than `0600` when it cannot
//! have its owner and group.

use std::ffi::OsString;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rustix::fs::{self, AtFlags, Statx, Timespec, Timestamps};
use rustix::io::Errno;

use super::Target;
use super::sys::{self, Result};

/// Makes the backup of the file opened as `old`, from its contents on disk now.
pub(super) fn make(
    target: &Target,
    old: &OwnedFd,
    st: &Statx,
    backup_dir: Option<&Path>,
) -> Result<PathBuf> {
    let data = sys::read_all(old)?;
    match backup_dir {
        None => {
            let mut name = target.name.clone();
            name.push("~");
            // Written under a temporary name, then renamed over any older backup: never written
            // through a symlink planted as NAME~.
            let (fd, tmp) = sys::create_temp(&target.dir, &name, 0o600)?;
            let result = fill(&fd, &data, st)
                .and_then(|()| fs::renameat(&target.dir, &tmp, &target.dir, &name));
            if let Err(e) = result {
                let _ = fs::unlinkat(&target.dir, &tmp, AtFlags::empty());
                return Err(e);
            }
            sys::sync_dir(&target.dir)?;
            Ok(target.dir_path.join(name))
        }
        Some(dir) => {
            let dir_fd = sys::open_dir(dir)?;
            let base = flat_name(target);
            for n in 0..100_000u32 {
                let mut name = base.clone();
                if n > 0 {
                    name.push(format!(".{n}"));
                }
                let fd = match sys::create_new(&dir_fd, &name, 0o600) {
                    Ok(fd) => fd,
                    Err(Errno::EXIST) => continue,
                    Err(e) => return Err(e),
                };
                if let Err(e) = fill(&fd, &data, st) {
                    let _ = fs::unlinkat(&dir_fd, &name, AtFlags::empty());
                    return Err(e);
                }
                sys::sync_dir(&dir_fd)?;
                return Ok(dir.join(name));
            }
            Err(Errno::EXIST)
        }
    }
}

/// The full path of the file with every `/` as `!`, and a `~`.
fn flat_name(target: &Target) -> OsString {
    let dir = std::fs::canonicalize(if target.dir_path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        &target.dir_path
    })
    .unwrap_or_else(|_| target.dir_path.clone());
    let full = dir.join(&target.name);
    let mut flat: Vec<u8> = full
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|&b| if b == b'/' { b'!' } else { b })
        .collect();
    flat.push(b'~');
    OsString::from_vec(flat)
}

fn fill(fd: &OwnedFd, data: &[u8], st: &Statx) -> Result<()> {
    sys::write_all(fd, data)?;
    sys::copy_owner_and_mode(fd, st)?;
    let time = |t: &fs::StatxTimestamp| Timespec {
        tv_sec: t.tv_sec,
        tv_nsec: t.tv_nsec.into(),
    };
    let times = Timestamps {
        last_access: time(&st.stx_atime),
        last_modification: time(&st.stx_mtime),
    };
    fs::futimens(fd.as_fd(), &times)?;
    fs::fsync(fd)
}
