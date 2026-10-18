//! What this project scans, mirroring `example_game_editor_bin`'s own `main.rs`.
//!
//! The asset root, the search paths, and the names that place the user's load order file are game
//! policy, not something `ename_asset_package` or `ename_asset_alias` could derive on their own --
//! the same reason `example_game_editor_bin`'s `main` passes them to `EnginePlugins` instead of
//! the engine defaulting to them. `ename_xtask` is a second target reading the same tree, so it
//! declares the same policy rather than importing it: `example_game_lib` links Bevy. The per-OS
//! layout itself is shared through `ename_application`, so only the names are copied.

use ename_application::{NameError, OrganisationName, Paths, ProjectName};
use std::{fmt, path::PathBuf};

/// Relative to the directory `cargo xtask` is run from, which is the workspace root for every
/// normal invocation. Mirrors `AssetPlugin::file_path`'s default and `example_game_editor_bin`'s
/// own asset root.
pub const ASSET_ROOT: &str = "assets";

/// Mirrors both example binaries' `with_content_search_paths(["basegame", "mods", "examples"])`. A
/// missing search path is only a warning, so a game without `assets/examples/` is unaffected.
pub const SEARCH_PATHS: [&str; 3] = ["basegame", "mods", "examples"];

/// Override the example's names, for a game on this engine with its own persistent data
/// directory. Both or neither.
pub const ORGANISATION_NAME_ENV: &str = "ENAME_ORGANISATION_NAME";
pub const PROJECT_NAME_ENV: &str = "ENAME_PROJECT_NAME";

/// Mirror `example_game_lib::ORGANISATION_NAME` and `PROJECT_NAME`.
const ORGANISATION_NAME: OrganisationName = OrganisationName::from_static("ename");
const PROJECT_NAME: ProjectName = ProjectName::from_static("example-game");

/// [`SEARCH_PATHS`], owned: `scan_packages` takes `&[String]`.
pub fn search_paths() -> Vec<String> {
    SEARCH_PATHS.iter().map(|s| (*s).to_owned()).collect()
}

/// Why the name override could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamesError {
    /// One of the two variables is set without the other.
    HalfSet {
        set: &'static str,
        missing: &'static str,
    },
    Invalid(NameError),
}

impl fmt::Display for NamesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HalfSet { set, missing } => write!(f, "{set} is set but {missing} is not"),
            Self::Invalid(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for NamesError {}

/// The names that place the load order file: the override when both values are given, else the
/// example's own. Takes the values rather than reading the environment, so a test can call it.
pub fn project_names(
    organisation: Option<String>,
    project: Option<String>,
) -> Result<(OrganisationName, ProjectName), NamesError> {
    match (organisation, project) {
        (None, None) => Ok((ORGANISATION_NAME, PROJECT_NAME)),
        (Some(organisation), Some(project)) => Ok((
            OrganisationName::new(organisation).map_err(NamesError::Invalid)?,
            ProjectName::new(project).map_err(NamesError::Invalid)?,
        )),
        (Some(_), None) => Err(NamesError::HalfSet {
            set: ORGANISATION_NAME_ENV,
            missing: PROJECT_NAME_ENV,
        }),
        (None, Some(_)) => Err(NamesError::HalfSet {
            set: PROJECT_NAME_ENV,
            missing: ORGANISATION_NAME_ENV,
        }),
    }
}

/// Where the user's load order file lives: the project's persistent data directory, as the
/// engine's default. `None` when the platform has none.
pub fn load_order_path(organisation: &OrganisationName, project: &ProjectName) -> Option<PathBuf> {
    Some(
        Paths::resolve(organisation, project)
            .persistent_data_path()?
            .join(ename_asset_package::LOAD_ORDER_FILE),
    )
}

#[cfg(test)]
mod tests {
    use super::{NamesError, ORGANISATION_NAME_ENV, PROJECT_NAME_ENV, project_names};

    #[test]
    fn no_override_keeps_the_example_names() {
        let (organisation, project) = project_names(None, None).unwrap();
        assert_eq!(organisation.as_str(), "ename");
        assert_eq!(project.as_str(), "example-game");
    }

    #[test]
    fn a_full_override_replaces_both_names() {
        let (organisation, project) =
            project_names(Some("Studio".into()), Some("my-game".into())).unwrap();
        assert_eq!(organisation.as_str(), "Studio");
        assert_eq!(project.as_str(), "my-game");
    }

    #[test]
    fn a_half_set_override_is_an_error() {
        assert_eq!(
            project_names(Some("Studio".into()), None),
            Err(NamesError::HalfSet {
                set: ORGANISATION_NAME_ENV,
                missing: PROJECT_NAME_ENV
            })
        );
        assert_eq!(
            project_names(None, Some("my-game".into())),
            Err(NamesError::HalfSet {
                set: PROJECT_NAME_ENV,
                missing: ORGANISATION_NAME_ENV
            })
        );
    }

    #[test]
    fn an_invalid_name_is_an_error() {
        assert!(matches!(
            project_names(Some("Studio".into()), Some("my.game".into())),
            Err(NamesError::Invalid(_))
        ));
        assert!(matches!(
            project_names(Some(String::new()), Some("my-game".into())),
            Err(NamesError::Invalid(_))
        ));
    }
}
