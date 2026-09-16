//! The user's load order file: the one place a player, rather than an author, orders packages.
//!
//! It lives outside the asset tree, so it is read with `std::fs` through a path the target
//! supplies rather than through the [`crate::Vfs`] the scan uses. Which directory that path comes
//! from is platform policy and belongs to the binary: `example_game_editor_bin` computes it from
//! `dirs::config_dir`,
//! and a test passes a fixture path.
//!
//! There is no sequence in here, deliberately. A total order over the installed set goes stale the
//! moment a package is added or removed; a constraint keeps meaning what it said.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The file name, under whichever config directory the target picked.
pub const LOAD_ORDER_FILE: &str = "load_order.toml";

/// Everything the user said about load order.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadOrder {
    /// `[[constraint]]` in the file. Renamed because the TOML reads better in the singular and the
    /// field reads better in the plural.
    #[serde(default, rename = "constraint")]
    pub constraints: Vec<UserConstraint>,
}

/// One `[[constraint]]`: where the user wants one package to sit relative to others.
///
/// The same two fields a package author writes, so there is one concept to learn rather than two.
/// The difference is who wins: a user constraint overrules a manifest constraint that contradicts
/// it, because the person running the game is the one who has to live with the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserConstraint {
    pub package: String,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
}

/// Why a load order file could not be read.
#[derive(Debug, Error)]
pub enum LoadOrderError {
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("{0}")]
    Parse(#[from] toml::de::Error),
}

impl LoadOrder {
    pub fn parse(text: &str) -> Result<Self, LoadOrderError> {
        Ok(toml::from_str(text)?)
    }

    /// Reads the file at `path`. A missing file is not an error -- most players never write one.
    pub fn read_from_path(path: &Path) -> Result<Self, LoadOrderError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(LoadOrderError::Io {
                path: path.to_path_buf(),
                message: err.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LoadOrder;

    #[test]
    fn an_empty_file_is_a_valid_load_order() {
        assert_eq!(LoadOrder::parse("").expect("parses"), LoadOrder::default());
    }

    #[test]
    fn a_constraint_names_a_package_and_what_it_comes_after() {
        let order = LoadOrder::parse(
            r#"
            [[constraint]]
            package = "bigships"
            after = ["pbroverhaul"]
            "#,
        )
        .expect("parses");

        assert_eq!(order.constraints.len(), 1);
        assert_eq!(order.constraints[0].package, "bigships");
        assert_eq!(order.constraints[0].after, ["pbroverhaul"]);
        assert!(order.constraints[0].before.is_empty());
    }

    #[test]
    fn several_constraints_keep_the_order_they_were_written_in() {
        let order = LoadOrder::parse(
            r#"
            [[constraint]]
            package = "a"
            before = ["b"]

            [[constraint]]
            package = "c"
            after = ["b"]
            "#,
        )
        .expect("parses");

        let packages: Vec<&str> = order
            .constraints
            .iter()
            .map(|c| c.package.as_str())
            .collect();
        assert_eq!(packages, ["a", "c"]);
    }

    #[test]
    fn a_malformed_file_is_an_error_rather_than_an_empty_order() {
        assert!(LoadOrder::parse("[[constraint]] this is not toml [[[").is_err());
    }

    /// The normal case: most players never write this file. A missing one must read as "no
    /// constraints" and not as a failure, or every fresh install starts with a problem.
    #[test]
    fn a_missing_file_reads_as_an_empty_order() {
        let missing = std::path::Path::new("does/not/exist/load_order.toml");
        assert_eq!(
            LoadOrder::read_from_path(missing).expect("a missing file is not an error"),
            LoadOrder::default()
        );
    }
}
