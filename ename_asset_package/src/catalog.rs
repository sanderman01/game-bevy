//! The content catalog: which descriptors of each registered kind the resolved index holds.
//!
//! A kind is recognised by its file extension (`map.ron`), and packages list nothing. The catalog is
//! derived from the alias index after load order, `overrides` and `removes`, never from a second
//! walk of the files, so it can only ever agree with what `alias://` resolves. The fold that builds
//! it is [`crate::build_index_with_catalog`].
//!
//! Plain data, no Bevy: `ename_xtask` builds the same catalog with a kind table read from the
//! workspace manifest, and `ename_asset_content` with the kinds the game registered.

use std::{collections::BTreeMap, path::Path};
use thiserror::Error;

/// Why a kind could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContentKindError {
    #[error("a content kind needs a name")]
    EmptyName,
    #[error("content kind `{kind}` needs an extension without a leading dot, not `{extension}`")]
    BadExtension { kind: String, extension: String },
    #[error("content kind `{0}` is registered twice")]
    DuplicateKind(String),
    #[error("extension `{extension}` of `{kind}` is already the extension of `{existing}`")]
    DuplicateExtension {
        kind: String,
        extension: String,
        existing: String,
    },
}

/// The registered kinds and the file extension that marks each.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ContentKinds {
    /// `(kind, extension)`, in registration order.
    kinds: Vec<(String, String)>,
}

impl ContentKinds {
    /// Registers `kind` for files ending in `.{extension}`. The kind name is opaque to the engine.
    pub fn insert(
        &mut self,
        kind: impl Into<String>,
        extension: impl Into<String>,
    ) -> Result<(), ContentKindError> {
        let (kind, extension) = (kind.into(), extension.into());
        if kind.is_empty() {
            return Err(ContentKindError::EmptyName);
        }
        if extension.is_empty() || extension.starts_with('.') || extension.ends_with('.') {
            return Err(ContentKindError::BadExtension { kind, extension });
        }
        if self.kinds.iter().any(|(existing, _)| *existing == kind) {
            return Err(ContentKindError::DuplicateKind(kind));
        }
        if let Some((existing, _)) = self.kinds.iter().find(|(_, ext)| *ext == extension) {
            return Err(ContentKindError::DuplicateExtension {
                existing: existing.clone(),
                kind,
                extension,
            });
        }
        self.kinds.push((kind, extension));
        Ok(())
    }

    /// The kind of the file at `path`, if any. Matched like Bevy matches a loader: on the file
    /// name's extensions, case-sensitive, the longest registered one winning (`a.map.ron` is a
    /// `map.ron` before it is a `ron`).
    pub fn kind_of(&self, path: &Path) -> Option<&str> {
        let name = path.file_name()?.to_str()?;
        self.kinds
            .iter()
            .filter(|(_, extension)| {
                name.len() > extension.len()
                    && name.ends_with(extension.as_str())
                    && name.as_bytes()[name.len() - extension.len() - 1] == b'.'
            })
            .max_by_key(|(_, extension)| extension.len())
            .map(|(kind, _)| kind.as_str())
    }

    /// `(kind, extension)`, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.kinds
            .iter()
            .map(|(kind, extension)| (kind.as_str(), extension.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}

/// One descriptor in the catalog. `id` is its alias, so `alias://{id}` loads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub kind: String,
    pub id: String,
    /// The package whose file won the alias.
    pub package: String,
}

/// Every descriptor of every registered kind, keyed `(kind, id)`.
///
/// In a running game the resource appears once every descriptor has loaded or failed; a failed
/// one is reported and left out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bevy", derive(bevy::ecs::resource::Resource))]
pub struct ContentCatalog {
    kinds: ContentKinds,
    /// Kind, then id. Every registered kind has a map, empty or not.
    entries: BTreeMap<String, BTreeMap<String, CatalogEntry>>,
}

impl ContentCatalog {
    /// An empty catalog for `kinds`.
    pub fn new(kinds: ContentKinds) -> Self {
        let entries = kinds
            .iter()
            .map(|(kind, _)| (kind.to_owned(), BTreeMap::new()))
            .collect();
        Self { kinds, entries }
    }

    /// The kinds this catalog was built for.
    pub fn kinds(&self) -> &ContentKinds {
        &self.kinds
    }

    /// Every entry of `kind`, sorted by id. Empty for an unknown kind.
    pub fn entries(&self, kind: &str) -> impl Iterator<Item = &CatalogEntry> {
        self.entries
            .get(kind)
            .into_iter()
            .flat_map(BTreeMap::values)
    }

    pub fn get(&self, kind: &str, id: &str) -> Option<&CatalogEntry> {
        self.entries.get(kind)?.get(id)
    }

    /// Every entry, sorted by kind and then id.
    pub fn iter(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.values().flat_map(BTreeMap::values)
    }

    pub fn len(&self) -> usize {
        self.entries.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.values().all(BTreeMap::is_empty)
    }

    /// Adds or replaces the entry with the same kind and id. Ignored for an unregistered kind.
    pub(crate) fn insert(&mut self, entry: CatalogEntry) {
        if let Some(kind) = self.entries.get_mut(&entry.kind) {
            kind.insert(entry.id.clone(), entry);
        }
    }

    /// Takes an entry out, for a descriptor that failed to load.
    pub fn remove(&mut self, kind: &str, id: &str) -> Option<CatalogEntry> {
        self.entries.get_mut(kind)?.remove(id)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentKindError, ContentKinds};
    use std::path::Path;

    fn kinds() -> ContentKinds {
        let mut kinds = ContentKinds::default();
        kinds.insert("map", "map.ron").unwrap();
        kinds.insert("data", "ron").unwrap();
        kinds
    }

    #[test]
    fn the_longest_matching_extension_names_the_kind() {
        let kinds = kinds();
        assert_eq!(kinds.kind_of(Path::new("core/fujix.map.ron")), Some("map"));
        assert_eq!(kinds.kind_of(Path::new("core/a.b.map.ron")), Some("map"));
        assert_eq!(kinds.kind_of(Path::new("core/sky.ron")), Some("data"));
        assert_eq!(kinds.kind_of(Path::new("core/fujixmap.ron")), Some("data"));
        assert_eq!(kinds.kind_of(Path::new("core/fujix.png")), None);
        assert_eq!(kinds.kind_of(Path::new("core/fujix.MAP.RON")), None);
    }

    #[test]
    fn a_kind_or_extension_registers_once() {
        let mut kinds = kinds();
        assert_eq!(
            kinds.insert("map", "other.ron"),
            Err(ContentKindError::DuplicateKind("map".into()))
        );
        assert!(matches!(
            kinds.insert("level", "map.ron"),
            Err(ContentKindError::DuplicateExtension { .. })
        ));
        assert!(matches!(
            kinds.insert("level", ".level.ron"),
            Err(ContentKindError::BadExtension { .. })
        ));
    }
}
