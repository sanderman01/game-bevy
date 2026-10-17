use crate::{OrganisationName, ProjectName};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// A project's per-user directories. `Some` means a location was found, not that it exists or is
/// writable; [`Paths::ensure_persistent_data_path`] and [`Paths::ensure_cache_path`] create them.
///
/// | OS | Persistent data | Cache |
/// | --- | --- | --- |
/// | Linux | `$XDG_DATA_HOME/<project>` | `$XDG_CACHE_HOME/<project>` |
/// | Windows | `%LOCALAPPDATA%\<org>\<project>\data` | `%LOCALAPPDATA%\<org>\<project>\cache` |
/// | macOS | `~/Library/Application Support/com.<org>.<project>` | `~/Library/Caches/com.<org>.<project>` |
///
/// On macOS both names are lowercased with their spaces removed, keeping a valid bundle identifier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    persistent_data_path: Option<PathBuf>,
    cache_path: Option<PathBuf>,
    rejected_cache_path: Option<PathBuf>,
}

/// Which convention to lay the directories out by. An argument rather than a `cfg`, so every
/// layout is testable on any host.
#[derive(Clone, Copy, Debug)]
enum Os {
    Linux,
    Windows,
    Mac,
}

impl Paths {
    /// Resolves from the OS: reads the environment and the OS folder APIs, creates nothing. All
    /// `None` on a target other than Linux, Windows and macOS.
    pub fn resolve(organisation: &OrganisationName, project: &ProjectName) -> Self {
        let Some(os) = host_os() else {
            return Self::default();
        };
        let (data_base, cache_base) = match os {
            Os::Linux | Os::Mac => (dirs::data_dir(), dirs::cache_dir()),
            // Both `FOLDERID_LocalAppData`: Local rather than Roaming, since roaming profiles
            // sync at logon and saves have no size cap.
            Os::Windows => (dirs::data_local_dir(), dirs::cache_dir()),
        };
        let (data, cache) = layout(os, data_base, cache_base, organisation, project);
        Self::checked(data, cache, host_folds_case())
    }

    /// Uses the given directories as they are, for tests and tools. Applies the same checks as
    /// [`Paths::resolve`].
    pub fn from_paths(persistent_data_path: PathBuf, cache_path: PathBuf) -> Self {
        Self::checked(
            Some(persistent_data_path),
            Some(cache_path),
            host_folds_case(),
        )
    }

    pub fn persistent_data_path(&self) -> Option<&Path> {
        self.persistent_data_path.as_deref()
    }

    pub fn cache_path(&self) -> Option<&Path> {
        self.cache_path.as_deref()
    }

    /// The cache path that was dropped because it equals, contains, or is inside the persistent
    /// data path, so clearing the cache can never delete saves. For the caller to report.
    pub fn rejected_cache_path(&self) -> Option<&Path> {
        self.rejected_cache_path.as_deref()
    }

    /// Creates the persistent data directory if it is missing and returns it.
    pub fn ensure_persistent_data_path(&self) -> io::Result<&Path> {
        ensure(self.persistent_data_path(), "persistent data")
    }

    /// Creates the cache directory if it is missing and returns it. Call it before each use: the
    /// user or the OS may delete the cache at any time.
    pub fn ensure_cache_path(&self) -> io::Result<&Path> {
        ensure(self.cache_path(), "cache")
    }

    /// A relative path would land under the working directory, and a `..` makes the overlap check
    /// unreliable, so both count as not found. The overlap check is lexical and does not follow
    /// symlinks; `fold_case` makes it case-insensitive, as Windows and macOS filesystems usually are.
    fn checked(data: Option<PathBuf>, cache: Option<PathBuf>, fold_case: bool) -> Self {
        let usable = |path: &PathBuf| {
            path.is_absolute() && !path.components().any(|c| c == Component::ParentDir)
        };
        let data = data.filter(usable);
        let cache = cache.filter(usable);
        let overlaps = match (&data, &cache) {
            (Some(data), Some(cache)) => {
                let data = comparable(data, fold_case);
                let cache = comparable(cache, fold_case);
                cache.starts_with(&data) || data.starts_with(&cache)
            }
            _ => false,
        };
        if overlaps {
            Self {
                persistent_data_path: data,
                cache_path: None,
                rejected_cache_path: cache,
            }
        } else {
            Self {
                persistent_data_path: data,
                cache_path: cache,
                rejected_cache_path: None,
            }
        }
    }
}

/// The path's components, lowercased when `fold_case`, for a component-wise prefix comparison.
fn comparable(path: &Path, fold_case: bool) -> Vec<String> {
    path.components()
        .map(|c| {
            let c = c.as_os_str().to_string_lossy();
            if fold_case {
                c.to_lowercase()
            } else {
                c.into_owned()
            }
        })
        .collect()
}

/// Whether the host's filesystems usually compare names case-insensitively.
const fn host_folds_case() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

fn host_os() -> Option<Os> {
    if cfg!(target_os = "linux") {
        Some(Os::Linux)
    } else if cfg!(target_os = "windows") {
        Some(Os::Windows)
    } else if cfg!(target_os = "macos") {
        Some(Os::Mac)
    } else {
        None
    }
}

/// Joins the project's directories onto the OS's bases. The one place the layout is decided.
fn layout(
    os: Os,
    data_base: Option<PathBuf>,
    cache_base: Option<PathBuf>,
    organisation: &OrganisationName,
    project: &ProjectName,
) -> (Option<PathBuf>, Option<PathBuf>) {
    match os {
        // XDG: one folder per application, no organisation level.
        Os::Linux => (
            data_base.map(|base| base.join(project.as_str())),
            cache_base.map(|base| base.join(project.as_str())),
        ),
        // `<Company>\<Product>`. Both bases are the same folder, so the suffixes keep them apart.
        Os::Windows => {
            let project_dir =
                |base: PathBuf| base.join(organisation.as_str()).join(project.as_str());
            (
                data_base.map(|base| project_dir(base).join("data")),
                cache_base.map(|base| project_dir(base).join("cache")),
            )
        }
        // The reverse-DNS bundle identifier: lowercase, spaces removed.
        Os::Mac => {
            let label = |name: &str| name.to_ascii_lowercase().replace(' ', "");
            let id = format!(
                "com.{}.{}",
                label(organisation.as_str()),
                label(project.as_str())
            );
            (
                data_base.map(|base| base.join(&id)),
                cache_base.map(|base| base.join(&id)),
            )
        }
    }
}

fn ensure<'a>(path: Option<&'a Path>, what: &str) -> io::Result<&'a Path> {
    let path = path.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {what} directory is available on this system"),
        )
    })?;
    fs::create_dir_all(path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "could not create the {what} directory {}: {err}",
                path.display()
            ),
        )
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: OrganisationName = OrganisationName::from_static("ename");
    const PROJECT: ProjectName = ProjectName::from_static("example-game");

    /// An absolute base on the host running the test.
    fn base(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn resolved(os: Os, data: Option<PathBuf>, cache: Option<PathBuf>) -> Paths {
        let (data, cache) = layout(os, data, cache, &ORG, &PROJECT);
        Paths::checked(data, cache, false)
    }

    #[test]
    fn linux_layout_has_no_organisation() {
        let paths = resolved(Os::Linux, Some(base("data")), Some(base("cache")));
        assert_eq!(
            paths.persistent_data_path(),
            Some(base("data").join("example-game").as_path())
        );
        assert_eq!(
            paths.cache_path(),
            Some(base("cache").join("example-game").as_path())
        );
    }

    #[test]
    fn windows_layout_nests_organisation_and_splits_one_base() {
        let local = base("local");
        let paths = resolved(Os::Windows, Some(local.clone()), Some(local.clone()));
        let project = local.join("ename").join("example-game");
        assert_eq!(
            paths.persistent_data_path(),
            Some(project.join("data").as_path())
        );
        assert_eq!(paths.cache_path(), Some(project.join("cache").as_path()));
        assert_eq!(paths.rejected_cache_path(), None);
    }

    #[test]
    fn macos_layout_uses_the_bundle_identifier() {
        let paths = resolved(Os::Mac, Some(base("support")), Some(base("caches")));
        assert_eq!(
            paths.persistent_data_path(),
            Some(base("support").join("com.ename.example-game").as_path())
        );
        assert_eq!(
            paths.cache_path(),
            Some(base("caches").join("com.ename.example-game").as_path())
        );
    }

    #[test]
    fn spaced_mixed_case_names_on_every_layout() {
        let org = OrganisationName::from_static("Alexander Verbeek");
        let project = ProjectName::from_static("Airship Game");
        let paths = |os| {
            let (data, _) = layout(os, Some(base("b")), Some(base("b")), &org, &project);
            data.unwrap()
        };
        assert_eq!(paths(Os::Linux), base("b").join("Airship Game"));
        assert_eq!(
            paths(Os::Windows),
            base("b")
                .join("Alexander Verbeek")
                .join("Airship Game")
                .join("data")
        );
        assert_eq!(
            paths(Os::Mac),
            base("b").join("com.alexanderverbeek.airshipgame")
        );
    }

    #[test]
    fn missing_base_drops_only_that_directory() {
        let paths = resolved(Os::Linux, None, Some(base("cache")));
        assert_eq!(paths.persistent_data_path(), None);
        assert!(paths.cache_path().is_some());

        let paths = resolved(Os::Linux, Some(base("data")), None);
        assert!(paths.persistent_data_path().is_some());
        assert_eq!(paths.cache_path(), None);
    }

    #[test]
    fn relative_base_is_not_found() {
        let paths = resolved(Os::Linux, Some("data".into()), Some(base("cache")));
        assert_eq!(paths.persistent_data_path(), None);
        assert!(paths.cache_path().is_some());
    }

    #[test]
    fn same_base_on_linux_drops_the_cache() {
        let shared = base("shared");
        let paths = resolved(Os::Linux, Some(shared.clone()), Some(shared.clone()));
        let project = shared.join("example-game");
        assert_eq!(paths.persistent_data_path(), Some(project.as_path()));
        assert_eq!(paths.cache_path(), None);
        assert_eq!(paths.rejected_cache_path(), Some(project.as_path()));
    }

    #[test]
    fn nested_paths_drop_the_cache() {
        let data = base("nested-data");
        let inside = Paths::from_paths(data.clone(), data.join("cache"));
        assert_eq!(inside.cache_path(), None);
        let outside = Paths::from_paths(data.join("saves"), data.clone());
        assert_eq!(outside.cache_path(), None);
        assert_eq!(outside.rejected_cache_path(), Some(data.as_path()));
    }

    #[test]
    fn parent_components_are_not_found() {
        let data = base("data");
        let paths = Paths::from_paths(data.join("..").join("data"), base("cache"));
        assert_eq!(paths.persistent_data_path(), None);
        assert!(paths.cache_path().is_some());
    }

    #[test]
    fn case_folding_catches_overlap_that_differs_only_by_case() {
        let data = base("Game");
        let cache = base("game").join("cache");
        let folded = Paths::checked(Some(data.clone()), Some(cache.clone()), true);
        assert_eq!(folded.cache_path(), None);
        let exact = Paths::checked(Some(data), Some(cache), false);
        assert!(exact.cache_path().is_some());
    }

    #[test]
    fn sibling_with_shared_prefix_is_not_nested() {
        let paths = Paths::from_paths(base("game"), base("game-cache"));
        assert!(paths.cache_path().is_some());
    }

    #[test]
    fn ensure_creates_accepts_existing_and_recreates() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::from_paths(root.path().join("data"), root.path().join("cache"));

        let data = paths.ensure_persistent_data_path().unwrap();
        assert!(data.is_dir());
        assert_eq!(paths.ensure_persistent_data_path().unwrap(), data);

        let cache = paths.ensure_cache_path().unwrap().to_path_buf();
        fs::remove_dir_all(&cache).unwrap();
        assert!(paths.ensure_cache_path().unwrap().is_dir());
    }

    #[test]
    fn ensure_fails_when_a_file_is_in_the_way() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        fs::write(&data, "").unwrap();
        let paths = Paths::from_paths(data, root.path().join("cache"));
        let err = paths.ensure_persistent_data_path().unwrap_err();
        assert!(err.to_string().contains("persistent data"), "{err}");
    }

    #[test]
    fn ensure_on_a_missing_path_is_not_found() {
        let err = Paths::default().ensure_cache_path().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
