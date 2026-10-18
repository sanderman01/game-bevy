//! Reading the asset an inspection subcommand was pointed at: a filesystem path, a path under the
//! asset root (also inside a mounted archive), or an alias such as `examples::airship`.

use ename_asset_alias::ContentIndex;
use ename_asset_package::build_index;
use ename_asset_vfs::{ArchiveVfs, Vfs, VfsError};
use std::{
    cell::OnceCell,
    fmt,
    path::{Component, Path, PathBuf},
};

/// What the content scan needs to resolve an alias. The scan runs once, on the first alias.
pub struct ScanPolicy<'a> {
    asset_root: &'a Path,
    search_paths: &'a [String],
    load_order_path: Option<&'a Path>,
    index: OnceCell<ContentIndex>,
}

impl<'a> ScanPolicy<'a> {
    pub fn new(
        asset_root: &'a Path,
        search_paths: &'a [String],
        load_order_path: Option<&'a Path>,
    ) -> Self {
        Self {
            asset_root,
            search_paths,
            load_order_path,
            index: OnceCell::new(),
        }
    }

    fn index(&self) -> &ContentIndex {
        self.index.get_or_init(|| {
            let (scan, _) =
                crate::scan::scan(self.asset_root, self.search_paths, self.load_order_path);
            build_index(&scan).0
        })
    }
}

/// One asset's bytes, and where to read the files next to it (a `.bin`, a `.meta`).
pub struct AssetFile {
    pub bytes: Vec<u8>,
    location: Location,
}

enum Location {
    File(PathBuf),
    /// A path relative to the asset root, read through the archive mounts.
    Asset {
        vfs: ArchiveVfs,
        path: PathBuf,
    },
}

/// Why an asset or a file next to it could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    NotFound(String),
    UnknownAlias(String),
    /// `alias#Label`: labels name what the game's loader makes, not a file.
    Label(String),
    /// A sibling reference that leaves the asset root or is absolute.
    OutsideRoot(String),
    Io {
        path: String,
        message: String,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(arg) => write!(
                f,
                "{arg}: not a file, nor a path under the asset root or an alias"
            ),
            Self::UnknownAlias(alias) => write!(f, "{alias}: no package claims this alias"),
            Self::Label(arg) => write!(
                f,
                "{arg}: a label names a sub-asset the game's loader makes; give the file \
                 without it, or ask the running game with `asset_inspect`"
            ),
            Self::OutsideRoot(path) => write!(f, "{path}: leaves the asset root"),
            Self::Io { path, message } => write!(f, "{path}: {message}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl AssetFile {
    /// How the asset is named in output: an asset path with `/` on every OS, a filesystem path in
    /// the OS's own form.
    pub fn name(&self) -> String {
        match &self.location {
            Location::File(path) => path.display().to_string(),
            Location::Asset { path, .. } => asset_path_string(path),
        }
    }

    /// The file's extension, lowercased.
    pub fn extension(&self) -> Option<String> {
        let path = match &self.location {
            Location::File(path) | Location::Asset { path, .. } => path,
        };
        Some(path.extension()?.to_str()?.to_ascii_lowercase())
    }

    /// Reads `relative` (forward slashes, as a glTF URI or a sidecar name) next to this file.
    pub fn read_sibling(&self, relative: &str) -> Result<Vec<u8>, ReadError> {
        match &self.location {
            Location::File(path) => {
                let sibling = path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(relative_path(relative)?);
                read_file(&sibling)
            }
            Location::Asset { vfs, path } => {
                let sibling = normalize(&path.parent().unwrap_or(Path::new("")).join(relative))
                    .ok_or_else(|| ReadError::OutsideRoot(relative.to_owned()))?;
                read_vfs(vfs, &sibling)
            }
        }
    }

    /// The `.meta` sidecar Bevy reads, or `None` when there is none.
    pub fn meta(&self) -> Result<Option<Vec<u8>>, ReadError> {
        let name = match &self.location {
            Location::File(path) | Location::Asset { path, .. } => path.file_name(),
        };
        let Some(name) = name.and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        match self.read_sibling(&format!("{name}.meta")) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(ReadError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Reads `arg`: an alias when it has `::`, else a filesystem path, else a path under the asset
/// root (with or without the root's own prefix), which also reaches into mounted archives.
pub fn read_asset(arg: &str, policy: &ScanPolicy) -> Result<AssetFile, ReadError> {
    let vfs = || ArchiveVfs::new(policy.asset_root);
    if is_alias(arg) {
        if arg.contains('#') {
            return Err(ReadError::Label(arg.to_owned()));
        }
        let path = policy
            .index()
            .resolve(arg)
            .ok_or_else(|| ReadError::UnknownAlias(arg.to_owned()))?
            .to_path_buf();
        let vfs = vfs();
        return Ok(AssetFile {
            bytes: read_vfs(&vfs, &path)?,
            location: Location::Asset { vfs, path },
        });
    }
    let file = Path::new(arg);
    if file.is_file() {
        return Ok(AssetFile {
            bytes: read_file(file)?,
            location: Location::File(file.to_path_buf()),
        });
    }
    let relative = file.strip_prefix(policy.asset_root).unwrap_or(file);
    let path = normalize(relative).ok_or_else(|| ReadError::NotFound(arg.to_owned()))?;
    let vfs = vfs();
    match read_vfs(&vfs, &path) {
        Ok(bytes) => Ok(AssetFile {
            bytes,
            location: Location::Asset { vfs, path },
        }),
        Err(ReadError::NotFound(_)) => Err(ReadError::NotFound(arg.to_owned())),
        Err(err) => Err(err),
    }
}

/// Whether `arg` is read as an alias, which is what needs the content scan and its load order.
pub fn is_alias(arg: &str) -> bool {
    arg.contains("::")
}

/// `path` with `/` separators, the way Bevy prints an asset path.
pub fn asset_path_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// `412 KB`, `1.4 MB`: binary units, as a file manager shows them.
pub fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < KB * KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes / (KB * KB))
    }
}

/// A relative path with `.` and `..` folded away, or `None` if it is absolute or climbs out.
fn normalize(path: &Path) -> Option<PathBuf> {
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
    Some(out)
}

/// A sibling reference must stay relative; on disk it may still climb with `..`. A Windows drive
/// prefix (`C:x`, relative to that drive's current directory) counts as leaving.
fn relative_path(relative: &str) -> Result<PathBuf, ReadError> {
    let path = Path::new(relative);
    let rooted = path
        .components()
        .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir));
    if path.is_absolute() || rooted {
        return Err(ReadError::OutsideRoot(relative.to_owned()));
    }
    Ok(path.to_path_buf())
}

fn read_file(path: &Path) -> Result<Vec<u8>, ReadError> {
    std::fs::read(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ReadError::NotFound(path.display().to_string()),
        _ => ReadError::Io {
            path: path.display().to_string(),
            message: err.to_string(),
        },
    })
}

fn read_vfs(vfs: &ArchiveVfs, path: &Path) -> Result<Vec<u8>, ReadError> {
    futures_lite::future::block_on(vfs.read_file(path)).map_err(|err| match err {
        VfsError::NotFound(_) => ReadError::NotFound(asset_path_string(path)),
        VfsError::Io { message, .. } => ReadError::Io {
            path: asset_path_string(path),
            message,
        },
    })
}
