//! The package manifest: what a `manifest.toml` says about a package and the assets it declares.
//!
//! Plain data. Not a Bevy `Asset`: the scanner reads and parses these itself, which is what lets
//! the whole scan finish inside one async function with nothing to sequence.

use serde::{Deserialize, Serialize};
use std::fmt::Display;

/// A package version, and the requirement syntax `requires` is written in.
///
/// This is Cargo's flavour of semver, from the `semver` crate, rather than a type of our own. A
/// `requires` entry needs a version *requirement* and not just a version, and reimplementing
/// `>= 2.0`, `^1`, `1.2.*` and prerelease ordering would be a private dialect that looks exactly
/// like the one every author already knows and behaves subtly differently.
pub use semver::{Version, VersionReq};

/// A package manifest: the entry point of a base game, mod, DLC or other content package.
///
/// It no longer declares assets. An asset's alias comes from a `.alias` file beside it or from the
/// `_alias_rules.toml` covering its folder, which is what removed the hand-written list that did
/// not
/// scale and the quoted TOML keys that made a typo silent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub package: PackageInfo,
}

/// Package section of a package [Manifest](Manifest). Contains all the information required to unique identify and load a package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageInfo {
    /// Unique human readable mod package identifier. eg. 'spacewar'
    pub id: String,

    /// Semantic version eg:
    /// - 1.0.0 -> 1.0.1 (patch change)
    /// - 1.0.0 -> 1.1.0 (minor change)
    /// - 1.0.0 -> 2.0.0 (major change and/or breaking changes)
    ///
    /// See also: <https://semver.org/>
    pub version: Version,

    /// Package authors list.
    pub authors: Vec<String>,

    /// Human friendly name of this mod package.
    pub title: String,

    /// Describes the contents and/or functionality included in this mod package.
    pub description: String,
}

impl Display for Manifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(id: {}, v: {})", self.package.id, self.package.version)
    }
}
