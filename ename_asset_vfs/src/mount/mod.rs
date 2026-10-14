//! Archive mounts: a `.zip` or `.tar` file read as a directory named without its extension.
//!
//! [`Mounts`] answers every question about one physical asset root: whether a logical path is
//! loose or inside a mount, what a mounted directory lists, and the bytes of a member. It is
//! Bevy-free and blocking; [`crate::ArchiveVfs`] and, behind the `bevy` feature, the archive asset
//! reader both sit on it. See `docs/assets-archives.md`.
//!
//! Always compiled, so the write guard and problem reporting exist whichever crate turned a
//! format on. With no format feature nothing ever mounts.
//!
//! Mounts are found one loose directory at a time: the first question that passes through a
//! directory lists it once, recording its subdirectories and archives. Every later question about
//! anything in it is answered from memory, so a loose tree costs one extra listing per directory
//! and nothing per file.

// With no format compiled in, nothing can be indexed or read, and the code that would is dead.
#![cfg_attr(
    not(any(feature = "zip", feature = "tar")),
    allow(dead_code, unused_variables, unreachable_code)
)]

mod files;
mod index;
#[cfg(feature = "tar")]
mod tar_index;
#[cfg(feature = "zip")]
mod zip_index;

use crate::{DirEntry, is_hidden_from_listings};
use files::{Identity, OpenFiles};
use index::{ArchiveIndex, IndexError, Member};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fmt::Display,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
};
use tracing::{info, warn};

/// What an archive may contain before it is refused. The member count and central directory size
/// a zip declares are checked before it is parsed, every record is counted as it is read, and the
/// decoded size is capped again while inflating.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Records of any kind: files, directories, and tar extension headers.
    pub max_members: usize,
    /// Bytes, compressed or decoded. A member is decoded fully into memory.
    pub max_member_size: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_members: 1 << 21,
            max_member_size: 1 << 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountProblemKind {
    /// A real directory has the mount's name, and is used instead.
    ShadowedArchive,
    /// Two archives, or an archive and a file, claim one mount.
    ConflictingArchive,
    /// The archive is malformed, holds something a mount must not serve, or a member's bytes
    /// failed their check.
    CorruptArchive,
    /// The archive could not be opened or read while indexing.
    UnreadableArchive,
}

impl Display for MountProblemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ShadowedArchive => "shadowed archive",
            Self::ConflictingArchive => "conflicting archive",
            Self::CorruptArchive => "corrupt archive",
            Self::UnreadableArchive => "unreadable archive",
        })
    }
}

/// Something wrong with a mount, naming both the logical directory and the physical archive. A
/// problem with one member names it in `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountProblem {
    /// The directory the archive would mount as, relative to the asset root.
    pub logical: PathBuf,
    pub physical: PathBuf,
    pub kind: MountProblemKind,
    pub detail: String,
}

impl Display for MountProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {} ({}): {}",
            self.kind,
            self.logical.display(),
            self.physical.display(),
            self.detail
        )
    }
}

/// Where a logical path lives, whether or not anything is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Not under any mount: a loose file or directory, or nothing yet.
    Loose,
    /// Absolute, or climbing above the asset root with `..`.
    Outside,
    /// The archive file of a mount.
    Archive { archive: PathBuf },
    /// An existing archive member.
    Member { archive: PathBuf, member: PathBuf },
    /// Inside a mount, but not an existing member: the mount root, a directory inside it, a path
    /// that doesn't exist, or anything under an unavailable mount.
    InMount { archive: PathBuf },
}

impl Origin {
    pub fn is_loose(&self) -> bool {
        matches!(self, Self::Loose)
    }
}

#[derive(Debug, Clone, Copy)]
enum Format {
    #[cfg(feature = "zip")]
    Zip,
    #[cfg(feature = "tar")]
    Tar,
}

/// The archive extensions this build mounts, matched exactly: `core.ZIP` is an ordinary file on
/// every platform. A format that is compiled out is an ordinary file too.
const FORMATS: &[(&str, Format)] = &[
    #[cfg(feature = "zip")]
    ("zip", Format::Zip),
    #[cfg(feature = "tar")]
    ("tar", Format::Tar),
];

/// The mount name and format of an archive file name, if it is one.
fn archive_name(name: &OsStr) -> Option<(OsString, Format)> {
    let name = name.to_str()?;
    let (stem, extension) = name.rsplit_once('.')?;
    let (_, format) = FORMATS.iter().find(|(known, _)| *known == extension)?;
    (!stem.is_empty()).then(|| (stem.into(), *format))
}

struct Indexed {
    index: ArchiveIndex,
    identity: Identity,
}

/// One archive mounted as a directory, indexed on first use.
struct Mount {
    logical: PathBuf,
    physical: PathBuf,
    format: Option<Format>,
    /// The reason it is unavailable when it is. Set up front for a conflict.
    index: OnceLock<Result<Indexed, String>>,
}

/// What one loose directory holds, as far as mounts are concerned. Built from one listing.
#[derive(Default)]
struct DirInfo {
    subdirs: HashSet<OsString>,
    mounts: HashMap<OsString, Arc<Mount>>,
    /// Regular files with an archive extension, with the mount they serve. Never listed.
    archive_files: HashMap<OsString, Option<Arc<Mount>>>,
}

enum Located {
    Loose,
    Outside,
    Archive(Arc<Mount>),
    /// `member` is `/`-separated and empty for the mount root.
    Mounted {
        mount: Arc<Mount>,
        member: String,
    },
}

/// An answer that may need filesystem work. `NeedsIo` only comes back when work was not allowed,
/// so a caller on an async thread can answer from memory and hop to a blocking thread otherwise.
pub(crate) enum Answer<T> {
    Loose,
    Ready(io::Result<T>),
    NeedsIo,
}

impl<T> Answer<T> {
    fn blocking(self) -> Option<io::Result<T>> {
        match self {
            Self::Loose => None,
            Self::Ready(result) => Some(result),
            Self::NeedsIo => unreachable!("filesystem work was allowed"),
        }
    }
}

/// Every archive mount under one physical asset root.
///
/// Directory listings and archive indexes are cached, so a changed archive, or a directory
/// created later with an archive in it, needs a restart.
pub struct Mounts {
    root: PathBuf,
    limits: Limits,
    dirs: RwLock<HashMap<PathBuf, Arc<DirInfo>>>,
    /// One lock per directory being listed, so concurrent first questions wait for one listing.
    listing: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    #[cfg(test)]
    listings: std::sync::atomic::AtomicUsize,
    files: OpenFiles,
    problems: Mutex<Vec<MountProblem>>,
}

impl Mounts {
    /// `root` is the physical asset root every logical path is relative to.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            limits: Limits::default(),
            dirs: RwLock::default(),
            listing: Mutex::default(),
            #[cfg(test)]
            listings: Default::default(),
            files: OpenFiles::default(),
            problems: Mutex::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Every problem found so far. Mounts are discovered lazily, so this grows as the tree is
    /// read; after a full scan it covers every mount the scan passed through.
    pub fn problems(&self) -> Vec<MountProblem> {
        self.problems
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Classifies `path`, relative to the asset root, existing or not. Fails when a directory on
    /// the way could not be listed: a writer must then refuse, not guess.
    pub fn origin(&self, path: &Path) -> io::Result<Origin> {
        Ok(match self.locate(path, true, true).expect("io allowed")? {
            Located::Loose => Origin::Loose,
            Located::Outside => Origin::Outside,
            Located::Archive(mount) => Origin::Archive {
                archive: mount.physical.clone(),
            },
            Located::Mounted { mount, member } => {
                let archive = mount.physical.clone();
                match self.indexed(&mount, true) {
                    Some(Ok(indexed)) if indexed.index.member(&member).is_some() => {
                        Origin::Member {
                            archive,
                            member: member.into(),
                        }
                    }
                    _ => Origin::InMount { archive },
                }
            }
        })
    }

    /// Classifies a filesystem path, absolute or relative to the current directory, by first
    /// making it relative to the asset root with symlinks resolved. A path outside the root is
    /// loose: it is not part of the asset tree at all.
    pub fn origin_of_file(&self, path: &Path) -> io::Result<Origin> {
        match self.logical_path(path) {
            Some(logical) => self.origin(&logical),
            None => Ok(Origin::Loose),
        }
    }

    /// Whether anything under `path`, itself included, is a mount or an archive file: what a
    /// recursive remove or a rename of `path` would destroy or move.
    pub fn holds_archive(&self, path: &Path) -> io::Result<bool> {
        if !self.origin(path)?.is_loose() {
            return Ok(true);
        }
        let Some(path) = normalize(path) else {
            return Ok(false);
        };
        let mut stack = vec![path.into_owned()];
        while let Some(dir) = stack.pop() {
            let info = self.dir_info(&dir, true).expect("io allowed")?;
            if !info.archive_files.is_empty() {
                return Ok(true);
            }
            stack.extend(info.subdirs.iter().map(|name| dir.join(name)));
        }
        Ok(false)
    }

    /// `path` relative to the asset root, or `None` when it lies outside it. A path that doesn't
    /// exist yet is resolved through its nearest existing ancestor.
    fn logical_path(&self, path: &Path) -> Option<PathBuf> {
        let root = std::fs::canonicalize(&self.root).ok()?;
        let path = std::path::absolute(path).ok()?;
        let mut existing = path.as_path();
        let mut rest = Vec::new();
        let canonical = loop {
            if let Ok(canonical) = std::fs::canonicalize(existing) {
                break canonical;
            }
            rest.push(existing.file_name()?);
            existing = existing.parent()?;
        };
        let mut logical = canonical.strip_prefix(&root).ok()?.to_path_buf();
        logical.extend(rest.iter().rev());
        Some(logical)
    }

    /// A member's bytes, or `None` when `path` is loose and the caller reads it itself.
    pub fn read_file(&self, path: &Path) -> Option<io::Result<Vec<u8>>> {
        self.read_file_answer(path, true).blocking()
    }

    /// A mounted directory's listing, sorted and with hidden entries dropped, or `None` when
    /// `path` is loose. Entry paths are logical, relative to the asset root.
    pub fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>> {
        self.read_dir_answer(path, true).blocking()
    }

    /// Whether a mounted `path` is a directory, or `None` when it is loose. A mount root is a
    /// directory even when the archive is unavailable, so a listing shows it and reading it fails
    /// loudly instead of the mount vanishing.
    pub fn is_dir(&self, path: &Path) -> Option<io::Result<bool>> {
        self.is_dir_answer(path, true).blocking()
    }

    pub(crate) fn read_file_answer(&self, path: &Path, io: bool) -> Answer<Vec<u8>> {
        let located = match self.locate(path, false, io) {
            None => return Answer::NeedsIo,
            Some(Err(err)) => return Answer::Ready(Err(err)),
            Some(Ok(located)) => located,
        };
        let Located::Mounted { mount, member } = located else {
            return Answer::Loose;
        };
        let indexed = match self.indexed(&mount, io) {
            None => return Answer::NeedsIo,
            Some(Err(err)) => return Answer::Ready(Err(err)),
            Some(Ok(indexed)) => indexed,
        };
        let Some(entry) = indexed.index.member(&member) else {
            let kind = if member.is_empty() || indexed.index.is_dir(&member) {
                io::ErrorKind::IsADirectory
            } else {
                io::ErrorKind::NotFound
            };
            return Answer::Ready(Err(io::Error::new(
                kind,
                format!("{member:?} in {}", mount.physical.display()),
            )));
        };
        if !io {
            return Answer::NeedsIo;
        }
        Answer::Ready(self.read_member(&mount, indexed, entry, &member))
    }

    pub(crate) fn read_dir_answer(&self, path: &Path, io: bool) -> Answer<Vec<DirEntry>> {
        let located = match self.locate(path, true, io) {
            None => return Answer::NeedsIo,
            Some(Err(err)) => return Answer::Ready(Err(err)),
            Some(Ok(located)) => located,
        };
        let Located::Mounted { mount, member } = located else {
            return Answer::Loose;
        };
        let indexed = match self.indexed(&mount, io) {
            None => return Answer::NeedsIo,
            Some(Err(err)) => return Answer::Ready(Err(err)),
            Some(Ok(indexed)) => indexed,
        };
        let Some(children) = indexed.index.children(&member) else {
            return Answer::Ready(Err(not_found(path, &mount, &member)));
        };
        Answer::Ready(Ok(children
            .iter()
            .map(|child| DirEntry {
                path: path.join(&*child.name),
                is_dir: child.is_dir,
            })
            .filter(|entry| !is_hidden_from_listings(&entry.path))
            .collect()))
    }

    pub(crate) fn is_dir_answer(&self, path: &Path, io: bool) -> Answer<bool> {
        let located = match self.locate(path, true, io) {
            None => return Answer::NeedsIo,
            Some(Err(err)) => return Answer::Ready(Err(err)),
            Some(Ok(located)) => located,
        };
        let Located::Mounted { mount, member } = located else {
            return Answer::Loose;
        };
        if member.is_empty() {
            return Answer::Ready(Ok(true));
        }
        match self.indexed(&mount, io) {
            None => Answer::NeedsIo,
            Some(Err(err)) => Answer::Ready(Err(err)),
            Some(Ok(indexed)) if indexed.index.is_dir(&member) => Answer::Ready(Ok(true)),
            Some(Ok(indexed)) if indexed.index.member(&member).is_some() => {
                Answer::Ready(Ok(false))
            }
            Some(Ok(_)) => Answer::Ready(Err(not_found(path, &mount, &member))),
        }
    }

    /// Whether `path` names a mounted archive file, from what is already known. An archive in a
    /// directory nobody has looked at yet was never mounted, so `false` is right for it too.
    #[cfg(feature = "bevy")]
    pub(crate) fn is_known_archive(&self, path: &Path) -> bool {
        matches!(
            self.locate(path, true, false),
            Some(Ok(Located::Archive(_)))
        )
    }

    /// Rewrites a loose directory's listing so each archive shows as its mount directory: archive
    /// files are dropped and mount directories added. Returns each path with whether it is a
    /// mount, or `None` when the directory has not been classified and `io` is false. Nothing
    /// outside the asset root is mounted, so a listing there comes back unchanged.
    pub(crate) fn overlay_listing(
        &self,
        dir: &Path,
        entries: &[PathBuf],
        io: bool,
    ) -> Option<io::Result<Vec<(PathBuf, bool)>>> {
        let Some(dir) = normalize(dir) else {
            return Some(Ok(entries
                .iter()
                .map(|entry| (entry.clone(), false))
                .collect()));
        };
        let info = match self.dir_info(&dir, io)? {
            Ok(info) => info,
            Err(err) => return Some(Err(err)),
        };
        let mut out: Vec<(PathBuf, bool)> = entries
            .iter()
            .filter(|entry| {
                entry
                    .file_name()
                    .is_none_or(|name| !info.archive_files.contains_key(name))
            })
            .map(|entry| (entry.clone(), false))
            .chain(info.mounts.keys().map(|name| (dir.join(name), true)))
            .collect();
        out.sort();
        Some(Ok(out))
    }

    fn dir_info(&self, dir: &Path, io: bool) -> Option<io::Result<Arc<DirInfo>>> {
        if let Some(info) = self.dirs.read().unwrap_or_else(|e| e.into_inner()).get(dir) {
            return Some(Ok(info.clone()));
        }
        io.then(|| self.classify(dir))
    }

    /// Walks `path` one directory at a time: a real directory continues the walk, a mount ends it,
    /// anything else means nothing below is mounted. `include_last` also classifies `path` itself,
    /// which a file read does not need. `None` when a directory on the way has not been listed
    /// and `io` is false.
    fn locate(&self, path: &Path, include_last: bool, io: bool) -> Option<io::Result<Located>> {
        let Some(path) = normalize(path) else {
            return Some(Ok(Located::Outside));
        };
        let mut prefixes: Vec<&Path> = path.ancestors().collect();
        prefixes.reverse();
        let components = prefixes.len() - 1;
        let checked = if include_last {
            components
        } else {
            components.saturating_sub(1)
        };
        loop {
            let missing = {
                let dirs = self.dirs.read().unwrap_or_else(|e| e.into_inner());
                match walk(&dirs, &path, &prefixes, checked, components) {
                    Ok(located) => return Some(Ok(located)),
                    Err(missing) => missing.to_path_buf(),
                }
            };
            if !io {
                return None;
            }
            if let Err(err) = self.classify(&missing) {
                return Some(Err(err));
            }
        }
    }

    /// Lists one loose directory once, however many threads ask at the same time. A failed
    /// listing is not cached, so the next question tries again.
    fn classify(&self, dir: &Path) -> io::Result<Arc<DirInfo>> {
        let lock = self
            .listing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(dir.to_path_buf())
            .or_default()
            .clone();
        let _listing = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(info) = self.dirs.read().unwrap_or_else(|e| e.into_inner()).get(dir) {
            return Ok(info.clone());
        }
        let result = self.list(dir).map(|info| {
            let info = Arc::new(info);
            self.dirs
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(dir.to_path_buf(), info.clone());
            info
        });
        self.listing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(dir);
        result
    }

    /// Lists one loose directory and records what in it is a mount, reporting shadowed and
    /// conflicting archives. A directory that is missing or not a directory holds nothing.
    fn list(&self, dir: &Path) -> io::Result<DirInfo> {
        #[cfg(test)]
        self.listings
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let physical = self.root.join(dir);
        let mut info = DirInfo::default();
        let mut files: HashSet<OsString> = HashSet::new();
        let mut archives: HashMap<OsString, Vec<(OsString, Format)>> = HashMap::new();
        let entries = match std::fs::read_dir(&physical) {
            Ok(entries) => entries,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(info);
            }
            Err(err) => return Err(listing_error(&physical, err)),
        };
        for entry in entries {
            let entry = entry.map_err(|err| listing_error(&physical, err))?;
            let name = entry.file_name();
            let mut file_type = entry
                .file_type()
                .map_err(|err| listing_error(&entry.path(), err))?;
            if file_type.is_symlink() {
                // A broken link is neither a directory nor a file, as the platform reader sees it.
                let Ok(metadata) = std::fs::metadata(entry.path()) else {
                    continue;
                };
                file_type = metadata.file_type();
            }
            if file_type.is_dir() {
                info.subdirs.insert(name);
            } else if file_type.is_file() {
                if let Some((stem, format)) = archive_name(&name) {
                    archives
                        .entry(stem)
                        .or_default()
                        .push((name.clone(), format));
                }
                files.insert(name);
            }
        }
        for (stem, mut found) in archives {
            found.sort_by(|a, b| a.0.cmp(&b.0));
            let logical = dir.join(&stem);
            let physicals: Vec<PathBuf> =
                found.iter().map(|(name, _)| physical.join(name)).collect();
            let problem = |physical: &Path, kind, detail: String| MountProblem {
                logical: logical.clone(),
                physical: physical.to_path_buf(),
                kind,
                detail,
            };
            let mount = if info.subdirs.contains(&stem) {
                for archive in &physicals {
                    self.record(problem(
                        archive,
                        MountProblemKind::ShadowedArchive,
                        format!(
                            "the directory {} is used instead",
                            physical.join(&stem).display()
                        ),
                    ));
                }
                None
            } else if files.contains(&stem) {
                for archive in &physicals {
                    self.record(problem(
                        archive,
                        MountProblemKind::ConflictingArchive,
                        format!(
                            "the file {} is used instead",
                            physical.join(&stem).display()
                        ),
                    ));
                }
                None
            } else if found.len() > 1 {
                let names: Vec<String> =
                    physicals.iter().map(|p| p.display().to_string()).collect();
                let reason = format!("{} all claim this mount", names.join(" and "));
                for archive in &physicals {
                    self.record(problem(
                        archive,
                        MountProblemKind::ConflictingArchive,
                        reason.clone(),
                    ));
                }
                let index = OnceLock::new();
                let _ = index.set(Err(reason));
                Some(Arc::new(Mount {
                    logical: logical.clone(),
                    physical: physicals[0].clone(),
                    format: None,
                    index,
                }))
            } else {
                Some(Arc::new(Mount {
                    logical: logical.clone(),
                    physical: physicals[0].clone(),
                    format: Some(found[0].1),
                    index: OnceLock::new(),
                }))
            };
            warn_case_clash(&stem, &logical, &info.subdirs, &files);
            for (name, _) in found {
                info.archive_files.insert(name, mount.clone());
            }
            if let Some(mount) = mount {
                info.mounts.insert(stem, mount);
            }
        }
        Ok(info)
    }

    /// The mount's index, building it on first use when `io` allows.
    fn indexed<'m>(&self, mount: &'m Mount, io: bool) -> Option<io::Result<&'m Indexed>> {
        let unavailable = |reason: &String| {
            io::Error::other(format!(
                "archive {} is unavailable: {reason}",
                mount.physical.display()
            ))
        };
        if let Some(result) = mount.index.get() {
            return Some(result.as_ref().map_err(unavailable));
        }
        if !io {
            return None;
        }
        let result = mount.index.get_or_init(|| self.build_index(mount));
        Some(result.as_ref().map_err(unavailable))
    }

    fn build_index(&self, mount: &Mount) -> Result<Indexed, String> {
        let result = File::open(&mount.physical)
            .map_err(IndexError::from)
            .and_then(|file| {
                let identity = Identity::of(&file)?;
                let index = match mount.format.expect("a conflict is never indexed") {
                    #[cfg(feature = "zip")]
                    Format::Zip => zip_index::index(&file, &self.limits)?,
                    #[cfg(feature = "tar")]
                    Format::Tar => tar_index::index(&file, &self.limits)?,
                };
                Ok(Indexed { index, identity })
            });
        let (kind, reason) = match result {
            Ok(indexed) => {
                hint_redundant_folder(&mount.logical, &indexed.index);
                return Ok(indexed);
            }
            Err(IndexError::Invalid(reason)) => (MountProblemKind::CorruptArchive, reason),
            Err(IndexError::Io(err)) => (MountProblemKind::UnreadableArchive, err.to_string()),
        };
        self.record(MountProblem {
            logical: mount.logical.clone(),
            physical: mount.physical.clone(),
            kind,
            detail: reason.clone(),
        });
        Err(reason)
    }

    fn read_member(
        &self,
        mount: &Mount,
        indexed: &Indexed,
        entry: &Member,
        member: &str,
    ) -> io::Result<Vec<u8>> {
        let bytes = self
            .files
            .get(&mount.physical, &indexed.identity)
            .and_then(|file| read_storage(&file, entry));
        bytes.map_err(|err| {
            let err = io::Error::new(
                err.kind(),
                format!(
                    "reading {member:?} from {}: {err}",
                    mount.physical.display()
                ),
            );
            // Only bad bytes are a problem with the archive; a failed read may be transient.
            if err.kind() == io::ErrorKind::InvalidData {
                self.record(MountProblem {
                    logical: mount.logical.clone(),
                    physical: mount.physical.clone(),
                    kind: MountProblemKind::CorruptArchive,
                    detail: err.to_string(),
                });
            }
            err
        })
    }

    fn record(&self, problem: MountProblem) {
        let mut problems = self.problems.lock().unwrap_or_else(|e| e.into_inner());
        if !problems.contains(&problem) {
            warn!("{problem}");
            problems.push(problem);
        }
    }
}

#[cfg_attr(
    not(any(feature = "zip", feature = "tar")),
    expect(unused_variables, reason = "no format, so no member can exist")
)]
fn read_storage(file: &File, entry: &Member) -> io::Result<Vec<u8>> {
    match &entry.storage {
        #[cfg(feature = "tar")]
        index::Storage::Tar { offset } => files::read_at(file, *offset, entry.size),
        #[cfg(feature = "zip")]
        index::Storage::Zip(zip) => zip_index::read(file, entry.size, zip),
        #[cfg(not(any(feature = "zip", feature = "tar")))]
        _ => unreachable!("no format is compiled in"),
    }
}

/// The walk behind [`Mounts::locate`] over the directories listed so far. `Err` names the first
/// directory on the way that has not been listed.
fn walk<'p>(
    dirs: &HashMap<PathBuf, Arc<DirInfo>>,
    path: &Path,
    prefixes: &[&'p Path],
    checked: usize,
    components: usize,
) -> Result<Located, &'p Path> {
    for i in 0..checked {
        let dir = prefixes[i];
        let child = prefixes[i + 1].file_name().expect("a normal component");
        let info = dirs.get(dir).ok_or(dir)?;
        if let Some(mount) = info.mounts.get(child) {
            let member = path
                .components()
                .skip(i + 1)
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            return Ok(Located::Mounted {
                mount: mount.clone(),
                member,
            });
        }
        if i + 1 == components
            && let Some(Some(mount)) = info.archive_files.get(child)
        {
            return Ok(Located::Archive(mount.clone()));
        }
        if !info.subdirs.contains(child) {
            return Ok(Located::Loose);
        }
    }
    Ok(Located::Loose)
}

/// `path` with `.` dropped and `..` applied, borrowed when it is already clean. `None` when it is
/// absolute or climbs above the root.
pub(crate) fn normalize(path: &Path) -> Option<Cow<'_, Path>> {
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Some(Cow::Borrowed(path));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(Cow::Owned(out))
}

/// Names that differ only in case are separate on this filesystem but not on every platform.
fn warn_case_clash(
    stem: &OsStr,
    logical: &Path,
    subdirs: &HashSet<OsString>,
    files: &HashSet<OsString>,
) {
    let Some(stem) = stem.to_str() else {
        return;
    };
    for other in subdirs.iter().chain(files).filter_map(|name| name.to_str()) {
        if other != stem && other.eq_ignore_ascii_case(stem) {
            warn!(
                "{other} differs only in case from the archive mount {}; they are separate on \
                 this filesystem but not on every platform",
                logical.display()
            );
        }
    }
}

/// A member at the top of an archive whose single top-level folder repeats the mount's name
/// probably meant to be the mount itself.
fn hint_redundant_folder(logical: &Path, index: &ArchiveIndex) {
    let Some(name) = logical.file_name().and_then(OsStr::to_str) else {
        return;
    };
    if let Some([only]) = index.children("")
        && only.is_dir
        && &*only.name == name
    {
        info!(
            "Archive mounted at {} holds a single folder {name}/, so its files are under \
             {}/{name}/. Pack the folder's contents instead if that is not intended.",
            logical.display(),
            logical.display()
        );
    }
}

fn listing_error(path: &Path, err: io::Error) -> io::Error {
    io::Error::new(
        err.kind(),
        format!("listing {} to find archives: {err}", path.display()),
    )
}

fn not_found(path: &Path, mount: &Mount, member: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "{} ({member:?} is not in {})",
            path.display(),
            mount.physical.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{Answer, Mounts};
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Barrier, atomic::Ordering},
    };

    /// A loose tree costs one cached listing per directory, never an entry per file, and once a
    /// directory is listed every question about its files is answered from memory.
    #[test]
    fn a_loose_tree_is_classified_per_directory() {
        let root = tempfile::tempdir().unwrap();
        for dir in ["a", "a/b"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
            for i in 0..50 {
                std::fs::write(root.path().join(dir).join(format!("{i}.png")), "x").unwrap();
            }
        }
        let mounts = Mounts::new(root.path());
        for dir in ["", "a", "a/b"] {
            assert!(
                mounts
                    .overlay_listing(Path::new(dir), &[], true)
                    .unwrap()
                    .is_ok()
            );
        }
        assert_eq!(mounts.dirs.read().unwrap().len(), 3);
        for i in 0..50 {
            for file in [format!("a/{i}.png"), format!("a/b/{i}.png")] {
                let path = Path::new(&file);
                assert!(matches!(mounts.is_dir_answer(path, false), Answer::Loose));
                assert!(matches!(
                    mounts.read_file_answer(path, false),
                    Answer::Loose
                ));
            }
        }
        assert_eq!(mounts.dirs.read().unwrap().len(), 3);
    }

    /// Threads asking about one unlisted directory at once share one listing.
    #[test]
    fn concurrent_first_questions_list_a_directory_once() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        let mounts = Arc::new(Mounts::new(root.path()));
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let (mounts, barrier) = (mounts.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let path = format!("a/b/{i}.png");
                    assert!(mounts.origin(Path::new(&path)).unwrap().is_loose());
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(mounts.listings.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_listing_outside_the_root_is_returned_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let mounts = Mounts::new(root.path());
        let entries = [PathBuf::from("../x.zip")];
        let overlaid = mounts
            .overlay_listing(Path::new(".."), &entries, false)
            .unwrap()
            .unwrap();
        assert_eq!(overlaid, [(PathBuf::from("../x.zip"), false)]);
    }

    /// A directory that could not be listed is an error, and is listed again next time.
    #[cfg(unix)]
    #[test]
    fn a_failed_listing_is_an_error_and_is_not_cached() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root ignores permissions; nothing to test then.
        if std::fs::read_dir(&locked).is_ok() {
            return;
        }
        let mounts = Mounts::new(root.path());
        assert!(mounts.origin(Path::new("locked/inner/x.txt")).is_err());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            mounts
                .origin(Path::new("locked/inner/x.txt"))
                .unwrap()
                .is_loose()
        );
    }
}
