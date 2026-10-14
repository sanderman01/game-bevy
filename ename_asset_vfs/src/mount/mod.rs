//! Archive mounts: a `.zip` or `.tar` file read as a directory named without its extension.
//!
//! [`Mounts`] answers every question about one physical asset root: whether a logical path is
//! loose or inside a mount, what a mounted directory lists, and the bytes of a member. It is
//! Bevy-free and blocking; [`crate::ArchiveVfs`] and, behind the `bevy` feature, the archive asset
//! reader both sit on it. See `docs/assets-archives.md`.

mod files;
mod index;
#[cfg(feature = "tar")]
mod tar_index;
#[cfg(feature = "zip")]
mod zip_index;

use crate::{DirEntry, is_hidden_from_listings};
use files::OpenFiles;
use index::{ArchiveIndex, Storage};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fmt::Display,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tracing::{info, warn};

/// What an archive may contain before it is refused. Checked before anything is allocated for a
/// member, and the decoded size again while inflating.
#[derive(Debug, Clone)]
pub struct Limits {
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
    /// Two archives, or an archive and a file, claim one mount. The mount is unavailable.
    ConflictingArchive,
    /// The archive could not be read, or holds something a mount must not serve.
    CorruptArchive,
}

impl Display for MountProblemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ShadowedArchive => "shadowed archive",
            Self::ConflictingArchive => "conflicting archive",
            Self::CorruptArchive => "corrupt archive",
        })
    }
}

/// Something wrong with a mount, naming both the logical directory and the physical archive.
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

/// The archive extensions this build mounts. A format that is compiled out is an ordinary file.
const FORMATS: &[(&str, Format)] = &[
    #[cfg(feature = "zip")]
    ("zip", Format::Zip),
    #[cfg(feature = "tar")]
    ("tar", Format::Tar),
];

/// One archive mounted as a directory. `index` is the reason it is unavailable when it is.
struct Mount {
    logical: PathBuf,
    physical: PathBuf,
    index: Result<ArchiveIndex, String>,
}

impl Mount {
    fn index(&self) -> io::Result<&ArchiveIndex> {
        self.index.as_ref().map_err(|reason| {
            io::Error::other(format!(
                "archive {} is unavailable: {reason}",
                self.physical.display()
            ))
        })
    }
}

/// What one logical path prefix is on disk.
#[derive(Clone)]
enum Prefix {
    Dir,
    /// A file, or nothing at all. Nothing below it is mounted.
    Plain,
    Mount(Arc<Mount>),
}

enum Resolved {
    Loose,
    /// `member` is `/`-separated and empty for the mount root.
    Mounted {
        mount: Arc<Mount>,
        member: String,
    },
}

/// Every archive mount under one physical asset root.
///
/// Classifications are cached, negative ones included, and an archive is indexed once, so a
/// changed archive or a new directory over one needs a restart.
pub struct Mounts {
    root: PathBuf,
    limits: Limits,
    prefixes: Mutex<HashMap<PathBuf, Arc<OnceLock<Prefix>>>>,
    files: OpenFiles,
    problems: Mutex<Vec<MountProblem>>,
}

impl Mounts {
    /// `root` is the physical asset root every logical path is relative to.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            limits: Limits::default(),
            prefixes: Mutex::default(),
            files: OpenFiles::default(),
            problems: Mutex::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every problem found so far. Mounts are discovered lazily, so this grows as the tree is
    /// read; after a full scan it covers every mount the scan passed through.
    pub fn problems(&self) -> Vec<MountProblem> {
        self.problems
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Classifies `path`, relative to the asset root, existing or not.
    pub fn origin(&self, path: &Path) -> Origin {
        match self.resolve(path, true) {
            Resolved::Loose => Origin::Loose,
            Resolved::Mounted { mount, member } => {
                let archive = mount.physical.clone();
                match &mount.index {
                    Ok(index) if index.member(&member).is_some() => Origin::Member {
                        archive,
                        member: member.into(),
                    },
                    _ => Origin::InMount { archive },
                }
            }
        }
    }

    /// Classifies a filesystem path, absolute or relative to the current directory, by first
    /// making it relative to the asset root with symlinks resolved. A path outside the root is
    /// loose.
    pub fn origin_of_file(&self, path: &Path) -> Origin {
        match self.logical_path(path) {
            Some(logical) => self.origin(&logical),
            None => Origin::Loose,
        }
    }

    /// `path` relative to the asset root, or `None` when it lies outside it. A path that doesn't
    /// exist yet is resolved through its nearest existing ancestor.
    pub fn logical_path(&self, path: &Path) -> Option<PathBuf> {
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
        match self.resolve(path, false) {
            Resolved::Loose => None,
            Resolved::Mounted { mount, member } => Some(self.read_member(&mount, &member)),
        }
    }

    /// A mounted directory's listing, sorted and with hidden entries dropped, or `None` when
    /// `path` is loose. Entry paths are logical, relative to the asset root.
    pub fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>> {
        let Resolved::Mounted { mount, member } = self.resolve(path, true) else {
            return None;
        };
        let listing = mount.index().and_then(|index| {
            index
                .children(&member)
                .ok_or_else(|| not_found(path, &mount, &member))
        });
        Some(listing.map(|children| {
            children
                .iter()
                .map(|child| DirEntry {
                    path: path.join(&*child.name),
                    is_dir: child.is_dir,
                })
                .filter(|entry| !is_hidden_from_listings(&entry.path))
                .collect()
        }))
    }

    /// Whether a mounted `path` is a directory, or `None` when it is loose. A mount root is a
    /// directory even when the archive is unavailable, so a listing shows it and reading it fails
    /// loudly instead of the mount vanishing.
    pub fn is_dir(&self, path: &Path) -> Option<io::Result<bool>> {
        let Resolved::Mounted { mount, member } = self.resolve(path, true) else {
            return None;
        };
        if member.is_empty() {
            return Some(Ok(true));
        }
        Some(mount.index().and_then(|index| {
            if index.is_dir(&member) {
                Ok(true)
            } else if index.member(&member).is_some() {
                Ok(false)
            } else {
                Err(not_found(path, &mount, &member))
            }
        }))
    }

    /// Rewrites a loose directory's listing so each archive shows as its mount directory.
    ///
    /// Returns each path with whether it is a mount. An archive shadowed by a real directory, or
    /// beside a file of its name, is dropped from the listing; it is never an asset itself.
    pub fn overlay_listing(&self, entries: Vec<PathBuf>) -> Vec<(PathBuf, bool)> {
        let loose: HashSet<&Path> = entries.iter().map(PathBuf::as_path).collect();
        let mut mounts = Vec::new();
        for entry in &entries {
            let Some(mount_path) = mount_path_of(entry) else {
                continue;
            };
            self.warn_case_clash(&mount_path, &loose);
            if matches!(self.prefix(&mount_path), Prefix::Mount(_))
                && !loose.contains(mount_path.as_path())
            {
                mounts.push(mount_path);
            }
        }
        let mut out: Vec<(PathBuf, bool)> = entries
            .iter()
            .filter(|entry| mount_path_of(entry).is_none())
            .map(|entry| (entry.clone(), false))
            .chain(mounts.into_iter().map(|path| (path, true)))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn warn_case_clash(&self, mount_path: &Path, loose: &HashSet<&Path>) {
        let Some(name) = mount_path.file_name().and_then(OsStr::to_str) else {
            return;
        };
        for other in loose {
            let Some(other_name) = other.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if other_name != name && other_name.eq_ignore_ascii_case(name) {
                warn!(
                    "{} differs only in case from the archive mount {}; they are separate on this \
                     filesystem but not on every platform",
                    other.display(),
                    mount_path.display()
                );
            }
        }
    }

    fn read_member(&self, mount: &Mount, member: &str) -> io::Result<Vec<u8>> {
        let index = mount.index()?;
        let entry = index.member(member).ok_or_else(|| {
            if member.is_empty() || index.is_dir(member) {
                io::Error::new(
                    io::ErrorKind::IsADirectory,
                    format!("{member:?} in {} is a directory", mount.physical.display()),
                )
            } else {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{member:?} is not in {}", mount.physical.display()),
                )
            }
        })?;
        let file = self.files.get(&mount.physical)?;
        let bytes = match &entry.storage {
            #[cfg(feature = "tar")]
            Storage::Tar { offset } => files::read_at(&file, *offset, entry.size),
            #[cfg(feature = "zip")]
            Storage::Zip(zip) => zip_index::read(&file, entry.size, zip),
        };
        bytes.map_err(|err| {
            let err = io::Error::new(
                err.kind(),
                format!(
                    "reading {member:?} from {}: {err}",
                    mount.physical.display()
                ),
            );
            if err.kind() != io::ErrorKind::NotFound {
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

    /// Walks `path`'s prefixes: a real directory continues the walk, an archive mounts, anything
    /// else means nothing below is mounted. `include_last` also classifies `path` itself, which a
    /// file read does not need.
    fn resolve(&self, path: &Path, include_last: bool) -> Resolved {
        self.walk(path, include_last, |prefix| Some(self.prefix(prefix)))
            .expect("every prefix is classified")
    }

    /// True when `path` is known to be loose from cached classifications alone, so a reader can
    /// skip the blocking hop that classifying a new prefix needs.
    #[cfg(feature = "bevy")]
    pub(crate) fn is_known_loose(&self, path: &Path, include_last: bool) -> bool {
        let cached = |prefix: &Path| {
            let prefixes = self.prefixes.lock().unwrap_or_else(|e| e.into_inner());
            prefixes.get(prefix)?.get().cloned()
        };
        matches!(self.walk(path, include_last, cached), Some(Resolved::Loose))
    }

    /// Walks `path`'s prefixes: a real directory continues the walk, an archive mounts, anything
    /// else means nothing below is mounted. `include_last` also classifies `path` itself, which a
    /// file read does not need. `None` when `classify` has no answer for a prefix.
    fn walk(
        &self,
        path: &Path,
        include_last: bool,
        mut classify: impl FnMut(&Path) -> Option<Prefix>,
    ) -> Option<Resolved> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part),
                Component::CurDir => {}
                _ => return Some(Resolved::Loose),
            }
        }
        let checked = if include_last {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };
        let mut prefix = PathBuf::new();
        for (i, part) in parts.iter().enumerate().take(checked) {
            prefix.push(part);
            match classify(&prefix)? {
                Prefix::Dir => {}
                Prefix::Plain => return Some(Resolved::Loose),
                Prefix::Mount(mount) => {
                    let member = parts[i + 1..]
                        .iter()
                        .map(|part| part.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    return Some(Resolved::Mounted { mount, member });
                }
            }
        }
        Some(Resolved::Loose)
    }

    fn prefix(&self, prefix: &Path) -> Prefix {
        let cell = self
            .prefixes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(prefix.to_path_buf())
            .or_default()
            .clone();
        // Outside the map's lock: indexing a large archive must not stall unrelated lookups.
        cell.get_or_init(|| self.classify(prefix)).clone()
    }

    fn classify(&self, prefix: &Path) -> Prefix {
        let physical = self.root.join(prefix);
        let archives: Vec<(PathBuf, Format)> = FORMATS
            .iter()
            .map(|(extension, format)| (with_suffix(&physical, extension), *format))
            .filter(|(path, _)| path.is_file())
            .collect();
        let problem = |physical: &Path, kind, detail: String| MountProblem {
            logical: prefix.to_path_buf(),
            physical: physical.to_path_buf(),
            kind,
            detail,
        };
        let unavailable = |physical: PathBuf, reason: String| {
            Prefix::Mount(Arc::new(Mount {
                logical: prefix.to_path_buf(),
                physical,
                index: Err(reason),
            }))
        };
        match std::fs::metadata(&physical) {
            Ok(metadata) if metadata.is_dir() => {
                for (archive, _) in &archives {
                    self.record(problem(
                        archive,
                        MountProblemKind::ShadowedArchive,
                        format!("the directory {} is used instead", physical.display()),
                    ));
                }
                Prefix::Dir
            }
            _ if archives.is_empty() => Prefix::Plain,
            Ok(_) => {
                let (archive, _) = archives.into_iter().next().expect("checked non-empty");
                let reason = format!("a file named {} sits beside it", physical.display());
                self.record(problem(
                    &archive,
                    MountProblemKind::ConflictingArchive,
                    reason.clone(),
                ));
                unavailable(archive, reason)
            }
            Err(_) if archives.len() > 1 => {
                let names: Vec<String> = archives
                    .iter()
                    .map(|(path, _)| path.display().to_string())
                    .collect();
                let reason = format!("{} all claim this mount", names.join(" and "));
                for (archive, _) in &archives {
                    self.record(problem(
                        archive,
                        MountProblemKind::ConflictingArchive,
                        reason.clone(),
                    ));
                }
                unavailable(archives[0].0.clone(), reason)
            }
            Err(_) => {
                let (archive, format) = archives.into_iter().next().expect("checked non-empty");
                match self.index(&archive, format) {
                    Ok(index) => {
                        hint_redundant_folder(prefix, &index);
                        Prefix::Mount(Arc::new(Mount {
                            logical: prefix.to_path_buf(),
                            physical: archive,
                            index: Ok(index),
                        }))
                    }
                    Err(reason) => {
                        self.record(problem(
                            &archive,
                            MountProblemKind::CorruptArchive,
                            reason.clone(),
                        ));
                        unavailable(archive, reason)
                    }
                }
            }
        }
    }

    fn index(&self, archive: &Path, format: Format) -> Result<ArchiveIndex, String> {
        let file = File::open(archive).map_err(|err| err.to_string())?;
        match format {
            #[cfg(feature = "zip")]
            Format::Zip => zip_index::index(&file, &self.limits),
            #[cfg(feature = "tar")]
            Format::Tar => tar_index::index(&file, &self.limits),
        }
    }

    fn record(&self, problem: MountProblem) {
        let mut problems = self.problems.lock().unwrap_or_else(|e| e.into_inner());
        if !problems.contains(&problem) {
            warn!("{problem}");
            problems.push(problem);
        }
    }
}

/// A member at the top of an archive whose single top-level folder repeats the mount's name
/// probably meant to be the mount itself.
fn hint_redundant_folder(prefix: &Path, index: &ArchiveIndex) {
    let Some(name) = prefix.file_name().and_then(OsStr::to_str) else {
        return;
    };
    if let Some([only]) = index.children("")
        && only.is_dir
        && &*only.name == name
    {
        info!(
            "Archive mounted at {} holds a single folder {name}/, so its files are under \
             {}/{name}/. Pack the folder's contents instead if that is not intended.",
            prefix.display(),
            prefix.display()
        );
    }
}

/// The mount directory an archive file in a listing would appear as, if it is one.
pub(crate) fn mount_path_of(entry: &Path) -> Option<PathBuf> {
    let extension = entry.extension()?.to_str()?;
    FORMATS
        .iter()
        .any(|(format, _)| *format == extension)
        .then(|| entry.with_extension(""))
}

fn with_suffix(path: &Path, extension: &str) -> PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push(".");
    path.push(extension);
    PathBuf::from(path)
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
