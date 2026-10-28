// SPDX-License-Identifier: MIT
//! Emergency copies, as GNU nano writes them when its terminal goes away (`SIGHUP`, such as a
//! closed SSH connection) or it is told to stop (`SIGTERM`): the text of a modified buffer goes to
//! `NAME.save`, or `NAME.save.1`, `NAME.save.2`, ... when that exists; a buffer without a name
//! goes to `nano.PID.save` in the current directory.
//!
//! Unlike nano, the copy goes next to the file only when both the file and the directory belong
//! to the user running the editor. Otherwise (root editing a user's file, or a file in a directory
//! another user controls) it goes to that user's home directory: a directory another user
//! controls is where the emergency copy of nano went wrong in 2024 (CVE-2024-5742). The copy also
//! goes there when it cannot be written next to the file. It is always a new file
//! (`O_EXCL | O_NOFOLLOW`), with the permissions of the original file when it is the user's own
//! (only the owner's with an ACL), and `0600` otherwise.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rustix::fs::{self, AtFlags, FileType, Gid, Mode, OFlags, StatxFlags};
use rustix::io::Errno;
use signal_hook::consts::{SIGHUP, SIGTERM};

use super::sys::{self, Result};

/// Makes `SIGHUP` and `SIGTERM` set the returned flag instead of ending the editor, which then
/// writes the emergency copies of its modified buffers ([`emergency_save_all`]) and exits. Waits for
/// keys must end at least every second or so to look at the flag (a closed terminal also ends them
/// with an error).
pub fn watch_signals() -> io::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    for signal in [SIGHUP, SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&flag))?;
    }
    Ok(flag)
}

/// How many copies are written before they are synced, as each holds two descriptors until then.
const BATCH: usize = 64;

/// A copy that is written but not synced yet.
struct Copy {
    path: PathBuf,
    name: OsString,
    dir: OwnedFd,
    file: OwnedFd,
    /// It is in the home directory, where there is no other place to try.
    home: bool,
}

/// Writes the emergency copy of each buffer (`path` is its file name, if it has one); returns
/// where each went, or why it did not. A buffer that cannot be written does not stop the others.
///
/// The copies are written first and synced after, a batch at a time: a sync waits for the disk
/// (70 to 300 ms on a busy one), the first one of a batch takes the data of all the others with
/// it, and with 256 buffers one sync each would take minutes, which a terminal that is going
/// away, or a `SIGKILL` after `SIGTERM`, does not wait for. A copy that cannot be synced is
/// removed and written again in the home directory, as one that cannot be written is.
pub fn emergency_save_all(buffers: &[(Option<&Path>, &[u8])]) -> Vec<io::Result<PathBuf>> {
    let mut results = Vec::with_capacity(buffers.len());
    for batch in buffers.chunks(BATCH) {
        let written: Vec<io::Result<Copy>> = batch
            .iter()
            .map(|&(path, bytes)| write_copy(path, bytes, false))
            .collect();
        for (copy, &(path, bytes)) in written.into_iter().zip(batch) {
            results.push(copy.and_then(|copy| synced(copy, path, bytes)));
        }
    }
    results
}

/// Syncs a copy; if that fails, removes it and tries the home directory, unless it is there.
fn synced(copy: Copy, path: Option<&Path>, bytes: &[u8]) -> io::Result<PathBuf> {
    let Err(error) = sync(&copy) else {
        return Ok(copy.path);
    };
    let _ = fs::unlinkat(&copy.dir, &copy.name, AtFlags::empty());
    if copy.home {
        return Err(error.into());
    }
    let again = write_copy(path, bytes, true)?;
    match sync(&again) {
        Ok(()) => Ok(again.path),
        Err(error) => {
            let _ = fs::unlinkat(&again.dir, &again.name, AtFlags::empty());
            Err(error.into())
        }
    }
}

fn sync(copy: &Copy) -> Result<()> {
    fs::fsync(&copy.file)?;
    sys::sync_dir(&copy.dir)
}

/// Writes the copy of one buffer, next to the file if that is allowed and works, else in the home
/// directory (only there when `home_only`), without syncing it.
fn write_copy(path: Option<&Path>, bytes: &[u8], home_only: bool) -> io::Result<Copy> {
    let (dir_path, base, name) = match path.and_then(|p| Some((p, p.file_name()?))) {
        Some((path, name)) => {
            let mut base = name.to_os_string();
            base.push(".save");
            (path.parent().unwrap_or(Path::new("")), base, Some(name))
        }
        None => {
            let base = format!("nano.{}.save", rustix::process::getpid().as_raw_nonzero());
            (Path::new(""), OsString::from(base), None)
        }
    };
    if let (false, Ok(dir)) = (home_only, sys::open_dir(dir_path)) {
        if let Some(perms) = beside(&dir, name) {
            if let Ok((file, written)) = write_numbered(&dir, &base, perms, bytes) {
                let path = dir_path.join(&written);
                return Ok(Copy {
                    path,
                    name: written,
                    dir,
                    file,
                    home: false,
                });
            }
        }
    }
    let (dir, home_path) = sys::home_dir()?;
    let (file, name) = write_numbered(&dir, &base, None, bytes)?;
    Ok(Copy {
        path: home_path.join(&name),
        name,
        dir,
        file,
        home: true,
    })
}

/// Whether the copy may go next to the file: the directory opened belongs to the user running
/// the editor, and so does the file (or there is no file yet). If so, the mode and group for the
/// copy: the file's own (owner bits only with an ACL), or `0600` (`None`) for a new file or a
/// symlink, whose target may sit in a directory others cannot enter.
fn beside(dir: &OwnedFd, name: Option<&OsStr>) -> Option<Option<(u32, u32)>> {
    let euid = sys::euid();
    if sys::stat(dir).ok()?.stx_uid != euid {
        return None;
    }
    let Some(name) = name else {
        return Some(None);
    };
    match sys::stat_at(dir, name) {
        Ok(st) if sys::file_type(&st) == FileType::Symlink => {
            let target = fs::statx(dir, name, AtFlags::empty(), StatxFlags::BASIC_STATS);
            match target {
                Ok(t) if t.stx_uid != euid => None,
                _ => Some(None),
            }
        }
        Ok(st) if st.stx_uid == euid => {
            // Unreadable: assume an ACL, keep only the owner's bits.
            let acl = fs::openat(dir, name, sys::READ | OFlags::NOFOLLOW, Mode::empty())
                .map_or(true, sys::has_acl);
            Some(Some((sys::copy_mode(&st, acl), st.stx_gid)))
        }
        Ok(_) => None,
        Err(Errno::NOENT) => Some(None),
        Err(_) => None,
    }
}

/// Writes `BASE`, or `BASE.1`, `BASE.2`, ... (the first that does not exist) in `dir`, with the
/// given mode and group, or `0600`; returns the open file and its name. It is not synced.
fn write_numbered(
    dir: &OwnedFd,
    base: &OsStr,
    perms: Option<(u32, u32)>,
    bytes: &[u8],
) -> Result<(OwnedFd, OsString)> {
    for n in 0..100_000u32 {
        let mut name = base.to_os_string();
        if n > 0 {
            name.push(format!(".{n}"));
        }
        let fd = match sys::create_new(dir, &name, 0o600) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(e),
        };
        let result: Result<()> = (|| {
            sys::write_all(&fd, bytes)?;
            // The original's mode only with its group, so no other group can read the copy.
            if let Some((mode, gid)) = perms {
                if fs::fchown(&fd, None, Some(Gid::from_raw(gid))).is_ok() {
                    fs::fchmod(&fd, Mode::from_raw_mode(mode))?;
                }
            }
            Ok(())
        })();
        return match result {
            Ok(()) => Ok((fd, name)),
            Err(e) => {
                let _ = fs::unlinkat(dir, &name, fs::AtFlags::empty());
                Err(e)
            }
        };
    }
    Err(Errno::EXIST)
}
