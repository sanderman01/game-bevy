//! The package manifest: what a `manifest.toml` says about a package and the assets it declares.
//!
//! Plain data. Not a Bevy `Asset`: the scanner reads and parses these itself, which is what lets
//! the whole scan finish inside one async function with nothing to sequence.

#[cfg(feature = "bevy")]
use bevy::reflect::Reflect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::{fmt::Display, str::FromStr};

/// A Mod Package Manifest contains metadata defining a base game content, or mod, or dlc, or other extension package.
///
/// The manifest can be considered the 'entry-point' of the package.
/// Similar to a shipping manifest document, this file contains data such as the id, name, description, authors and other relevant information used to identify the mod package and its origin.
///
/// Additionally the manifest contains information declaring all assets in the package and how they are to be inserted and used inside the game.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(Reflect))]
pub struct Manifest {
    pub package: PackageInfo,
    /// Absent entirely for a package that only carries assets a folder rule or a `.alias`
    /// sidecar already names, which is the common case. `AssetsInfo`'s own fields are all
    /// optional; this is the same default at the table level.
    #[serde(default)]
    pub assets: AssetsInfo,
}

/// Package section of a package [Manifest](Manifest). Contains all the information required to unique identify and load a package.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(Reflect))]
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

/// Assets section of a package [Manifest](Manifest). Used to declare assets added or modified by this package.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(Reflect))]
pub struct AssetsInfo {
    pub add: Option<HashMap<String, String>>,
    pub replace: Option<HashMap<String, String>>,
    pub remove: Option<HashMap<String, String>>,
}

impl Display for Manifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(id: {}, v: {})", self.package.id, self.package.version)
    }
}

#[derive(Debug)]
pub enum VersionError {
    Parse(String),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionError::Parse(s) => write!(
                f,
                "parse error: {}. version format should be 'major.minor.patch' eg. '1.2.3'",
                s
            ),
        }
    }
}

/// Semantic version eg:
/// - 1.0.0 -> 1.0.1 (patch change)
/// - 1.0.0 -> 1.1.0 (minor change)
/// - 1.0.0 -> 2.0.0 (major change and/or breaking changes)
///
/// See also: <https://semver.org/>
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "bevy", derive(Reflect))]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sss: Vec<&str> = s.split('.').collect();
        if sss.len() != 3 {
            return Result::Err(VersionError::Parse(s.into()));
        }

        let major = u64::from_str(sss[0]).map_err(|e| VersionError::Parse(e.to_string()))?;
        let minor = u64::from_str(sss[1]).map_err(|e| VersionError::Parse(e.to_string()))?;
        let patch = u64::from_str(sss[2]).map_err(|e| VersionError::Parse(e.to_string()))?;
        Result::Ok(Version {
            major,
            minor,
            patch,
        })
    }
}

impl Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = format!("{}.{}.{}", self.major, self.minor, self.patch);
        f.write_str(&s)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Version::from_str(&s).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
