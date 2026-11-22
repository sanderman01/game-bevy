use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use ename_application::Paths;
use tracing::{debug, warn};

use crate::CacheKey;

/// Starts every entry file; bump the digit when the layout changes.
const MAGIC: [u8; 8] = *b"ENCACHE1";
/// Magic, payload length (u64 little endian), payload BLAKE3 hash.
const HEADER_LEN: usize = 8 + 8 + 32;
const TEMP_EXTENSION: &str = "tmp";
/// A temporary file this old belongs to a write that died.
const ABANDONED_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// One namespace of the project's disk cache. Cheap to clone and safe to use from any thread or
/// several processes at once: writers never touch a file readers can see until it is complete.
#[derive(Clone, Debug)]
pub struct DiskCache {
    paths: Paths,
    namespace: PathBuf,
}

/// What [`DiskCache::prune`] removed: entries, and temporary files of writes that died.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub files: u64,
    pub bytes: u64,
}

/// Names Windows reserves for devices in every folder.
const RESERVED_ON_WINDOWS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

impl DiskCache {
    /// `namespace` is a folder under the cache directory, one or more `/`-separated segments of
    /// lowercase ASCII letters, digits, `_` and `-`, such as `landscape/cards`. Name it after the
    /// crate and the data, so two users never share one. A namespace may sit inside another: a
    /// segment is never two hex digits, which name entry shards, so neither touches the other's
    /// entries.
    ///
    /// # Panics
    ///
    /// On a namespace outside those rules, or a Windows device name: it is a literal in the
    /// caller's code.
    pub fn new(paths: Paths, namespace: &str) -> Self {
        let valid = |segment: &str| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
                && !is_shard_name(segment)
                && !RESERVED_ON_WINDOWS.contains(&segment)
        };
        assert!(
            namespace.split('/').all(valid),
            "cache namespace `{namespace}` is not `/`-separated [a-z0-9_-] segments, or uses a \
             two hex digit segment or a Windows device name"
        );
        Self {
            paths,
            namespace: namespace.split('/').collect(),
        }
    }

    /// The entry's payload, or `None` on a miss. An entry that fails its checksum is removed and
    /// reads as a miss. A hit marks the entry used, for [`DiskCache::prune`].
    pub fn get(&self, key: &CacheKey) -> Option<Vec<u8>> {
        let path = self.entry_path(self.paths.cache_path()?, key);
        let payload = match read_entry(&path) {
            Ok(Some(payload)) => payload,
            Ok(None) => {
                warn!("Cache entry {} is damaged; removing it", path.display());
                if let Err(error) = fs::remove_file(&path) {
                    debug!("Removing {} failed: {error}", path.display());
                }
                return None;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                warn!("Cache entry {} unreadable: {error}", path.display());
                return None;
            }
        };
        if let Err(error) = mark_used(&path) {
            debug!("Marking {} used failed: {error}", path.display());
        }
        Some(payload)
    }

    /// Stores `payload` under `key`, replacing any entry there. Does nothing without a cache
    /// directory. On an error nothing is stored and earlier entries are untouched.
    pub fn put(&self, key: &CacheKey, payload: &[u8]) -> io::Result<()> {
        if self.paths.cache_path().is_none() {
            return Ok(());
        }
        let path = self.entry_path(self.paths.ensure_cache_path()?, key);
        let Some(shard) = path.parent() else {
            return Err(io::Error::other("cache entry path has no parent"));
        };
        fs::create_dir_all(shard)?;
        // Unique per process and write, so concurrent writers of one key never share a file.
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let temp = path.with_extension(format!(
            "{}-{}.{TEMP_EXTENSION}",
            std::process::id(),
            WRITES.fetch_add(1, Ordering::Relaxed)
        ));
        let written = write_entry(&temp, payload).and_then(|()| fs::rename(&temp, &path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        match written {
            // Windows refuses to replace a file another process holds open, such as a second game
            // reading or writing the same key. The entry there is the same content, so keep it.
            Err(_) if path.is_file() => Ok(()),
            written => written,
        }
    }

    /// Removes the entries not read or written for `unused_for`, and temporary files of writes
    /// that died. Files another process changes meanwhile are skipped. Shard folders stay, so a
    /// concurrent `put` always finds its folder. Slow on a large cache: call it off the main
    /// thread.
    pub fn prune(&self, unused_for: Duration) -> io::Result<Pruned> {
        let Some(cache) = self.paths.cache_path() else {
            return Ok(Pruned::default());
        };
        let root = cache.join(&self.namespace);
        let now = SystemTime::now();
        let mut pruned = Pruned::default();
        let shards = match fs::read_dir(&root) {
            Ok(shards) => shards,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(pruned),
            Err(error) => return Err(error),
        };
        let shards = shards
            .flatten()
            .filter(|shard| shard.file_name().to_str().is_some_and(is_shard_name));
        for shard in shards {
            let Ok(entries) = fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
                let is_temp = path.extension().is_some_and(|e| e == TEMP_EXTENSION);
                let limit = if is_temp {
                    ABANDONED_TEMP_AGE
                } else {
                    unused_for
                };
                if age > limit && fs::remove_file(&path).is_ok() {
                    pruned.files += 1;
                    pruned.bytes += metadata.len();
                }
            }
        }
        Ok(pruned)
    }

    /// `<cache>/<namespace>/<first two hex digits>/<hex key>`: shards keep folders small.
    fn entry_path(&self, cache: &Path, key: &CacheKey) -> PathBuf {
        let hex = key.to_hex();
        cache.join(&self.namespace).join(&hex[..2]).join(hex)
    }
}

fn is_shard_name(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn write_entry(path: &Path, payload: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(&MAGIC)?;
    file.write_all(&(payload.len() as u64).to_le_bytes())?;
    file.write_all(blake3::hash(payload).as_bytes())?;
    file.write_all(payload)?;
    Ok(())
}

/// The payload, or `None` when the file is not a whole, intact entry. Reads the payload straight
/// into a buffer of its size, after checking the size against the file's.
fn read_entry(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut header = [0; HEADER_LEN];
    if file_len < HEADER_LEN as u64 {
        return Ok(None);
    }
    file.read_exact(&mut header)?;
    let (magic, rest) = header.split_at(8);
    let (len, hash) = rest.split_at(8);
    let len = u64::from_le_bytes(len.try_into().unwrap_or_default());
    if magic != MAGIC || file_len - HEADER_LEN as u64 != len {
        return Ok(None);
    }
    let Ok(len) = usize::try_from(len) else {
        return Ok(None);
    };
    let mut payload = vec![0; len];
    file.read_exact(&mut payload)?;
    Ok((blake3::hash(&payload).as_bytes() == hash).then_some(payload))
}

fn mark_used(path: &Path) -> io::Result<()> {
    File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyHasher;

    struct Fixture {
        _dir: tempfile::TempDir,
        cache_dir: PathBuf,
        cache: DiskCache,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let paths = Paths::from_paths(dir.path().join("data"), cache_dir.clone());
        Fixture {
            cache: DiskCache::new(paths, "tests/store"),
            cache_dir,
            _dir: dir,
        }
    }

    fn key(name: &str) -> CacheKey {
        KeyHasher::new().str(name).finish()
    }

    fn entry_file(fixture: &Fixture, key: &CacheKey) -> PathBuf {
        fixture.cache.entry_path(&fixture.cache_dir, key)
    }

    #[test]
    fn stored_payload_reads_back() {
        let fixture = fixture();
        assert_eq!(fixture.cache.get(&key("a")), None);
        fixture.cache.put(&key("a"), b"payload").unwrap();
        assert_eq!(
            fixture.cache.get(&key("a")).as_deref(),
            Some(&b"payload"[..])
        );
        assert_eq!(fixture.cache.get(&key("b")), None);
    }

    #[test]
    fn put_replaces_an_entry() {
        let fixture = fixture();
        fixture.cache.put(&key("a"), b"one").unwrap();
        fixture.cache.put(&key("a"), b"two").unwrap();
        assert_eq!(fixture.cache.get(&key("a")).as_deref(), Some(&b"two"[..]));
    }

    #[test]
    fn put_recreates_a_deleted_cache_directory() {
        let fixture = fixture();
        fixture.cache.put(&key("a"), b"one").unwrap();
        fs::remove_dir_all(&fixture.cache_dir).unwrap();
        fixture.cache.put(&key("a"), b"two").unwrap();
        assert_eq!(fixture.cache.get(&key("a")).as_deref(), Some(&b"two"[..]));
    }

    #[test]
    fn damaged_entries_miss_and_are_removed() {
        let fixture = fixture();
        fixture.cache.put(&key("a"), b"payload").unwrap();
        let path = entry_file(&fixture, &key("a"));
        let mut data = fs::read(&path).unwrap();
        *data.last_mut().unwrap() ^= 1;
        fs::write(&path, &data).unwrap();
        assert_eq!(fixture.cache.get(&key("a")), None);
        assert!(!path.exists());

        fixture.cache.put(&key("b"), b"payload").unwrap();
        let path = entry_file(&fixture, &key("b"));
        fs::write(&path, &fs::read(&path).unwrap()[..HEADER_LEN + 3]).unwrap();
        assert_eq!(fixture.cache.get(&key("b")), None);
    }

    #[test]
    fn prune_removes_only_unused_entries() {
        let fixture = fixture();
        fixture.cache.put(&key("old"), b"old").unwrap();
        fixture.cache.put(&key("new"), b"new").unwrap();
        let old = entry_file(&fixture, &key("old"));
        let long_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let pruned = fixture
            .cache
            .prune(Duration::from_secs(24 * 60 * 60))
            .unwrap();
        assert_eq!(pruned.files, 1);
        assert_eq!(pruned.bytes, (HEADER_LEN + 3) as u64);
        assert_eq!(fixture.cache.get(&key("old")), None);
        assert!(fixture.cache.get(&key("new")).is_some());
    }

    #[test]
    fn pruning_a_namespace_leaves_one_inside_it() {
        let fixture = fixture();
        let inner = DiskCache::new(
            Paths::from_paths(fixture._dir.path().join("data"), fixture.cache_dir.clone()),
            "tests/store/inner",
        );
        inner.put(&key("a"), b"a").unwrap();
        fixture.cache.prune(Duration::ZERO).unwrap();
        assert!(inner.get(&key("a")).is_some());
    }

    #[test]
    fn a_hit_keeps_an_entry_from_pruning() {
        let fixture = fixture();
        fixture.cache.put(&key("a"), b"a").unwrap();
        let path = entry_file(&fixture, &key("a"));
        let long_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert!(fixture.cache.get(&key("a")).is_some());
        let pruned = fixture
            .cache
            .prune(Duration::from_secs(24 * 60 * 60))
            .unwrap();
        assert_eq!(pruned.files, 0);
    }

    #[test]
    fn without_a_cache_directory_nothing_is_stored() {
        let cache = DiskCache::new(Paths::default(), "tests/store");
        cache.put(&key("a"), b"a").unwrap();
        assert_eq!(cache.get(&key("a")), None);
        assert_eq!(cache.prune(Duration::ZERO).unwrap(), Pruned::default());
    }

    #[test]
    #[should_panic(expected = "not `/`-separated")]
    fn namespaces_cannot_leave_the_cache() {
        DiskCache::new(Paths::default(), "../saves");
    }

    #[test]
    #[should_panic(expected = "Windows device name")]
    fn namespaces_cannot_name_a_device() {
        DiskCache::new(Paths::default(), "tests/nul");
    }

    #[test]
    #[should_panic(expected = "two hex digit")]
    fn namespaces_cannot_look_like_a_shard() {
        DiskCache::new(Paths::default(), "tests/ab");
    }
}
