//! The package manifest: what a `manifest.toml` says about a package and the assets it declares.
//!
//! Plain data. Not a Bevy `Asset`: the scanner reads and parses these itself, which is what lets
//! the whole scan finish inside one async function with nothing to sequence.

#[cfg(feature = "bevy")]
use bevy::reflect::Reflect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt::Display, str::FromStr};

/// A package manifest: the entry point of a base game, mod, DLC or other content package.
///
/// It no longer declares assets. An asset's alias comes from a `.alias` file beside it or from the
/// `_rules.toml` covering its folder, which is what removed the hand-written list that did not
/// scale and the quoted TOML keys that made a typo silent.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(Reflect))]
pub struct Manifest {
    pub package: PackageInfo,
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
