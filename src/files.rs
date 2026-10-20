// SPDX-License-Identifier: MIT
//! Reading and writing files.
//!
//! A minimal version for now: `write` writes the file in place. Safe saving (a temporary file,
//! fsync and rename), emergency saves, locking and backups replace it in a later change.

use std::io;
use std::path::Path;

pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}
