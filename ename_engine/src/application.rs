//! The project's per-user directories as a resource. The names and the per-OS layout live in
//! `ename_application`, re-exported here, so `ename_xtask` shares them without linking Bevy.

use bevy::prelude::*;
use std::{io, path::Path};

pub use ename_application::*;

/// This run's names and per-user directories. A path is `None` only when the OS has no such
/// directory (no home, an unsupported target).
#[derive(Resource, Clone, Debug)]
pub struct ApplicationPaths {
    organisation: OrganisationName,
    project: ProjectName,
    paths: Paths,
}

impl ApplicationPaths {
    /// `paths` wins over resolving from the names, for tests and throwaway agent sessions.
    pub fn new(organisation: OrganisationName, project: ProjectName, paths: Option<Paths>) -> Self {
        let paths = paths.unwrap_or_else(|| Paths::resolve(&organisation, &project));
        Self {
            organisation,
            project,
            paths,
        }
    }

    pub fn organisation(&self) -> &OrganisationName {
        &self.organisation
    }

    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    pub fn persistent_data_path(&self) -> Option<&Path> {
        self.paths.persistent_data_path()
    }

    pub fn cache_path(&self) -> Option<&Path> {
        self.paths.cache_path()
    }

    /// See [`Paths::ensure_persistent_data_path`].
    pub fn ensure_persistent_data_path(&self) -> io::Result<&Path> {
        self.paths.ensure_persistent_data_path()
    }

    /// See [`Paths::ensure_cache_path`].
    pub fn ensure_cache_path(&self) -> io::Result<&Path> {
        self.paths.ensure_cache_path()
    }
}

/// Inserts [`ApplicationPaths`], logs where they point, and creates both directories if they are
/// missing. Creation happens in `build`, so the directories exist before any `Startup` system; a
/// failure is a warning, and writers still call `ensure_*` since the user may delete them later.
/// Must build after `LogPlugin`, or the log lines are lost.
pub struct ApplicationPlugin {
    pub paths: ApplicationPaths,
}

impl Plugin for ApplicationPlugin {
    fn build(&self, app: &mut App) {
        let paths = &self.paths;
        let data = display(paths.persistent_data_path());
        let cache = display(paths.cache_path());
        info!("project: {}", paths.project());
        info!("organisation: {}", paths.organisation());
        info!("persistent data path: {data}");
        info!("cache path: {cache}");

        if let Some(rejected) = paths.paths.rejected_cache_path() {
            warn!(
                "no cache directory: {} overlaps the persistent data directory, so clearing it \
                 could delete saves",
                rejected.display()
            );
        }
        if paths.persistent_data_path().is_some()
            && let Err(err) = paths.ensure_persistent_data_path()
        {
            warn!("{err}");
        }
        if paths.cache_path().is_some()
            && let Err(err) = paths.ensure_cache_path()
        {
            warn!("{err}");
        }
        app.insert_resource(paths.clone());
    }
}

fn display(path: Option<&Path>) -> String {
    path.map_or_else(
        || "unavailable".to_owned(),
        |path| path.display().to_string(),
    )
}
