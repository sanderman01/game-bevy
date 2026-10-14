//! Open archive files, and positioned reads from them.
//!
//! Reads name their offset explicitly and never rely on a shared cursor, so concurrent loads from
//! one archive need no lock around the read itself; only finding the open file takes one.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

/// How many archive files stay open at once. Mods can ship many archives; a process has a limited
/// number of file handles.
const MAX_OPEN: usize = 64;

/// Enough of a file's metadata to tell that it was replaced since it was indexed.
///
/// Unix compares device and inode. On Windows the file index is unstable in std, so creation time
/// stands in: a replacement with the same length, modification time and creation time (a copy
/// that preserves all three) goes unnoticed there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
    #[cfg(windows)]
    created: u64,
}

impl Identity {
    pub(crate) fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
            #[cfg(windows)]
            created: std::os::windows::fs::MetadataExt::creation_time(&metadata),
        })
    }
}

/// The most recently used archive files, reopened on demand.
#[derive(Default)]
pub(crate) struct OpenFiles {
    /// Most recently used last.
    files: Mutex<Vec<(PathBuf, Arc<File>)>>,
}

impl OpenFiles {
    /// The open archive at `path`, reopening it if it was evicted. A reopened file that no longer
    /// matches `identity` was replaced since it was indexed, and its index would read garbage.
    pub(crate) fn get(&self, path: &Path, identity: &Identity) -> io::Result<Arc<File>> {
        {
            let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = files.iter().position(|(open, _)| open == path) {
                let entry = files.remove(i);
                let file = entry.1.clone();
                files.push(entry);
                return Ok(file);
            }
        }
        // Opened outside the lock: other archives' reads need not wait on this one.
        let file = Arc::new(File::open(path)?);
        advise_random(&file);
        if Identity::of(&file)? != *identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} changed since it was indexed; restart to use the new archive",
                    path.display()
                ),
            ));
        }
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, open)) = files.iter().find(|(open, _)| open == path) {
            return Ok(open.clone());
        }
        if files.len() >= MAX_OPEN {
            files.remove(0);
        }
        files.push((path.to_path_buf(), file.clone()));
        Ok(file)
    }
}

/// Tells the kernel that reads of `file` are scattered. Every archive read is an exact range, so
/// readahead only pulls in data nobody asked for: indexing a tar of small members otherwise reads
/// most of the file to get at its 512-byte headers. Advice only, so a failure is ignored.
pub(crate) fn advise_random(file: &File) {
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(file, 0, None, rustix::fs::Advice::Random);
    #[cfg(not(target_os = "linux"))]
    let _ = file;
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
