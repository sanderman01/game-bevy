use bevy::{
    asset::{Asset, AssetLoader, LoadContext, io::Reader},
    prelude::*,
    reflect::{Reflect, TypePath},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml::de::Error as TomlError;
use std::{fmt::Display, str::FromStr};
use thiserror::Error;

#[derive(Default, Debug, Clone, Resource, Reflect)]
#[reflect(Default, Debug, Clone, Resource)]
pub struct Manifests {
    pub items: Vec<Handle<Manifest>>,
}

/// A Mod Package Manifest contains metadata defining a game base content, mod or dlc or other extension package.
///
/// The manifest can be considered the 'entry-point' of the package.
/// Similar to a shipping manifest document, this file contains data such as the id, name, description, authors and other relevant information used to identify the mod package and its origin.
///
/// Additionally the manifest contains lists declaring all assets in the package and how they are to be inserted and used inside the game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Reflect, Asset)]
pub struct Manifest {
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

    /// Load priority. Higher values = loaded later.
    /// This can be useful if one mod wants to make changes to game content specifically before or after others.
    pub load_priority: i32,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            id: String::new(),
            version: Version::default(),
            authors: Vec::<String>::new(),
            title: String::new(),
            description: String::new(),
            load_priority: 0,
        }
    }
}

impl Display for Manifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string())
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
#[derive(Debug, Default, Copy, Clone, PartialEq, Reflect)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major: major,
            minor: minor,
            patch: patch,
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
            major: major,
            minor: minor,
            patch: patch,
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
        Version::from_str(&s)
            .map(Self::from)
            .map_err(|e| serde::de::Error::custom(e.to_string()))
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

#[derive(Default, TypePath)]
pub struct ManifestAssetLoader;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ManifestAssetLoaderError {
    /// An [IO](std::io) Error
    #[error("Could not load asset: {0}")]
    Io(#[from] std::io::Error),
    ///# A [TOML](toml::de) TomlError
    #[error("Could not parse toml: {0}")]
    TomlError(#[from] TomlError),
}

impl AssetLoader for ManifestAssetLoader {
    type Asset = Manifest;
    type Settings = ();
    type Error = ManifestAssetLoaderError;
    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        //let custom_asset = ron::de::from_bytes::<Manifest>(&bytes)?;
        let asset: Manifest = toml::from_slice(&bytes)?;
        Ok(asset)
    }

    fn extensions(&self) -> &[&str] {
        &["custom"]
    }
}
