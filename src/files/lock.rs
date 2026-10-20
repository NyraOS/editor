// SPDX-License-Identifier: MIT
//! Lock files, in the vim-style format of GNU nano (`--locking`), so nano, vim and the editor see
//! each other: `.NAME.swp` next to the file (next to the symlink, for a symlink), 1024 bytes:
//!
//! | Offset | Bytes | Contents |
//! |---|---|---|
//! | 0 | 2 | `b0` |
//! | 2 | 10 | the program and its version (`nano 8.4`) |
//! | 24 | 4 | the process ID, little-endian |
//! | 28 | 40 | the user name |
//! | 68 | 40 | the host name |
//! | 108 | 768 | the file name, as given |
//! | 1007 | 1 | `0x55` when the buffer is modified |
//!
//! Text fields end with a zero byte. A lock file is created new (`O_EXCL | O_NOFOLLOW`) with the
//! permissions of the file, never more than `0600` when the file belongs to another user.

use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rustix::fs::{self, AtFlags, CWD, FileType, Mode, OFlags};
use rustix::io::{self as rio, Errno};

use super::{split, sys};

const SIZE: usize = 1024;
const MODIFIED: u64 = 1007;

/// Who holds a lock file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockInfo {
    pub user: String,
    pub pid: u32,
    /// The program and its version, such as `nano 8.4`.
    pub program: String,
}

#[derive(Debug)]
pub enum Locking {
    /// The editor holds the lock now.
    Locked(Lock),
    /// Another editor holds it: nano asks "File NAME is being edited by USER (with PROGRAM, PID
    /// PID); open anyway?", and on yes locks again with `force`.
    Busy(LockInfo),
    /// A file is there that is not a lock file: nano says "Bad lock file is ignored: PATH" and
    /// edits without a lock, leaving that file alone.
    Bad(PathBuf),
}

/// A lock file the editor holds; removed when dropped, unless another editor took it over.
#[derive(Debug)]
pub struct Lock {
    dir: OwnedFd,
    name: OsString,
    fd: OwnedFd,
    path: PathBuf,
}

impl Lock {
    /// Where the lock file is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Marks the buffer as modified, or not, in the lock file (nano updates it at each change).
    pub fn set_modified(&self, modified: bool) -> io::Result<()> {
        let byte = if modified { 0x55 } else { 0 };
        rio::pwrite(&self.fd, &[byte], MODIFIED)?;
        Ok(())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let ours = sys::stat(&self.fd);
        let there = sys::stat_at(&self.dir, &self.name);
        if let (Ok(ours), Ok(there)) = (ours, there) {
            if sys::same_file(&ours, &there) {
                let _ = fs::unlinkat(&self.dir, &self.name, AtFlags::empty());
            }
        }
    }
}

/// Locks the file at `path` for editing; with `force`, replaces the lock file of another editor.
/// A directory where no file can be created gives an error (nano: "Directory 'DIR' is not
/// writable") and the file is edited without a lock.
pub fn lock(path: &Path, force: bool) -> io::Result<Locking> {
    let (dir_path, file_name) = split(path)?;
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(".swp");
    let dir_path = if dir_path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir_path
    };
    let lock_path = dir_path.join(&name);
    let dir = sys::open_dir(dir_path)?;
    let data = contents(path);
    let mode = mode_for(path);

    let fd = if force {
        let (fd, tmp) = sys::create_temp(&dir, &name, mode)?;
        let result =
            sys::write_all(&fd, &data).and_then(|()| fs::renameat(&dir, &tmp, &dir, &name));
        if let Err(e) = result {
            let _ = fs::unlinkat(&dir, &tmp, AtFlags::empty());
            return Err(e.into());
        }
        fd
    } else {
        match sys::create_new(&dir, &name, mode) {
            Ok(fd) => {
                if let Err(e) = sys::write_all(&fd, &data) {
                    let _ = fs::unlinkat(&dir, &name, AtFlags::empty());
                    return Err(e.into());
                }
                fd
            }
            Err(Errno::EXIST) => {
                return Ok(match read_existing(&dir, &name)? {
                    Some(info) => Locking::Busy(info),
                    None => Locking::Bad(lock_path),
                });
            }
            Err(e) => return Err(e.into()),
        }
    };
    Ok(Locking::Locked(Lock {
        dir,
        name,
        fd,
        path: lock_path,
    }))
}

/// The permissions of the file (only the owner's with an ACL) for its owner, `0600` for anyone else; `0666` (less the umask) for
/// a new file.
fn mode_for(path: &Path) -> u32 {
    match fs::statx(CWD, path, AtFlags::empty(), fs::StatxFlags::BASIC_STATS) {
        Ok(st) if st.stx_uid == sys::euid() => {
            let mut acl = [0u8; 0];
            let acl = !matches!(
                fs::getxattr(path, sys::ACL, &mut acl),
                Err(Errno::NODATA | Errno::NOTSUP)
            );
            sys::copy_mode(&st, acl) & 0o666
        }
        Ok(_) => 0o600,
        Err(_) => 0o666,
    }
}

fn contents(path: &Path) -> [u8; SIZE] {
    let mut data = [0u8; SIZE];
    let put = |data: &mut [u8; SIZE], at: usize, size: usize, value: &[u8]| {
        // One byte stays zero: the end of the text.
        let n = value.len().min(size - 1);
        data[at..at + n].copy_from_slice(&value[..n]);
    };
    put(&mut data, 0, 3, b"b0");
    let program = format!("editor {}", env!("CARGO_PKG_VERSION"));
    let n = program.len().min(10);
    data[2..2 + n].copy_from_slice(&program.as_bytes()[..n]);
    let pid = rustix::process::getpid().as_raw_nonzero().get();
    data[24..28].copy_from_slice(&pid.to_le_bytes());
    let user = sys::passwd(sys::euid()).map(|(name, _)| name.into_vec());
    let user = user.unwrap_or_else(|| sys::euid().to_string().into_bytes());
    put(&mut data, 28, 40, &user);
    let host = rustix::system::uname();
    put(&mut data, 68, 40, host.nodename().to_bytes());
    put(&mut data, 108, 768, path.as_os_str().as_bytes());
    data
}

/// Reads a lock file that exists; `None` when it is not one.
fn read_existing(dir: &OwnedFd, name: &OsString) -> io::Result<Option<LockInfo>> {
    let fd = match fs::openat(dir, name, sys::READ | OFlags::NOFOLLOW, Mode::empty()) {
        Ok(fd) => fd,
        // A symlink.
        Err(Errno::LOOP) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if sys::file_type(&sys::stat(&fd)?) != FileType::RegularFile {
        return Ok(None);
    }
    let mut data = [0u8; SIZE];
    let mut len = 0;
    while len < SIZE {
        match rio::pread(&fd, &mut data[len..], len as u64) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if len < SIZE || !data.starts_with(b"b0") {
        return Ok(None);
    }
    // Shown on the screen: control characters (C0, DEL, C1, such as escape sequences) and the
    // bidirectional embeddings, overrides and isolates (which reorder the text shown) become `?`.
    let text = |field: &[u8]| {
        let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
        let text = String::from_utf8_lossy(&field[..end]);
        text.chars()
            .map(|c| match c {
                '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '?',
                c if c.is_control() => '?',
                c => c,
            })
            .collect()
    };
    Ok(Some(LockInfo {
        program: text(&data[2..12]),
        pid: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
        user: text(&data[28..68]),
    }))
}
