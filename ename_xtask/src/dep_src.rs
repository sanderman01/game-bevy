//! `ename_dep_src` -- where cargo reads a locked dependency's source from.
//!
//! Reads `cargo metadata` instead of `Cargo.lock`: metadata already names each resolved package's
//! `manifest_path`, so git checkout directories need no reimplementation of cargo's path hashing.

use serde::Deserialize;
use std::cmp::Ordering;
use std::fmt::Display;
use std::path::PathBuf;

/// The part of `cargo metadata --format-version 1` this subcommand reads.
#[derive(Debug, Deserialize)]
pub struct Metadata {
    packages: Vec<Package>,
}

impl Metadata {
    pub fn from_json(json: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(json)
    }
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    version: String,
    /// `None` for workspace members and path dependencies.
    source: Option<String>,
    manifest_path: PathBuf,
}

/// Where one resolved version of a package lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSource {
    pub name: String,
    pub version: String,
    pub kind: SourceKind,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    Registry,
    Git { rev: String },
    Path,
}

impl SourceKind {
    fn parse(source: Option<&str>) -> Self {
        let Some(source) = source else {
            return Self::Path;
        };
        let Some(git) = source.strip_prefix("git+") else {
            return Self::Registry;
        };
        let (url, sha) = git.split_once('#').unwrap_or((git, ""));
        let rev = url
            .split_once('?')
            .and_then(|(_, query)| query.split('&').find_map(|kv| kv.strip_prefix("rev=")))
            .map(str::to_owned)
            .unwrap_or_else(|| sha.chars().take(7).collect());
        Self::Git { rev }
    }
}

impl Display for DepSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} ", self.name, self.version)?;
        match &self.kind {
            SourceKind::Registry => write!(f, "registry")?,
            SourceKind::Git { rev } => write!(f, "git {rev}")?,
            SourceKind::Path => write!(f, "path")?,
        }
        write!(f, " {}", self.dir.display())
    }
}

/// Cargo treats `-` and `_` in package names as the same name.
fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

/// Every resolved version of `name`, oldest first.
pub fn find(metadata: &Metadata, name: &str) -> Vec<DepSource> {
    let wanted = normalize(name);
    let mut found: Vec<DepSource> = metadata
        .packages
        .iter()
        .filter(|p| normalize(&p.name) == wanted)
        .map(|p| DepSource {
            name: p.name.clone(),
            version: p.version.clone(),
            kind: SourceKind::parse(p.source.as_deref()),
            dir: p
                .manifest_path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default(),
        })
        .collect();
    found.sort_by(|a, b| compare_versions(&a.version, &b.version));
    found
}

/// Up to `limit` locked package names that contain `name`, sorted and without duplicates.
pub fn near_misses(metadata: &Metadata, name: &str, limit: usize) -> Vec<String> {
    let wanted = normalize(name);
    let mut names: Vec<String> = metadata
        .packages
        .iter()
        .filter(|p| normalize(&p.name).contains(&wanted))
        .map(|p| p.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names.truncate(limit);
    names
}

/// Numeric comparison of the `major.minor.patch` core, then the full string.
fn compare_versions(a: &str, b: &str) -> Ordering {
    fn core(version: &str) -> Vec<u64> {
        version
            .split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    }
    core(a).cmp(&core(b)).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::{DepSource, Metadata, SourceKind, find, near_misses};

    const METADATA: &str = r#"{"packages": [
        {"name": "big_space", "version": "0.13.0",
         "source": "git+https://github.com/aevyrie/big_space?rev=5f25522#5f255228e9b4ff30756347b80f92b19ebf7b5c56",
         "manifest_path": "/git/big_space/5f25522/Cargo.toml"},
        {"name": "bevy_ecs", "version": "0.19.1",
         "source": "registry+https://github.com/rust-lang/crates.io-index",
         "manifest_path": "/reg/bevy_ecs-0.19.1/Cargo.toml"},
        {"name": "bitflags", "version": "2.10.0",
         "source": "registry+https://github.com/rust-lang/crates.io-index",
         "manifest_path": "/reg/bitflags-2.10.0/Cargo.toml"},
        {"name": "bitflags", "version": "1.3.2",
         "source": "registry+https://github.com/rust-lang/crates.io-index",
         "manifest_path": "/reg/bitflags-1.3.2/Cargo.toml"},
        {"name": "ename-local", "version": "0.0.0", "source": null,
         "manifest_path": "/ws/ename-local/Cargo.toml"},
        {"name": "on_branch", "version": "0.1.0",
         "source": "git+https://example.com/x?branch=main#abcdef0123456789",
         "manifest_path": "/git/x/abcdef0/Cargo.toml"}
    ]}"#;

    fn metadata() -> Metadata {
        Metadata::from_json(METADATA.as_bytes()).unwrap()
    }

    #[test]
    fn git_source_uses_the_rev_query_value() {
        let found = find(&metadata(), "big_space");
        assert_eq!(
            found,
            vec![DepSource {
                name: "big_space".into(),
                version: "0.13.0".into(),
                kind: SourceKind::Git {
                    rev: "5f25522".into()
                },
                dir: "/git/big_space/5f25522".into(),
            }]
        );
        assert_eq!(
            found[0].to_string(),
            "big_space 0.13.0 git 5f25522 /git/big_space/5f25522"
        );
    }

    #[test]
    fn git_source_without_rev_uses_the_short_sha() {
        let found = find(&metadata(), "on_branch");
        assert_eq!(
            found[0].kind,
            SourceKind::Git {
                rev: "abcdef0".into()
            }
        );
    }

    #[test]
    fn dashes_and_underscores_match_each_other() {
        assert_eq!(find(&metadata(), "bevy-ecs").len(), 1);
        let local = find(&metadata(), "ename_local");
        assert_eq!(local[0].kind, SourceKind::Path);
        assert_eq!(
            local[0].to_string(),
            "ename-local 0.0.0 path /ws/ename-local"
        );
    }

    #[test]
    fn several_versions_sort_oldest_first() {
        let versions: Vec<String> = find(&metadata(), "bitflags")
            .into_iter()
            .map(|d| d.version)
            .collect();
        assert_eq!(versions, ["1.3.2", "2.10.0"]);
    }

    #[test]
    fn near_misses_are_substring_matches_only() {
        assert!(find(&metadata(), "bevy_ec").is_empty());
        assert_eq!(near_misses(&metadata(), "bevy_ec", 10), ["bevy_ecs"]);
        assert!(near_misses(&metadata(), "bevy_ecz", 10).is_empty());
        assert_eq!(
            near_misses(&metadata(), "bi", 10),
            ["big_space", "bitflags"]
        );
    }
}
