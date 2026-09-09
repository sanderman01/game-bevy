//! The alias index: a validated alias, and where it resolves to under the asset root.
//!
//! No Bevy io here on purpose. Resolution is a lookup, and keeping it that way lets it be tested
//! without an `App`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Why an alias was rejected.
///
/// Every variant is a case where `AssetPath` would have parsed the alias into something other
/// than the alias, so the failure has to happen at registration rather than at load.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AliasError {
    #[error("an alias must not be empty")]
    Empty,
    #[error("an alias must not contain `://`, which `AssetPath` reads as an asset source: {0}")]
    SourceDelimiter(String),
    #[error("an alias must not contain `#`, which `AssetPath` reads as a label: {0}")]
    LabelDelimiter(String),
    #[error("an alias must not contain whitespace: {0}")]
    Whitespace(String),
}

/// Checks that `alias` survives a round trip through `AssetPath` unchanged.
pub fn validate_alias(alias: &str) -> Result<(), AliasError> {
    if alias.is_empty() {
        return Err(AliasError::Empty);
    }
    if alias.contains("://") {
        return Err(AliasError::SourceDelimiter(alias.to_owned()));
    }
    if alias.contains('#') {
        return Err(AliasError::LabelDelimiter(alias.to_owned()));
    }
    if alias.chars().any(char::is_whitespace) {
        return Err(AliasError::Whitespace(alias.to_owned()));
    }
    Ok(())
}

/// Maps a package-namespaced alias such as `core::airship` to a path under the asset root.
///
/// The live copy the reader resolves through lives in a `ContentIndexCell`, not in the `World`:
/// an `AssetReader` cannot reach a resource. `ename_asset_content` mirrors the finished index in
/// here as well, for the editor and the log to read. That copy is for inspection only.
#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "bevy", derive(bevy::ecs::resource::Resource))]
pub struct ContentIndex {
    alias_to_path: HashMap<String, PathBuf>,
}

impl ContentIndex {
    /// Registers `alias`, returning the path it displaced. A later package overriding an earlier
    /// one is the point of the system, so replacing is not an error.
    pub fn insert(
        &mut self,
        alias: &str,
        path: impl Into<PathBuf>,
    ) -> Result<Option<PathBuf>, AliasError> {
        validate_alias(alias)?;
        Ok(self.alias_to_path.insert(alias.to_owned(), path.into()))
    }

    pub fn remove(&mut self, alias: &str) -> Option<PathBuf> {
        self.alias_to_path.remove(alias)
    }

    pub fn resolve(&self, alias: &str) -> Option<&Path> {
        self.alias_to_path.get(alias).map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.alias_to_path.len()
    }

    pub fn is_empty(&self) -> bool {
        self.alias_to_path.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.alias_to_path
            .iter()
            .map(|(alias, path)| (alias.as_str(), path.as_path()))
    }
}

#[cfg(test)]
mod tests {
    use super::{AliasError, ContentIndex, validate_alias};
    use std::path::Path;

    #[test]
    fn accepts_a_namespaced_alias() {
        assert_eq!(validate_alias("core::airship"), Ok(()));
        assert_eq!(validate_alias("core::props/barrel"), Ok(()));
    }

    /// `AssetPath` splits on the first `://`, so an alias containing one would be parsed as a
    /// second source and silently resolve somewhere else.
    #[test]
    fn rejects_an_alias_that_would_parse_as_a_source() {
        assert_eq!(
            validate_alias("core::http://x"),
            Err(AliasError::SourceDelimiter("core::http://x".into()))
        );
    }

    /// `#` opens the label section of an `AssetPath`, so an alias containing one would truncate.
    #[test]
    fn rejects_an_alias_containing_a_label_delimiter() {
        assert_eq!(
            validate_alias("core::airship#Scene0"),
            Err(AliasError::LabelDelimiter("core::airship#Scene0".into()))
        );
    }

    #[test]
    fn rejects_an_empty_or_whitespace_alias() {
        assert_eq!(validate_alias(""), Err(AliasError::Empty));
        assert_eq!(
            validate_alias("core:: airship"),
            Err(AliasError::Whitespace("core:: airship".into()))
        );
    }

    #[test]
    fn resolves_an_inserted_alias() {
        let mut index = ContentIndex::default();
        index
            .insert("core::airship", "basegame/core/airship.glb")
            .unwrap();
        assert_eq!(
            index.resolve("core::airship"),
            Some(Path::new("basegame/core/airship.glb"))
        );
        assert_eq!(index.resolve("core::missing"), None);
        assert_eq!(index.len(), 1);
    }

    /// A later package overriding an earlier one is the whole point, so insert replaces and
    /// hands back what it displaced rather than failing.
    #[test]
    fn insert_replaces_and_returns_the_previous_path() {
        let mut index = ContentIndex::default();
        index
            .insert("core::airship", "basegame/core/airship.glb")
            .unwrap();
        let previous = index
            .insert("core::airship", "mods/bigships/airship.glb")
            .unwrap();
        assert_eq!(
            previous.as_deref(),
            Some(Path::new("basegame/core/airship.glb"))
        );
        assert_eq!(
            index.resolve("core::airship"),
            Some(Path::new("mods/bigships/airship.glb"))
        );
    }

    #[test]
    fn insert_rejects_an_invalid_alias_without_storing_it() {
        let mut index = ContentIndex::default();
        assert!(index.insert("bad#alias", "x.glb").is_err());
        assert!(index.is_empty());
    }

    #[test]
    fn remove_reports_whether_anything_was_there() {
        let mut index = ContentIndex::default();
        index
            .insert("core::banana", "basegame/core/banana.glb")
            .unwrap();
        assert!(index.remove("core::banana").is_some());
        assert!(index.remove("core::banana").is_none());
    }
}
