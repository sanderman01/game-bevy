//! Open archive files, and positioned reads from them.
//!
//! Reads name their offset explicitly and never move a shared cursor, so concurrent loads from one
//! archive need no lock around the read itself.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// How many archive files stay open at once. Mods can ship many archives; a process has a limited
/// number of file handles.
const MAX_OPEN: usize = 64;

/// The most recently used archive files, reopened on demand.
#[derive(Default)]
pub(crate) struct OpenFiles {
    /// Most recently used last.
    files: Mutex<Vec<(PathBuf, Arc<File>)>>,
}

impl OpenFiles {
    pub(crate) fn get(&self, path: &Path) -> io::Result<Arc<File>> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = files.iter().position(|(open, _)| open == path) {
            let entry = files.remove(i);
            let file = entry.1.clone();
            files.push(entry);
            return Ok(file);
        }
        let file = Arc::new(File::open(path)?);
        if files.len() >= MAX_OPEN {
            files.remove(0);
        }
        files.push((path.to_path_buf(), file.clone()));
        Ok(file)
    }
}

/// Reads exactly `len` bytes at `offset`.
pub(crate) fn read_at(file: &File, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{len} bytes do not fit in memory"),
        )
    })?;
    let mut buf = vec![0; len];
    read_exact_at(file, &mut buf, offset)?;
    Ok(buf)
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// `seek_read` moves the handle's cursor, but every call names its own offset, so the cursor is
/// never relied on.
#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
