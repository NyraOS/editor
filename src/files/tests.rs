// SPDX-License-Identifier: MIT
//! The safe-saving tests: every save runs in a child process (this test binary again, running only
//! the `child` test), so it can be killed with `SIGKILL` at each step of a save, get `SIGHUP` from
//! a closed terminal, and run as another user.
//!
//! As root, each test runs twice: as root, and as an ordinary user (uid 4242); the tests that need
//! root to set up (another user's files, bind mounts, a full file system) also run only as root.
//! As an ordinary user, those are skipped with a note, and the rest run as that user. CI runs them
//! both ways.

use std::ffi::OsStr;
use std::fs::{self as stdfs, File};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustix::fs::{XattrFlags, getxattr, listxattr, setxattr};
use rustix::process::{Signal, geteuid, getpid, kill_process};

use super::*;

const USER: u32 = 4242;
const OTHER: u32 = 4343;
const OLD: u8 = b'o';
const NEW: u8 = b'n';

// ---------------------------------------------------------------------------------------------
// The child side.

static STEPS: AtomicUsize = AtomicUsize::new(0);

/// Called at each step of a save: the child kills itself at step `FILES_TEST_KILL_AT`.
pub(super) fn step(what: &str) {
    let Some(kill_at) = std::env::var("FILES_TEST_KILL_AT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    if STEPS.fetch_add(1, Ordering::SeqCst) + 1 == kill_at {
        println!("KILLED: {what}");
        let _ = std::io::stdout().flush();
        let _ = kill_process(getpid(), Signal::KILL);
    }
}

/// Not a test: the body of the child processes, which does nothing without `FILES_TEST_OP`.
#[test]
fn child() {
    let Ok(op) = std::env::var("FILES_TEST_OP") else {
        return;
    };
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    if let Ok(mask) = std::env::var("FILES_TEST_UMASK") {
        let mask = u32::from_str_radix(&mask, 8).unwrap();
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(mask));
    }
    let path = env("FILES_TEST_PATH");
    let input = env("FILES_TEST_INPUT").map(|p| stdfs::read(p).unwrap());
    let result = match op.as_str() {
        "save" => {
            let mut opts = SaveOptions::default();
            for opt in std::env::var("FILES_TEST_OPTS").unwrap().split(',') {
                match opt.split_once('=') {
                    Some(("backup_dir", dir)) => opts.backup_dir = Some(dir.into()),
                    _ if opt == "backup" => opts.backup = true,
                    _ if opt == "in_place" => opts.in_place = true,
                    _ => {}
                }
            }
            let path = path.as_deref().unwrap();
            // What the editor read: given by the test, or the file there now.
            let expected = match std::env::var("FILES_TEST_EXPECT").as_deref() {
                Ok("absent") => Expected::Absent,
                Ok(id) => {
                    let n: Vec<u64> = id.split(':').map(|n| n.parse().unwrap()).collect();
                    let mut meta = super::stat(Path::new("/")).unwrap().unwrap();
                    meta.dev = (n[0] as u32, n[1] as u32);
                    meta.ino = n[2];
                    Expected::File(meta)
                }
                // A path that cannot be looked at: the save reports why.
                Err(_) => match super::stat(path).ok().flatten() {
                    Some(meta) => Expected::File(meta),
                    None => Expected::Absent,
                },
            };
            match save(path, &input.unwrap(), expected, &opts) {
                Ok(saved) => {
                    let method = match saved.method {
                        Method::Replaced => "replaced",
                        Method::InPlace => "in place",
                    };
                    match saved.backup {
                        Some(b) => format!("{method}, backup {}", b.display()),
                        None => method.to_string(),
                    }
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "emergency" => describe(emergency_save(path.as_deref(), &input.unwrap())),
        "lock" => match lock(path.as_deref().unwrap(), false) {
            Ok(Locking::Locked(lock)) => format!("locked {}", lock.path().display()),
            Ok(other) => format!("{other:?}"),
            Err(e) => format!("error: {e}"),
        },
        "signal" => {
            // On the terminal given as standard input: make it the controlling terminal of a new
            // session, as a login shell's, so closing it sends SIGHUP.
            if std::env::var_os("FILES_TEST_SETSID").is_some() {
                rustix::process::setsid().unwrap();
                rustix::process::ioctl_tiocsctty(std::io::stdin()).unwrap();
            }
            let flag = watch_signals().unwrap();
            println!("READY");
            let _ = std::io::stdout().flush();
            let start = Instant::now();
            while !flag.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let saved = emergency_save(path.as_deref(), &input.unwrap());
            // The terminal is gone: the result goes to a file.
            let out = env("FILES_TEST_RESULT").unwrap();
            stdfs::write(out, describe(saved)).unwrap();
            // Nothing more to print: the terminal may be gone.
            std::process::exit(0);
        }
        _ => panic!("unknown operation {op}"),
    };
    println!("RESULT: {result}");
}

fn describe(r: std::io::Result<PathBuf>) -> String {
    match r {
        Ok(p) => p.display().to_string(),
        Err(e) => format!("error: {e}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The parent side.

fn is_root() -> bool {
    geteuid().is_root()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Who {
    /// The user running the tests (root or not).
    Me,
    /// An ordinary user, when the tests run as root.
    User,
}

impl Who {
    fn uid(self) -> u32 {
        match self {
            Who::Me => geteuid().as_raw(),
            Who::User => USER,
        }
    }
}

/// Root: root and an ordinary user. Otherwise: the user running the tests.
fn everyone() -> Vec<Who> {
    if is_root() {
        vec![Who::Me, Who::User]
    } else {
        vec![Who::Me]
    }
}

fn needs_root(test: &str) -> bool {
    if !is_root() {
        println!("{test}: skipped, needs root (CI runs it as root)");
    }
    is_root()
}

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

/// A directory for one test: `dir` belongs to the user the save runs as, `home` is that user's
/// home directory; everything is removed at the end, after unmounting what the test mounted.
struct Scratch {
    base: PathBuf,
    dir: PathBuf,
    home: PathBuf,
    who: Who,
    mounts: std::cell::RefCell<Vec<PathBuf>>,
}

impl Scratch {
    fn new(who: Who) -> Self {
        let n = SCRATCH.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!(
            "nyra-editor-files-{}-{n}",
            getpid().as_raw_nonzero()
        ));
        let _ = stdfs::remove_dir_all(&base);
        stdfs::create_dir(&base).unwrap();
        set_mode(&base, 0o755);
        let s = Scratch {
            dir: base.join("d"),
            home: base.join("home"),
            base,
            who,
            mounts: Default::default(),
        };
        s.mkdir(&s.dir, 0o755, who.uid());
        s.mkdir(&s.home, 0o700, who.uid());
        s
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn mkdir(&self, path: &Path, mode: u32, uid: u32) {
        stdfs::create_dir(path).unwrap();
        own(path, uid);
        set_mode(path, mode);
    }

    /// A file owned by `uid` (and its group).
    fn file(&self, path: &Path, content: &[u8], mode: u32, uid: u32) {
        stdfs::write(path, content).unwrap();
        // The owner first: chown clears the set-user-ID bit.
        own(path, uid);
        set_mode(path, mode);
    }

    fn mount_tmpfs(&self, path: &Path, size: &std::ffi::CStr) {
        rustix::mount::mount(
            "tmpfs",
            path,
            "tmpfs",
            rustix::mount::MountFlags::empty(),
            Some(size),
        )
        .unwrap();
        self.mounts.borrow_mut().push(path.to_path_buf());
    }

    fn bind(&self, source: &Path, target: &Path) {
        rustix::mount::mount_bind(source, target).unwrap();
        self.mounts.borrow_mut().push(target.to_path_buf());
    }

    /// Runs an operation in a child process as `self.who`.
    fn run(&self, op: &str, path: Option<&Path>, input: &[u8], opts: &str) -> String {
        self.run_expecting(op, path, input, opts, None)
    }

    /// Runs a save that expects `expect` at the path (`absent`, or `MAJOR:MINOR:INODE`).
    fn run_expecting(
        &self,
        op: &str,
        path: Option<&Path>,
        input: &[u8],
        opts: &str,
        expect: Option<&str>,
    ) -> String {
        let expect = expect.map(|e| ("FILES_TEST_EXPECT", e.to_string()));
        let out = self.run_with(op, path, input, opts, expect.as_slice());
        assert!(out.killed.is_none());
        out.result.expect("the child gave no result")
    }

    fn run_with(
        &self,
        op: &str,
        path: Option<&Path>,
        input: &[u8],
        opts: &str,
        env: &[(&str, String)],
    ) -> Outcome {
        let input_path = self.base.join("input");
        stdfs::write(&input_path, input).unwrap();
        set_mode(&input_path, 0o644);
        let mut cmd = self.command(op, path, &input_path);
        cmd.env("FILES_TEST_OPTS", opts);
        // FILES_TEST_KILL_AT, FILES_TEST_UMASK, FILES_TEST_EXPECT.
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        let output = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = |prefix: &str| {
            stdout
                .lines()
                .find_map(|l| l.split_once(prefix).map(|(_, rest)| rest.to_string()))
        };
        let killed = line("KILLED: ");
        if killed.is_none() && !output.status.success() {
            panic!(
                "child failed: {}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Outcome {
            killed,
            result: line("RESULT: "),
        }
    }

    fn command(&self, op: &str, path: Option<&Path>, input: &Path) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "files::tests::child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FILES_TEST_OP", op)
        .env("FILES_TEST_INPUT", input)
        .env("HOME", &self.home)
        .env_remove("FILES_TEST_KILL_AT")
        .current_dir(&self.dir);
        if let Some(path) = path {
            cmd.env("FILES_TEST_PATH", path);
        }
        if self.who == Who::User {
            std::os::unix::process::CommandExt::uid(&mut cmd, USER);
            std::os::unix::process::CommandExt::gid(&mut cmd, USER);
        }
        cmd
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for m in self.mounts.borrow().iter().rev() {
            let _ = rustix::mount::unmount(m, rustix::mount::UnmountFlags::DETACH);
        }
        // Directories the test made read-only.
        let _ = Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(&self.base)
            .status();
        let _ = stdfs::remove_dir_all(&self.base);
    }
}

fn kill_at(n: usize) -> (&'static str, String) {
    ("FILES_TEST_KILL_AT", n.to_string())
}

struct Outcome {
    /// The step the child was killed at.
    killed: Option<String>,
    result: Option<String>,
}

/// `.NAME.` and 16 hexadecimal digits and `.tmp`.
fn random_temp(name: &str, file: &str) -> bool {
    name.strip_prefix(&format!(".{file}."))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|hex| hex.len() == 16 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

fn set_mode(path: &Path, mode: u32) {
    stdfs::set_permissions(path, stdfs::Permissions::from_mode(mode)).unwrap();
}

/// Gives a file to a user (and the group of the same number), when running as root.
fn own(path: &Path, uid: u32) {
    if is_root() {
        std::os::unix::fs::lchown(path, Some(uid), Some(uid)).unwrap();
    } else {
        assert_eq!(uid, geteuid().as_raw());
    }
}

fn content(byte: u8, len: usize) -> Vec<u8> {
    vec![byte; len]
}

fn old() -> Vec<u8> {
    content(OLD, 10_000)
}

fn new() -> Vec<u8> {
    content(NEW, 9_000)
}

fn read(path: &Path) -> Vec<u8> {
    stdfs::read(path).unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = stdfs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn meta(path: &Path) -> stdfs::Metadata {
    stdfs::symlink_metadata(path).unwrap()
}

fn xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let mut buf = [0u8; 4096];
    getxattr(path, name, &mut buf)
        .ok()
        .map(|n| buf[..n].to_vec())
}

fn xattr_names(path: &Path) -> Vec<String> {
    let mut buf = [0u8; 4096];
    let n = listxattr(path, &mut buf).unwrap_or(0);
    buf[..n]
        .split(|&b| b == 0)
        .filter(|n| !n.is_empty())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect()
}

/// A POSIX ACL as the kernel stores it in `system.posix_acl_*`: (tag, permissions, id).
fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut v = 2u32.to_le_bytes().to_vec();
    for (tag, perm, id) in entries {
        v.extend(tag.to_le_bytes());
        v.extend(perm.to_le_bytes());
        v.extend(id.to_le_bytes());
    }
    v
}

const ACL: &str = "system.posix_acl_access";
const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;
const NO_ID: u32 = u32::MAX;

/// Extended attributes or ACLs are not supported where the tests run.
fn unsupported(e: rustix::io::Errno, test: &str) -> bool {
    if e == rustix::io::Errno::NOTSUP {
        println!("{test}: skipped, the file system has no extended attributes or ACLs");
        return true;
    }
    false
}

// ---------------------------------------------------------------------------------------------
// SIGKILL at every step of a save.

/// Kills the save at step 1, 2, ... until it completes; after each kill, `check` sees the
/// directory. Returns the number of steps.
fn kill_at_every_step(
    who: Who,
    opts: &str,
    setup: impl Fn(&Scratch) -> PathBuf,
    check: impl Fn(&Scratch, &Path, &str),
) -> usize {
    for n in 1.. {
        let s = Scratch::new(who);
        let path = setup(&s);
        let out = s.run_with("save", Some(&path), &new(), opts, &[kill_at(n)]);
        match out.killed {
            Some(step) => {
                println!("{who:?}: killed at step {n}: {step}");
                check(&s, &path, &step);
            }
            None => {
                assert!(!out.result.unwrap().starts_with("error"), "{who:?}");
                assert_eq!(read(&path), new(), "{who:?}: the save completed");
                return n - 1;
            }
        }
    }
    unreachable!()
}

#[test]
fn sigkill_replace_leaves_old_or_new_whole() {
    for who in everyone() {
        let steps = kill_at_every_step(
            who,
            "",
            |s| {
                let path = s.path("f");
                s.file(&path, &old(), 0o640, who.uid());
                path
            },
            |s, path, step| {
                let got = read(path);
                assert!(
                    got == old() || got == new(),
                    "{who:?}: half a file after {step}"
                );
                // The new file has the original's metadata before it takes the name.
                let m = meta(path);
                assert_eq!(m.mode() & 0o7777, 0o640, "{who:?}: mode after {step}");
                assert_eq!(m.uid(), who.uid());
                // Only the file and, killed before the rename, a hidden temporary file with a
                // random name.
                for name in names(&s.dir) {
                    assert!(
                        name == "f" || random_temp(&name, "f"),
                        "{name} after {step}"
                    );
                }
            },
        );
        // Created, chunks of the temporary file, metadata, synced, renamed, directory synced.
        assert!(steps >= 7, "{who:?}: only {steps} steps");
    }
}

#[test]
fn sigkill_new_file_leaves_nothing_or_new_whole() {
    for who in everyone() {
        kill_at_every_step(
            who,
            "",
            |s| s.path("new"),
            |_, path, step| {
                if path.exists() {
                    assert_eq!(read(path), new(), "{who:?}: half a new file after {step}");
                }
            },
        );
    }
}

#[test]
fn sigkill_in_place_keeps_a_whole_copy() {
    // A hard link makes the save write in place.
    for who in everyone() {
        let steps = kill_at_every_step(
            who,
            "",
            |s| {
                let path = s.path("f");
                s.file(&path, &old(), 0o644, who.uid());
                stdfs::hard_link(&path, s.path("link")).unwrap();
                path
            },
            |s, path, step| {
                assert_eq!(meta(path).ino(), meta(&s.path("link")).ino());
                let got = read(path);
                if got != old() && got != new() {
                    // Killed in the middle of the write: the old contents are in the copy.
                    let copies: Vec<_> = names(&s.dir)
                        .into_iter()
                        .filter(|n| n.starts_with(".f."))
                        .collect();
                    assert_eq!(copies.len(), 1, "{who:?}: no copy after {step}");
                    assert_eq!(read(&s.path(&copies[0])), old(), "{who:?}: after {step}");
                }
            },
        );
        assert!(steps >= 7, "{who:?}: only {steps} steps");
    }
}

#[test]
fn sigkill_with_backup_keeps_old_contents_somewhere() {
    for who in everyone() {
        kill_at_every_step(
            who,
            "backup",
            |s| {
                let path = s.path("f");
                s.file(&path, &old(), 0o600, who.uid());
                path
            },
            |s, path, step| {
                let got = read(path);
                assert!(got == old() || got == new(), "{who:?}: after {step}");
                let backup = s.path("f~");
                if got == new() || backup.exists() {
                    assert_eq!(read(&backup), old(), "{who:?}: backup after {step}");
                }
            },
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Saving: metadata, links, mounts, permissions.

#[test]
fn replace_keeps_owner_mode_and_extended_attributes() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, &old(), 0o751, who.uid());
        let mut skip_xattrs = false;
        if let Err(e) = setxattr(&path, "user.note", b"kept", XattrFlags::empty()) {
            skip_xattrs = unsupported(e, "user extended attributes");
        }
        let result = s.run("save", Some(&path), &new(), "");
        assert_eq!(result, "replaced", "{who:?}");
        assert_eq!(read(&path), new());
        let m = meta(&path);
        assert_eq!(
            (m.mode() & 0o7777, m.uid(), m.gid()),
            (0o751, who.uid(), who.uid())
        );
        if !skip_xattrs {
            assert_eq!(xattr(&path, "user.note").as_deref(), Some(&b"kept"[..]));
        }
        assert_eq!(names(&s.dir), ["f"]);
    }
}

#[test]
fn replace_as_root_keeps_set_user_id_and_trusted_attributes() {
    if !needs_root("replace_as_root_keeps_set_user_id_and_trusted_attributes") {
        return;
    }
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, &old(), 0o4755, USER);
    let trusted = setxattr(&path, "trusted.note", b"kept", XattrFlags::empty());
    assert_eq!(s.run("save", Some(&path), &new(), ""), "replaced");
    let m = meta(&path);
    assert_eq!((m.mode() & 0o7777, m.uid(), m.gid()), (0o4755, USER, USER));
    if trusted.is_ok() {
        assert_eq!(xattr(&path, "trusted.note").as_deref(), Some(&b"kept"[..]));
    }
}

#[test]
fn replace_keeps_the_acl_and_drops_an_inherited_one() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, &old(), 0o640, who.uid());
        let access = acl(&[
            (ACL_USER_OBJ, 6, NO_ID),
            (ACL_USER, 6, OTHER),
            (ACL_GROUP_OBJ, 4, NO_ID),
            (ACL_MASK, 6, NO_ID),
            (ACL_OTHER, 0, NO_ID),
        ]);
        if let Err(e) = setxattr(
            &path,
            "system.posix_acl_access",
            &access,
            XattrFlags::empty(),
        ) {
            if unsupported(e, "ACLs") {
                return;
            }
            panic!("{e}");
        }
        // A plain file in a directory with a default ACL: its new version must not inherit it.
        let plain = s.path("plain");
        s.file(&plain, &old(), 0o644, who.uid());
        let default = acl(&[
            (ACL_USER_OBJ, 7, NO_ID),
            (ACL_USER, 7, OTHER),
            (ACL_GROUP_OBJ, 5, NO_ID),
            (ACL_MASK, 7, NO_ID),
            (ACL_OTHER, 5, NO_ID),
        ]);
        setxattr(
            &s.dir,
            "system.posix_acl_default",
            &default,
            XattrFlags::empty(),
        )
        .unwrap();
        let mode_before = meta(&path).mode();

        assert_eq!(
            s.run("save", Some(&path), &new(), ""),
            "replaced",
            "{who:?}"
        );
        assert_eq!(xattr(&path, "system.posix_acl_access"), Some(access));
        assert_eq!(meta(&path).mode(), mode_before);

        assert_eq!(
            s.run("save", Some(&plain), &new(), ""),
            "replaced",
            "{who:?}"
        );
        assert_eq!(read(&plain), new());
        assert!(!xattr_names(&plain).contains(&"system.posix_acl_access".to_string()));
        assert_eq!(meta(&plain).mode() & 0o7777, 0o644);
    }
}

#[test]
fn hard_links_are_written_in_place() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, &old(), 0o644, who.uid());
        stdfs::hard_link(&path, s.path("link")).unwrap();
        let ino = meta(&path).ino();
        assert_eq!(
            s.run("save", Some(&path), &new(), ""),
            "in place",
            "{who:?}"
        );
        assert_eq!(read(&s.path("link")), new());
        assert_eq!((meta(&path).ino(), meta(&path).nlink()), (ino, 2));
        // The copy made before writing is gone.
        assert_eq!(names(&s.dir), ["f", "link"]);
    }
}

#[test]
fn in_place_option_and_a_longer_or_shorter_file() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, &old(), 0o644, who.uid());
        let ino = meta(&path).ino();
        assert_eq!(s.run("save", Some(&path), &new(), "in_place"), "in place");
        assert_eq!(read(&path), new());
        let long = content(b'l', 50_000);
        assert_eq!(s.run("save", Some(&path), &long, "in_place"), "in place");
        assert_eq!(read(&path), long);
        assert_eq!(s.run("save", Some(&path), b"", "in_place"), "in place");
        assert_eq!(read(&path), b"");
        assert_eq!(meta(&path).ino(), ino);
    }
}

#[test]
fn bind_mounted_files_are_written_in_place() {
    if !needs_root("bind_mounted_files_are_written_in_place") {
        return;
    }
    for who in everyone() {
        let s = Scratch::new(who);
        let source = s.base.join("resolv.conf");
        s.file(&source, &old(), 0o644, who.uid());
        let path = s.path("resolv.conf");
        s.file(&path, b"under the mount", 0o644, who.uid());
        s.bind(&source, &path);
        let ino = meta(&source).ino();
        assert_eq!(
            s.run("save", Some(&path), &new(), ""),
            "in place",
            "{who:?}"
        );
        assert_eq!(read(&source), new());
        assert_eq!(meta(&path).ino(), ino, "{who:?}: still mounted");
        assert_eq!(names(&s.dir), ["resolv.conf"]);
    }
}

#[test]
fn symlinks_are_followed_and_stay_symlinks() {
    for who in everyone() {
        let s = Scratch::new(who);
        let sub = s.path("sub");
        s.mkdir(&sub, 0o755, who.uid());
        let target = sub.join("target");
        s.file(&target, &old(), 0o640, who.uid());
        let link = s.path("link");
        std::os::unix::fs::symlink("sub/target", &link).unwrap();
        own(&link, who.uid());
        let chain = s.path("chain");
        std::os::unix::fs::symlink(&link, &chain).unwrap();
        own(&chain, who.uid());

        assert_eq!(
            s.run("save", Some(&link), &new(), ""),
            "replaced",
            "{who:?}"
        );
        assert_eq!(read(&target), new());
        assert_eq!(stdfs::read_link(&link).unwrap(), Path::new("sub/target"));
        assert_eq!(meta(&target).mode() & 0o777, 0o640);

        let again = content(b'2', 100);
        assert_eq!(s.run("save", Some(&chain), &again, ""), "replaced");
        assert_eq!(read(&target), again);
        assert!(meta(&chain).file_type().is_symlink() && meta(&link).file_type().is_symlink());

        // A symlink to a file that does not exist yet: the file is created.
        let dangling = s.path("dangling");
        std::os::unix::fs::symlink("sub/created", &dangling).unwrap();
        own(&dangling, who.uid());
        assert_eq!(s.run("save", Some(&dangling), &new(), ""), "replaced");
        assert_eq!(read(&sub.join("created")), new());
        assert!(meta(&dangling).file_type().is_symlink());

        // A loop.
        std::os::unix::fs::symlink("loop", s.path("loop")).unwrap();
        let result = s.run("save", Some(&s.path("loop")), &new(), "");
        assert!(result.contains("os error 40"), "{result}");
    }
}

#[test]
fn another_users_symlink_in_a_sticky_directory_is_not_followed() {
    if !needs_root("another_users_symlink_in_a_sticky_directory_is_not_followed") {
        return;
    }
    // As /tmp: a user's symlink there must not send root's save to a file of root's choice.
    let s = Scratch::new(Who::Me);
    let tmp = s.path("tmp");
    s.mkdir(&tmp, 0o1777, 0);
    let secret = s.path("secret");
    s.file(&secret, &old(), 0o600, 0);
    let link = tmp.join("link");
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    own(&link, USER);
    let result = s.run("save", Some(&link), &new(), "");
    assert!(result.contains("os error 13"), "{result}");
    assert_eq!(read(&secret), old());
    // Root's own symlink there is followed.
    let mine = tmp.join("mine");
    std::os::unix::fs::symlink(&secret, &mine).unwrap();
    assert_eq!(s.run("save", Some(&mine), &new(), ""), "replaced");
    assert_eq!(read(&secret), new());
}

#[test]
fn a_directory_without_write_permission() {
    for who in everyone() {
        if is_root() && who == Who::Me {
            // Root writes in any directory.
            continue;
        }
        let s = Scratch::new(who);
        let ro = s.path("ro");
        s.mkdir(&ro, 0o755, who.uid());
        let path = ro.join("f");
        s.file(&path, &old(), 0o644, who.uid());
        let readonly = ro.join("readonly");
        s.file(&readonly, &old(), 0o444, who.uid());
        set_mode(&ro, 0o555);
        let ino = meta(&path).ino();
        assert_eq!(
            s.run("save", Some(&path), &new(), ""),
            "in place",
            "{who:?}"
        );
        assert_eq!((read(&path), meta(&path).ino()), (new(), ino));
        // Nothing can be written: an error, and the file is untouched.
        let result = s.run("save", Some(&readonly), &new(), "");
        assert!(result.contains("os error 13"), "{result}");
        assert_eq!(read(&readonly), old());
        // Neither can a new file be created there, nor a backup.
        let result = s.run("save", Some(&ro.join("new")), &new(), "");
        assert!(result.contains("os error 13"), "{result}");
        let result = s.run("save", Some(&path), &old(), "backup");
        assert!(result.starts_with("error: Cannot make backup"), "{result}");
        assert_eq!(read(&path), new());
        assert_eq!(names(&ro), ["f", "readonly"]);
    }
}

#[test]
fn another_users_file() {
    if !needs_root("another_users_file") {
        return;
    }
    // Root saves a user's file: replaced, with the user's owner, group and mode.
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, &old(), 0o640, USER);
    assert_eq!(s.run("save", Some(&path), &new(), ""), "replaced");
    let m = meta(&path);
    assert_eq!((m.uid(), m.gid(), m.mode() & 0o7777), (USER, USER, 0o640));

    // A user saves a file of another user they may write: a new file would be theirs, so the
    // save writes in place, and the owner stays.
    let s = Scratch::new(Who::User);
    let shared = s.base.join("shared");
    s.mkdir(&shared, 0o777, OTHER);
    let path = shared.join("f");
    s.file(&path, &old(), 0o666, OTHER);
    let ino = meta(&path).ino();
    assert_eq!(
        s.run("save", Some(&path), &new(), "backup"),
        format!("in place, backup {}~", path.display())
    );
    let m = meta(&path);
    assert_eq!((m.uid(), m.ino(), read(&path)), (OTHER, ino, new()));
    // The backup belongs to the user, and only the user can read it.
    let backup = shared.join("f~");
    assert_eq!(
        (meta(&backup).uid(), meta(&backup).mode() & 0o777),
        (USER, 0o600)
    );
    assert_eq!(names(&shared), ["f", "f~"]);
}

#[test]
fn a_full_file_system() {
    if !needs_root("a_full_file_system") {
        return;
    }
    for who in everyone() {
        let s = Scratch::new(who);
        let small = s.path("small");
        s.mkdir(&small, 0o755, who.uid());
        s.mount_tmpfs(&small, c"size=256k,mode=0755");
        own(&small, who.uid());
        let path = small.join("f");
        s.file(&path, &content(OLD, 8192), 0o644, who.uid());
        let filler = small.join("filler");
        let fill = |leave: u64| {
            let mut f = File::create(&filler).unwrap();
            while f.write_all(&[0u8; 4096]).is_ok() {}
            let len = f.metadata().unwrap().len();
            f.set_len(len.saturating_sub(leave)).unwrap();
        };
        let big = content(NEW, 64 * 1024);

        // Replacing: the new file does not fit.
        fill(0);
        let result = s.run("save", Some(&path), &big, "");
        assert!(result.contains("os error 28"), "{who:?}: {result}");
        assert_eq!(read(&path), content(OLD, 8192));
        assert_eq!(names(&small), ["f", "filler"]);

        // In place (a hard link): the copy fits, the new contents do not.
        stdfs::hard_link(&path, small.join("link")).unwrap();
        fill(12 * 1024);
        let result = s.run("save", Some(&path), &big, "");
        assert!(result.contains("os error 28"), "{who:?}: {result}");
        assert_eq!(read(&path), content(OLD, 8192));
        assert_eq!(names(&small), ["f", "filler", "link"]);

        // In place, not even the copy fits.
        fill(0);
        let result = s.run("save", Some(&path), &big, "");
        assert!(result.contains("os error 28"), "{who:?}: {result}");
        assert_eq!(read(&path), content(OLD, 8192));
        assert_eq!(names(&small), ["f", "filler", "link"]);

        // With room again, it saves.
        stdfs::remove_file(&filler).unwrap();
        assert_eq!(s.run("save", Some(&path), &big, ""), "in place");
        assert_eq!(read(&small.join("link")), big);
    }
}

#[test]
fn new_files_follow_the_umask() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("new");
        let out = s.run_with(
            "save",
            Some(&path),
            &new(),
            "",
            &[("FILES_TEST_UMASK", "027".into())],
        );
        assert_eq!(out.result.as_deref(), Some("replaced"));
        let m = meta(&path);
        assert_eq!(
            (m.mode() & 0o7777, m.uid(), read(&path)),
            (0o640, who.uid(), new())
        );
        assert_eq!(names(&s.dir), ["new"]);
    }
}

#[test]
fn only_regular_files_are_saved_and_read() {
    let s = Scratch::new(Who::Me);
    let fifo = s.path("fifo");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
    )
    .unwrap();
    let result = s.run("save", Some(&fifo), &new(), "");
    assert!(result.contains("is not a normal file"), "{result}");
    let e = read_file(&fifo).unwrap_err();
    assert_eq!(e.to_string(), "is not a normal file");
    let e = read_file(Path::new("/dev/null")).unwrap_err();
    assert_eq!(e.to_string(), "is a device file");
    let e = read_file(&s.dir).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::IsADirectory);
    let result = s.run("save", Some(&s.dir), &new(), "");
    assert!(result.contains("is a directory"), "{result}");
    let result = s.run("save", Some(Path::new("d/")), &new(), "");
    assert!(result.contains("os error 21"), "{result}");
    let e = read_file(&s.path("missing")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
}

fn read_file(path: &Path) -> std::io::Result<Loaded> {
    super::read(path)
}

#[test]
fn read_gives_bytes_and_notices_changes() {
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, b"one\r\ntwo\xff", 0o644, geteuid().as_raw());
    let loaded = read_file(&path).unwrap();
    assert_eq!(loaded.bytes, b"one\r\ntwo\xff");
    let now = super::stat(&path).unwrap().unwrap();
    assert!(!now.changed_since(&loaded.meta));
    stdfs::write(&path, b"changed").unwrap();
    let now = super::stat(&path).unwrap().unwrap();
    assert!(now.changed_since(&loaded.meta));
    assert_eq!(super::stat(&s.path("missing")).unwrap(), None);
}

// ---------------------------------------------------------------------------------------------
// Backups.

#[test]
fn backups_next_to_the_file() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, b"first", 0o640, who.uid());
        let mtime = meta(&path).mtime();
        // A symlink planted as the backup's name is replaced, not written through.
        let victim = s.base.join("victim");
        s.file(&victim, b"victim", 0o666, who.uid());
        std::os::unix::fs::symlink(&victim, s.path("f~")).unwrap();
        own(&s.path("f~"), who.uid());

        let result = s.run("save", Some(&path), b"second", "backup");
        assert_eq!(
            result,
            format!("replaced, backup {}~", path.display()),
            "{who:?}"
        );
        let backup = s.path("f~");
        assert_eq!(read(&backup), b"first");
        let m = meta(&backup);
        assert!(m.file_type().is_file());
        assert_eq!(
            (m.mode() & 0o7777, m.uid(), m.mtime()),
            (0o640, who.uid(), mtime)
        );
        assert_eq!(read(&victim), b"victim");

        s.run("save", Some(&path), b"third", "backup");
        assert_eq!(
            (read(&backup), read(&path)),
            (b"second".to_vec(), b"third".to_vec())
        );
        assert_eq!(names(&s.dir), ["f", "f~"]);
    }
}

#[test]
fn numbered_backups_in_a_backup_directory() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, b"1", 0o600, who.uid());
        let dir = s.base.join("backups");
        s.mkdir(&dir, 0o700, who.uid());
        let opts = format!("backup,backup_dir={}", dir.display());
        for n in 2..=4 {
            s.run("save", Some(&path), n.to_string().as_bytes(), &opts);
        }
        let flat = stdfs::canonicalize(&path)
            .unwrap()
            .display()
            .to_string()
            .replace('/', "!");
        let expected = [
            format!("{flat}~"),
            format!("{flat}~.1"),
            format!("{flat}~.2"),
        ];
        assert_eq!(names(&dir), expected, "{who:?}");
        for (name, contents) in expected.iter().zip(["1", "2", "3"]) {
            assert_eq!(read(&dir.join(name)), contents.as_bytes());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Emergency copies.

/// Runs the child that waits for a signal; `hangup` closes its terminal, otherwise it gets
/// SIGTERM. Returns what the child wrote: the path of the copy, or an error.
fn emergency_on_signal(s: &Scratch, path: Option<&Path>, hangup: bool) -> String {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = File::options()
        .read(true)
        .write(true)
        .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32)
        .open(OsStr::from_bytes(name.as_bytes()))
        .unwrap();
    let input = s.base.join("input");
    stdfs::write(&input, b"unsaved text").unwrap();
    set_mode(&input, 0o644);
    let result = s.home.join("result");
    let mut cmd = s.command("signal", path, &input);
    cmd.env("FILES_TEST_RESULT", &result)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    if hangup {
        cmd.env("FILES_TEST_SETSID", "1");
    }
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    let mut master = File::from(master);
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("READY") {
        let mut buf = [0u8; 1024];
        let n = master.read(&mut buf).unwrap();
        assert!(n > 0, "the child ended early");
        seen.extend_from_slice(&buf[..n]);
    }
    if hangup {
        // The SSH connection closes: the terminal goes away.
        drop(master);
    } else {
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        kill_process(pid, Signal::TERM).unwrap();
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    stdfs::read_to_string(&result).unwrap()
}

#[test]
fn sighup_writes_the_emergency_copy_next_to_the_users_own_file() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, b"saved text", 0o640, who.uid());
        let copy = emergency_on_signal(&s, Some(&path), true);
        assert_eq!(copy, format!("{}.save", path.display()), "{who:?}");
        assert_eq!(read(Path::new(&copy)), b"unsaved text");
        let m = meta(Path::new(&copy));
        assert_eq!((m.mode() & 0o7777, m.uid()), (0o640, who.uid()));
        assert_eq!(read(&path), b"saved text");

        // SIGTERM too; the existing copy stays, the next one is numbered.
        let copy = emergency_on_signal(&s, Some(&path), false);
        assert_eq!(copy, format!("{}.save.1", path.display()));
        assert_eq!(read(Path::new(&copy)), b"unsaved text");
    }
}

#[test]
fn emergency_copies_never_write_through_a_symlink() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, b"saved text", 0o644, who.uid());
        let victim = s.base.join("victim");
        s.file(&victim, b"victim", 0o666, who.uid());
        std::os::unix::fs::symlink(&victim, s.path("f.save")).unwrap();
        own(&s.path("f.save"), who.uid());
        let out = s.run("emergency", Some(&path), b"unsaved", "");
        assert_eq!(out, format!("{}.save.1", path.display()));
        assert_eq!(read(&victim), b"victim");
        assert!(meta(&s.path("f.save")).file_type().is_symlink());
    }
}

#[test]
fn emergency_copy_of_another_users_file_goes_home() {
    if !needs_root("emergency_copy_of_another_users_file_goes_home") {
        return;
    }
    // Root's copy of a user's file: in root's home, not in the user's directory (CVE-2024-5742).
    let s = Scratch::new(Who::Me);
    let user_dir = s.base.join("user");
    s.mkdir(&user_dir, 0o755, USER);
    let path = user_dir.join("f");
    s.file(&path, b"saved", 0o644, USER);
    let copy = emergency_on_signal(&s, Some(&path), true);
    assert_eq!(copy, s.home.join("f.save").display().to_string());
    let m = meta(Path::new(&copy));
    assert_eq!((m.uid(), m.mode() & 0o777), (0, 0o600));
    assert_eq!(names(&user_dir), ["f"]);

    // A user's copy of another user's file they may write: in the user's home.
    let s = Scratch::new(Who::User);
    let shared = s.base.join("shared");
    s.mkdir(&shared, 0o777, OTHER);
    let path = shared.join("f");
    s.file(&path, b"saved", 0o666, OTHER);
    let copy = s.run("emergency", Some(&path), b"unsaved", "");
    assert_eq!(copy, s.home.join("f.save").display().to_string());
    let m = meta(Path::new(&copy));
    assert_eq!((m.uid(), m.mode() & 0o777), (USER, 0o600));
    assert_eq!(names(&shared), ["f"]);
}

#[test]
fn emergency_copy_without_a_writable_directory_goes_home() {
    for who in everyone() {
        if is_root() && who == Who::Me {
            continue;
        }
        let s = Scratch::new(who);
        let ro = s.path("ro");
        s.mkdir(&ro, 0o755, who.uid());
        let path = ro.join("f");
        s.file(&path, b"saved", 0o644, who.uid());
        set_mode(&ro, 0o555);
        let copy = s.run("emergency", Some(&path), b"unsaved", "");
        assert_eq!(copy, s.home.join("f.save").display().to_string(), "{who:?}");
        assert_eq!(read(Path::new(&copy)), b"unsaved");
    }
}

#[test]
fn emergency_copy_of_a_buffer_without_a_name() {
    for who in everyone() {
        let s = Scratch::new(who);
        let copy = s.run("emergency", None, b"unsaved", "");
        let name = Path::new(&copy)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("nano.") && name.ends_with(".save"),
            "{copy}"
        );
        assert_eq!(read(&s.dir.join(&name)), b"unsaved");
        assert_eq!(meta(&s.dir.join(&name)).mode() & 0o777, 0o600);
    }
}

// ---------------------------------------------------------------------------------------------
// Lock files.

/// A lock file as GNU nano 8.4 writes it (seen with `nano --locking`).
fn nano_lock(user: &str, pid: u32, file: &str) -> Vec<u8> {
    let mut b = vec![0u8; 1024];
    b[0..10].copy_from_slice(b"b0nano 8.4");
    b[24..28].copy_from_slice(&pid.to_le_bytes());
    b[28..28 + user.len()].copy_from_slice(user.as_bytes());
    b[68..72].copy_from_slice(b"host");
    b[108..108 + file.len()].copy_from_slice(file.as_bytes());
    b
}

#[test]
fn lock_files_in_nanos_format() {
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, b"text", 0o640, geteuid().as_raw());
    let Locking::Locked(lock) = lock(&path, false).unwrap() else {
        panic!("not locked");
    };
    let lock_path = s.path(".f.swp");
    assert_eq!(lock.path(), lock_path);
    let data = read(&lock_path);
    assert_eq!(data.len(), 1024);
    assert_eq!(&data[0..2], b"b0");
    assert!(data[2..12].starts_with(b"editor "));
    let pid = getpid().as_raw_nonzero().get() as u32;
    assert_eq!(&data[24..28], &pid.to_le_bytes());
    let path_bytes = path.as_os_str().as_bytes();
    assert_eq!(&data[108..108 + path_bytes.len()], path_bytes);
    assert_eq!(data[1007], 0);
    assert_eq!(meta(&lock_path).mode() & 0o777, 0o640);
    lock.set_modified(true).unwrap();
    assert_eq!(read(&lock_path)[1007], 0x55);
    lock.set_modified(false).unwrap();
    assert_eq!(read(&lock_path)[1007], 0);

    // A second editor sees who holds it.
    let Locking::Busy(info) = super::lock(&path, false).unwrap() else {
        panic!("not busy");
    };
    assert_eq!(info.pid, pid);
    drop(lock);
    assert!(!lock_path.exists());
}

#[test]
fn another_editors_lock() {
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, b"text", 0o644, geteuid().as_raw());
    let lock_path = s.path(".f.swp");
    stdfs::write(&lock_path, nano_lock("alice", 99_999, "f")).unwrap();
    let Locking::Busy(info) = lock(&path, false).unwrap() else {
        panic!("not busy");
    };
    let expected = LockInfo {
        user: "alice".into(),
        pid: 99_999,
        program: "nano 8.4".into(),
    };
    assert_eq!(info, expected);
    // "Open anyway": the lock becomes the editor's.
    let Locking::Locked(lock) = lock(&path, true).unwrap() else {
        panic!("not locked");
    };
    assert!(read(&lock_path)[2..12].starts_with(b"editor "));
    // Another editor takes it over in turn: dropping ours leaves theirs.
    stdfs::remove_file(&lock_path).unwrap();
    stdfs::write(&lock_path, nano_lock("bob", 1, "f")).unwrap();
    drop(lock);
    assert_eq!(read(&lock_path), nano_lock("bob", 1, "f"));
}

#[test]
fn bad_lock_files_are_left_alone() {
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, b"text", 0o644, geteuid().as_raw());
    let lock_path = s.path(".f.swp");
    stdfs::write(&lock_path, b"garbage\n").unwrap();
    assert!(matches!(lock(&path, false).unwrap(), Locking::Bad(p) if p == lock_path));
    assert_eq!(read(&lock_path), b"garbage\n");
    // A symlink as the lock file is not followed.
    stdfs::remove_file(&lock_path).unwrap();
    let victim = s.path("victim");
    stdfs::write(&victim, b"victim").unwrap();
    std::os::unix::fs::symlink(&victim, &lock_path).unwrap();
    assert!(matches!(lock(&path, false).unwrap(), Locking::Bad(_)));
    assert_eq!(read(&victim), b"victim");
}

#[test]
fn locks_of_new_files_symlinks_and_other_users_files() {
    let s = Scratch::new(Who::Me);
    // A new file: the lock exists before the file does.
    let new_file = s.path("new");
    let Locking::Locked(lock_new) = lock(&new_file, false).unwrap() else {
        panic!("not locked");
    };
    assert!(s.path(".new.swp").exists() && !new_file.exists());
    // A symlink: the lock goes next to the symlink, as nano's.
    std::os::unix::fs::symlink("new", s.path("link")).unwrap();
    let Locking::Locked(lock_link) = lock(&s.path("link"), false).unwrap() else {
        panic!("not locked");
    };
    assert!(s.path(".link.swp").exists());
    drop((lock_new, lock_link));
    if is_root() {
        // Root's lock on a user's file: only root can read it.
        let path = s.path("users");
        s.file(&path, b"text", 0o644, USER);
        let Locking::Locked(lock) = lock(&path, false).unwrap() else {
            panic!("not locked");
        };
        assert_eq!(meta(lock.path()).mode() & 0o777, 0o600);
    }
}

#[test]
fn no_lock_in_a_directory_without_write_permission() {
    for who in everyone() {
        if is_root() && who == Who::Me {
            continue;
        }
        let s = Scratch::new(who);
        let ro = s.path("ro");
        s.mkdir(&ro, 0o755, who.uid());
        s.file(&ro.join("f"), b"text", 0o644, who.uid());
        set_mode(&ro, 0o555);
        let result = s.run("lock", Some(&ro.join("f")), b"", "");
        assert!(result.contains("os error 13"), "{who:?}: {result}");
        assert_eq!(names(&ro), ["f"]);
        // Where it can, the lock is taken as that user.
        let result = s.run("lock", Some(&s.path("f")), b"", "");
        assert_eq!(result, format!("locked {}", s.path(".f.swp").display()));
    }
}

// ---------------------------------------------------------------------------------------------
// After the security review.

/// Mode 0660 with `group::---`: the group bits are the mask; the owning group may not read it.
fn acl_denying_the_group() -> Vec<u8> {
    acl(&[
        (ACL_USER_OBJ, 6, NO_ID),
        (ACL_USER, 6, OTHER),
        (ACL_GROUP_OBJ, 0, NO_ID),
        (ACL_MASK, 6, NO_ID),
        (ACL_OTHER, 0, NO_ID),
    ])
}

/// A file with that ACL; false when the file system has no ACLs.
fn file_with_acl(s: &Scratch, path: &Path, uid: u32) -> bool {
    s.file(path, &old(), 0o600, uid);
    match setxattr(path, ACL, &acl_denying_the_group(), XattrFlags::empty()) {
        Ok(()) => {
            assert_eq!(meta(path).mode() & 0o777, 0o660);
            true
        }
        Err(e) if unsupported(e, "ACLs") => false,
        Err(e) => panic!("{e}"),
    }
}

/// Nobody but the owner may use a copy of a file with an ACL: the copies get no ACL.
fn owner_only(path: &Path) {
    assert_eq!(meta(path).mode() & 0o077, 0, "{}", path.display());
    assert!(xattr(path, ACL).is_none());
}

#[test]
fn copies_of_a_file_with_an_acl_are_for_the_owner_only() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        if !file_with_acl(&s, &path, who.uid()) {
            return;
        }
        // The backup.
        let result = s.run("save", Some(&path), &new(), "backup");
        assert!(result.starts_with("replaced"), "{who:?}: {result}");
        owner_only(&s.path("f~"));
        assert_eq!(xattr(&path, ACL), Some(acl_denying_the_group()));
        // The emergency copy.
        let copy = s.run("emergency", Some(&path), b"unsaved", "");
        assert_eq!(copy, format!("{}.save", path.display()), "{who:?}");
        owner_only(Path::new(&copy));
        // The lock file.
        let result = s.run("lock", Some(&path), b"", "");
        assert!(result.starts_with("locked"), "{result}");
        // The child has ended and removed it: take it here to look at it.
        if who == Who::Me {
            let Locking::Locked(lock) = lock(&path, false).unwrap() else {
                panic!("not locked");
            };
            owner_only(lock.path());
        }
        // The hidden copy made before writing in place, kept when the save is killed.
        stdfs::hard_link(&path, s.path("link")).unwrap();
        let out = s.run_with("save", Some(&path), &new(), "", &[kill_at(3)]);
        let killed = out.killed.unwrap();
        let copies: Vec<_> = names(&s.dir)
            .into_iter()
            .filter(|n| random_temp(n, "f"))
            .collect();
        assert_eq!(copies.len(), 1, "{who:?}: killed at {killed}");
        owner_only(&s.path(&copies[0]));
    }
}

#[test]
fn sigkill_never_leaves_a_temporary_file_more_open_than_the_original() {
    for who in everyone() {
        // An ACL that keeps the owning group out.
        let mut acl_steps = 0;
        let s = Scratch::new(who);
        if file_with_acl(&s, &s.path("probe"), who.uid()) {
            acl_steps = kill_at_every_step(
                who,
                "",
                |s| {
                    let path = s.path("f");
                    file_with_acl(s, &path, who.uid());
                    path
                },
                |s, _, step| {
                    for name in names(&s.dir).iter().filter(|n| random_temp(n, "f")) {
                        let tmp = s.path(name);
                        let acl = xattr(&tmp, ACL);
                        let open = meta(&tmp).mode() & 0o077 != 0;
                        assert!(
                            !open || acl == Some(acl_denying_the_group()),
                            "{who:?}: {name} open to others without the ACL after {step}"
                        );
                    }
                },
            );
        }
        // Set-user-ID: only in the last moment before the rename.
        let suid_steps = kill_at_every_step(
            who,
            "",
            |s| {
                let path = s.path("f");
                s.file(&path, &old(), 0o4755, who.uid());
                path
            },
            |s, path, step| {
                for name in names(&s.dir).iter().filter(|n| random_temp(n, "f")) {
                    let suid = meta(&s.path(name)).mode() & 0o6000 != 0;
                    assert!(
                        !suid || step == "privileges given",
                        "{who:?}: {name} after {step}"
                    );
                }
                assert_eq!(meta(path).mode() & 0o7777, 0o4755);
            },
        );
        assert!(suid_steps > 9 && (acl_steps == 0 || acl_steps > 9));
    }
}

#[test]
fn a_file_swapped_under_the_editor_is_not_saved() {
    for who in everyone() {
        let s = Scratch::new(who);
        let path = s.path("f");
        s.file(&path, &old(), 0o644, who.uid());
        // What the editor read.
        let m = super::stat(&path).unwrap().unwrap();
        let id = format!("{}:{}:{}", m.dev.0, m.dev.1, m.ino);
        // The name now points somewhere else (as a user who owns the directory could do).
        let victim = s.base.join("victim");
        s.file(&victim, b"victim", 0o666, who.uid());
        // Moved away, not removed: its inode number is not given to another file.
        stdfs::rename(&path, s.base.join("read")).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        own(&path, who.uid());
        let result = s.run_expecting("save", Some(&path), &new(), "", Some(&id));
        assert_eq!(result, "error: File on disk has changed", "{who:?}");
        assert_eq!(read(&victim), b"victim");
        // Another file under the name.
        stdfs::remove_file(&path).unwrap();
        s.file(&path, b"other", 0o644, who.uid());
        let result = s.run_expecting("save", Some(&path), &new(), "", Some(&id));
        assert_eq!(result, "error: File on disk has changed");
        assert_eq!(read(&path), b"other");
        // A new buffer, and a file appeared under its name.
        let result = s.run_expecting("save", Some(&path), &new(), "", Some("absent"));
        assert_eq!(result, "error: File on disk has changed");
        // The file the editor read is gone.
        stdfs::remove_file(&path).unwrap();
        let result = s.run_expecting("save", Some(&path), &new(), "", Some(&id));
        assert_eq!(result, "error: File on disk has changed");
        assert!(!path.exists());
        // Told so, the editor saves again with what is there now.
        let result = s.run_expecting("save", Some(&path), &new(), "", Some("absent"));
        assert_eq!((result.as_str(), read(&path)), ("replaced", new()));
    }
}

#[test]
fn emergency_copies_only_next_to_the_file_in_the_users_own_directory() {
    if !needs_root("emergency_copies_only_next_to_the_file_in_the_users_own_directory") {
        return;
    }
    // Root edits, through a symlink in a user's directory, a root file in a private directory.
    let s = Scratch::new(Who::Me);
    let private = s.base.join("private");
    s.mkdir(&private, 0o700, 0);
    let file = private.join("f");
    s.file(&file, b"saved", 0o644, 0);
    let user_dir = s.base.join("user");
    s.mkdir(&user_dir, 0o755, USER);
    let link = user_dir.join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    own(&link, USER);
    let copy = s.run("emergency", Some(&link), b"unsaved", "");
    assert_eq!(copy, s.home.join("link.save").display().to_string());
    // Root's own file in a user's directory: also home.
    let own_file = user_dir.join("g");
    s.file(&own_file, b"saved", 0o644, 0);
    let copy = s.run("emergency", Some(&own_file), b"unsaved", "");
    assert_eq!(copy, s.home.join("g.save").display().to_string());
    assert_eq!(names(&user_dir), ["g", "link"]);
    // A buffer without a name, in a user's directory as the current one.
    let mut cmd = s.command("emergency", None, &s.base.join("input"));
    stdfs::write(s.base.join("input"), b"unsaved").unwrap();
    let out = cmd.current_dir(&user_dir).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains(&s.home.display().to_string()));
    assert_eq!(names(&user_dir), ["g", "link"]);

    // A user's own file in a directory another user owns.
    let s = Scratch::new(Who::User);
    let shared = s.base.join("shared");
    s.mkdir(&shared, 0o777, OTHER);
    let path = shared.join("f");
    s.file(&path, b"saved", 0o644, USER);
    let copy = s.run("emergency", Some(&path), b"unsaved", "");
    assert_eq!(copy, s.home.join("f.save").display().to_string());
    assert_eq!(names(&shared), ["f"]);
}

#[test]
fn control_characters_in_a_lock_file_are_not_shown() {
    let s = Scratch::new(Who::Me);
    let path = s.path("f");
    s.file(&path, b"text", 0o644, geteuid().as_raw());
    let mut data = nano_lock("evil\x1b]0;t\x07\x1b[2J\u{9b}x", 7, "f");
    data[2..12].copy_from_slice(b"n\x7fo\x1b[1m8.4");
    stdfs::write(s.path(".f.swp"), data).unwrap();
    let Locking::Busy(info) = lock(&path, false).unwrap() else {
        panic!("not busy");
    };
    assert_eq!(info.user, "evil?]0;t??[2J?x");
    assert_eq!(info.program, "n?o?[1m8.4");
}
