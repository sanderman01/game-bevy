//! The package manifest: what a `manifest.toml` says about a package and the assets it declares.
//!
//! Plain data. Not a Bevy `Asset`: the scanner reads and parses these itself, which is what lets
//! the whole scan finish inside one async function with nothing to sequence.

use serde::{Deserialize, Serialize};
use std::{fmt::Display, str::FromStr};
use thiserror::Error;

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

    /// Package ids this package must load after. An ordering hint, not a dependency: an entry
    /// naming a package that is not installed is ignored, because a mod mentioning a popular
    /// optional package should not warn on every machine that does not have it. Use `requires`
    /// for a real dependency.
    #[serde(default)]
    pub after: Vec<String>,

    /// Package ids this package must load before. Same rules as [`PackageInfo::after`].
    #[serde(default)]
    pub before: Vec<String>,

    /// Packages that must be installed, at a version this one can work with. An unsatisfied entry
    /// disables this package and says why, and disabling cascades to whatever required it.
    #[serde(default)]
    pub requires: Vec<Requirement>,

    /// Packages this one means to shadow. Declaring it is not what makes an override happen --
    /// claiming the alias is. This is the statement of intent that turns a surprise into a
    /// warning: colliding with a package not named here warns, and naming a package this one
    /// never collides with warns too, which is what catches an alias typo.
    #[serde(default)]
    pub overrides: Vec<String>,

    /// Aliases this package takes out of the index as it loads. An array of strings rather than a
    /// table, so `::` is a value and never a TOML key.
    #[serde(default)]
    pub removes: Vec<String>,
}

impl Display for Manifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(id: {}, v: {})", self.package.id, self.package.version)
    }
}

/// One entry of [`PackageInfo::requires`]: a package id and the versions of it that will do.
///
/// Written as one string, `"core >= 2.0"`, because a table per requirement is three lines of TOML
/// for two values. The id ends at the first whitespace and everything after it is a
/// [`VersionReq`], so a bare `"core"` means any version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub req: VersionReq,
}

impl Requirement {
    /// True when `id` is the package this requires and `version` is one it accepts.
    pub fn matches(&self, id: &str, version: &Version) -> bool {
        self.id == id && self.req.matches(version)
    }
}

impl FromStr for Requirement {
    type Err = RequirementError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (id, rest) = match s.find(char::is_whitespace) {
            Some(at) => (&s[..at], s[at..].trim()),
            None => (s, ""),
        };
        validate_package_id(id).map_err(|err| RequirementError::Id(err.to_string()))?;
        let req = if rest.is_empty() {
            VersionReq::STAR
        } else {
            VersionReq::parse(rest).map_err(|source| RequirementError::Req {
                text: rest.to_owned(),
                source,
            })?
        };
        Ok(Self {
            id: id.to_owned(),
            req,
        })
    }
}

impl Display for Requirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.req == VersionReq::STAR {
            f.write_str(&self.id)
        } else {
            write!(f, "{} {}", self.id, self.req)
        }
    }
}

impl<'de> Deserialize<'de> for Requirement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Requirement {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Why a [`Requirement`] could not be read.
///
/// `Clone`, `PartialEq` and `Eq` are left off: `semver::Error` implements none of them, so
/// deriving those here would need to wrap it instead of holding it directly.
#[derive(Debug, Error)]
pub enum RequirementError {
    #[error("a requirement's package id is invalid: {0}")]
    Id(String),
    #[error("`{text}` is not a version requirement: {source}")]
    Req { text: String, source: semver::Error },
}

/// Why a package id was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PackageIdError {
    #[error("a package id must not be empty")]
    Empty,
    #[error("a package id must not contain whitespace: {0}")]
    Whitespace(String),
}

/// Checks that `id` can be named unambiguously wherever package ids are written.
///
/// The binding constraint is [`Requirement`]: it splits an id from its version range at the first
/// whitespace, so an id containing whitespace has no single reading. Rejecting it once, here, is
/// what lets every other place assume it.
pub fn validate_package_id(id: &str) -> Result<(), PackageIdError> {
    if id.is_empty() {
        return Err(PackageIdError::Empty);
    }
    if id.chars().any(char::is_whitespace) {
        return Err(PackageIdError::Whitespace(id.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Manifest, PackageIdError, Requirement, Version, validate_package_id};

    fn parse(extra: &str) -> Manifest {
        toml::from_str(&format!(
            r#"
            [package]
            id = "core"
            version = "2.0.0"
            authors = []
            title = "Core"
            description = ""
            {extra}
            "#
        ))
        .expect("the manifest parses")
    }

    /// Every constraint field is optional. A manifest written before phase 3 must keep parsing,
    /// and most manifests will never name any of them.
    #[test]
    fn the_constraint_fields_all_default_to_empty() {
        let manifest = parse("");
        assert!(manifest.package.after.is_empty());
        assert!(manifest.package.before.is_empty());
        assert!(manifest.package.requires.is_empty());
        assert!(manifest.package.overrides.is_empty());
        assert!(manifest.package.removes.is_empty());
    }

    #[test]
    fn ordering_constraints_parse_as_package_ids() {
        let manifest = parse(
            r#"after = ["core"]
            before = ["hats"]"#,
        );
        assert_eq!(manifest.package.after, ["core"]);
        assert_eq!(manifest.package.before, ["hats"]);
    }

    /// `removes` is an array of alias strings, never a TOML key, which is the whole reason it is
    /// spelled this way: `::` is a value here and needs no quoting rules.
    #[test]
    fn removes_carries_namespaced_aliases() {
        let manifest = parse(r#"removes = ["core::banana"]"#);
        assert_eq!(manifest.package.removes, ["core::banana"]);
    }

    #[test]
    fn a_requirement_splits_an_id_from_a_version_range() {
        let requirement: Requirement = "core >= 2.0".parse().expect("parses");
        assert_eq!(requirement.id, "core");
        assert!(requirement.matches("core", &Version::new(2, 1, 0)));
        assert!(!requirement.matches("core", &Version::new(1, 9, 0)));
        assert!(!requirement.matches("hats", &Version::new(2, 1, 0)));
    }

    /// A bare id means "any version", which is what an author writing `requires = ["core"]` means.
    #[test]
    fn a_requirement_without_a_range_accepts_any_version() {
        let requirement: Requirement = "core".parse().expect("parses");
        assert!(requirement.matches("core", &Version::new(0, 1, 0)));
    }

    #[test]
    fn a_requirement_round_trips_through_toml() {
        let manifest = parse(r#"requires = ["core >= 2.0", "hats"]"#);
        assert_eq!(manifest.package.requires.len(), 2);
        assert_eq!(manifest.package.requires[0].id, "core");
        assert_eq!(manifest.package.requires[1].id, "hats");
    }

    /// A `requires` entry splits at the first whitespace, so an id containing one has no
    /// unambiguous reading. Rejecting it at the manifest is what keeps that true everywhere else.
    #[test]
    fn a_package_id_may_not_contain_whitespace() {
        assert_eq!(validate_package_id("core"), Ok(()));
        assert_eq!(
            validate_package_id("big ships"),
            Err(PackageIdError::Whitespace("big ships".into()))
        );
        assert_eq!(validate_package_id(""), Err(PackageIdError::Empty));
    }
}
