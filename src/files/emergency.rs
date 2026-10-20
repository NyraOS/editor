// SPDX-License-Identifier: MIT
//! Emergency copies, as GNU nano writes them when its terminal goes away (`SIGHUP`, such as a
//! closed SSH connection) or it is told to stop (`SIGTERM`): the text of a modified buffer goes to
//! `NAME.save`, or `NAME.save.1`, `NAME.save.2`, ... when that exists; a buffer without a name
//! goes to `nano.PID.save` in the current directory.
//!
//! Unlike nano, the copy of a file that belongs to another user (root editing a user's file) goes
//! to the home directory of the user running the editor, not next to the file: a directory
//! another user controls is where the emergency copy of nano went wrong in 2024 (CVE-2024-5742).
//! The copy also goes there when it cannot be written next to the file. It is always a new file
//! (`O_EXCL | O_NOFOLLOW`), with the permissions of the original file when it is the user's own,
//! and `0600` otherwise.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rustix::fs::{self, CWD, Gid, Mode};
use rustix::io::Errno;
use signal_hook::consts::{SIGHUP, SIGTERM};

use super::sys::{self, Result};

/// Makes `SIGHUP` and `SIGTERM` set the returned flag instead of ending the editor, which then
/// writes the emergency copies of its modified buffers ([`emergency_save`]) and exits. Waits for
/// keys must end at least every second or so to look at the flag (a closed terminal also ends them
/// with an error).
pub fn watch_signals() -> io::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    for signal in [SIGHUP, SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&flag))?;
    }
    Ok(flag)
}

/// Writes the emergency copy of a buffer (`path` is its file name, if it has one); returns where.
pub fn emergency_save(path: Option<&Path>, bytes: &[u8]) -> io::Result<PathBuf> {
    let (dir_path, base, beside) = match path.and_then(|p| Some((p, p.file_name()?))) {
        Some((path, name)) => {
            let mut base = name.to_os_string();
            base.push(".save");
            let dir = path.parent().unwrap_or(Path::new(""));
            (dir, base, own_file(path, dir))
        }
        None => {
            let base = format!("nano.{}.save", rustix::process::getpid().as_raw_nonzero());
            (Path::new(""), OsString::from(base), Some(None))
        }
    };
    // `None`: another user's file; `Some(perms)`: the user's own, with its mode and group.
    if let Some(perms) = beside {
        let written =
            sys::open_dir(dir_path).and_then(|dir| write_numbered(&dir, &base, perms, bytes));
        if let Ok(name) = written {
            return Ok(dir_path.join(name));
        }
    }
    let (home, home_path) = sys::home_dir()?;
    let name = write_numbered(&home, &base, None, bytes)?;
    Ok(home_path.join(name))
}

/// Whether the file (or, for a new file, its directory) belongs to the user running the editor;
/// if so, the mode and group for the copy (`None` for a new file: `0600`).
fn own_file(path: &Path, dir: &Path) -> Option<Option<(u32, u32)>> {
    let euid = sys::euid();
    match fs::statx(CWD, path, fs::AtFlags::empty(), fs::StatxFlags::BASIC_STATS) {
        Ok(st) => (st.stx_uid == euid).then_some(Some((sys::mode(&st) & 0o777, st.stx_gid))),
        Err(Errno::NOENT) => {
            let dir = sys::open_dir(dir).ok()?;
            let st = sys::stat(&dir).ok()?;
            (st.stx_uid == euid).then_some(None)
        }
        Err(_) => None,
    }
}

/// Writes `BASE`, or `BASE.1`, `BASE.2`, ... (the first that does not exist) in `dir`, with the
/// given mode and group, or `0600`.
fn write_numbered(
    dir: &OwnedFd,
    base: &OsStr,
    perms: Option<(u32, u32)>,
    bytes: &[u8],
) -> Result<OsString> {
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
        let result = (|| {
            sys::write_all(&fd, bytes)?;
            // The original's mode only with its group, so no other group can read the copy.
            if let Some((mode, gid)) = perms {
                if fs::fchown(&fd, None, Some(Gid::from_raw(gid))).is_ok() {
                    fs::fchmod(&fd, Mode::from_raw_mode(mode))?;
                }
            }
            fs::fsync(&fd)?;
            sys::sync_dir(dir)
        })();
        return match result {
            Ok(()) => Ok(name),
            Err(e) => {
                let _ = fs::unlinkat(dir, &name, fs::AtFlags::empty());
                Err(e)
            }
        };
    }
    Err(Errno::EXIST)
}
