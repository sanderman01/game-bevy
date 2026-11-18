//! The alias index: a validated alias, and where it resolves to under the asset root.
//!
//! No Bevy io here on purpose. Resolution is a lookup, and keeping it that way lets it be tested
//! without an `App`.

use hashbrown::HashTable;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
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
    /// Bevy's asset paths are text, so a path that is not could never be loaded.
    #[error("an asset path must be UTF-8: {0:?}")]
    PathNotUtf8(PathBuf),
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
/// here as well, for the editor and the log to read. That copy is for inspection only, and a
/// clone shares the storage until one of them changes.
///
/// Laid out for hundreds of thousands of aliases, which a tiled map has: every path is one slice of
/// a shared text buffer, and an alias is a shared prefix (`islandmap::`) plus a slice of its own
/// path (`height_L8_0_0` in `.../height_L8_0_0.png`), so a derived alias costs no text of its own.
/// About 80 bytes an entry, where a map of strings took twice that.
#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "bevy", derive(bevy::ecs::resource::Resource))]
pub struct ContentIndex {
    table: Arc<Table>,
}

#[derive(Debug, Default, Clone)]
struct Table {
    /// Every path, back to back.
    text: String,
    /// The distinct alias prefixes, and each one's index in `prefixes`.
    prefixes: Vec<Box<str>>,
    prefix_ids: HashMap<Box<str>, u32>,
    entries: Vec<Entry>,
    /// Indices into `entries` of the live ones, keyed by alias.
    lookup: HashTable<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    path_start: u32,
    path_len: u32,
    prefix: u32,
    /// The alias after its prefix, as a slice of the path.
    rest_start: u32,
    rest_len: u32,
}

impl Table {
    fn slice(&self, start: u32, len: u32) -> &str {
        &self.text[start as usize..(start + len) as usize]
    }

    fn path(&self, entry: &Entry) -> &str {
        self.slice(entry.path_start, entry.path_len)
    }

    /// The alias in two parts, prefix then rest.
    fn alias(&self, entry: &Entry) -> (&str, &str) {
        alias_parts(&self.text, &self.prefixes, entry)
    }

    fn find(&self, alias: &str) -> Option<u32> {
        self.lookup
            .find(fnv(&[alias]), |&i| {
                let (prefix, rest) = self.alias(&self.entries[i as usize]);
                alias.len() == prefix.len() + rest.len()
                    && alias.starts_with(prefix)
                    && alias.ends_with(rest)
            })
            .copied()
    }

    fn push_text(&mut self, text: &str) -> u32 {
        let start = u32::try_from(self.text.len()).expect("content index text under 4 GiB");
        self.text.push_str(text);
        start
    }

    /// The prefix's index, added if new.
    fn prefix(&mut self, prefix: &str) -> u32 {
        if let Some(&id) = self.prefix_ids.get(prefix) {
            return id;
        }
        let id = u32::try_from(self.prefixes.len()).expect("under 4 billion prefixes");
        self.prefixes.push(prefix.into());
        self.prefix_ids.insert(prefix.into(), id);
        id
    }
}

fn alias_parts<'a>(text: &'a str, prefixes: &'a [Box<str>], entry: &Entry) -> (&'a str, &'a str) {
    let rest_start = (entry.path_start + entry.rest_start) as usize;
    (
        &prefixes[entry.prefix as usize],
        &text[rest_start..rest_start + entry.rest_len as usize],
    )
}

/// Hashes the concatenation of `parts`, so an alias hashes the same whole or as prefix and rest.
fn fnv(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in parts.iter().flat_map(|part| part.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
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
        let path = path.into();
        let path = path
            .to_str()
            .ok_or_else(|| AliasError::PathNotUtf8(path.clone()))?;
        let table = Arc::make_mut(&mut self.table);

        let path_start = table.push_text(path);
        // A derived alias ends in its file's stem, or its path from the package root: the tail
        // after the last `::` is usually in the path. Otherwise the whole alias is the prefix.
        let tail = alias.rfind("::").map_or(0, |i| i + 2);
        let split = if path.contains(&alias[tail..]) {
            tail
        } else {
            alias.len()
        };
        let rest = &alias[split..];
        let rest_start = path.rfind(rest).unwrap_or(0) as u32;
        let prefix = table.prefix(&alias[..split]);
        let entry = Entry {
            path_start,
            path_len: path.len() as u32,
            prefix,
            rest_start,
            rest_len: rest.len() as u32,
        };

        let displaced = table.find(alias);
        let previous = displaced.map(|i| PathBuf::from(table.path(&table.entries[i as usize])));
        let index = u32::try_from(table.entries.len()).expect("under 4 billion aliases");
        table.entries.push(entry);
        let Table {
            text,
            prefixes,
            entries,
            lookup,
            ..
        } = table;
        match displaced {
            Some(old) => {
                if let Ok(mut slot) = lookup.find_entry(fnv(&[alias]), |&i| i == old) {
                    *slot.get_mut() = index;
                }
            }
            None => {
                lookup.insert_unique(fnv(&[alias]), index, |&i| {
                    let (prefix, rest) = alias_parts(text, prefixes, &entries[i as usize]);
                    fnv(&[prefix, rest])
                });
            }
        }
        Ok(previous)
    }

    pub fn remove(&mut self, alias: &str) -> Option<PathBuf> {
        let i = self.table.find(alias)?;
        let table = Arc::make_mut(&mut self.table);
        let path = PathBuf::from(table.path(&table.entries[i as usize]));
        if let Ok(slot) = table.lookup.find_entry(fnv(&[alias]), |&live| live == i) {
            slot.remove();
        }
        Some(path)
    }

    pub fn resolve(&self, alias: &str) -> Option<&Path> {
        let i = self.table.find(alias)?;
        Some(Path::new(self.table.path(&self.table.entries[i as usize])))
    }

    pub fn len(&self) -> usize {
        self.table.lookup.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.lookup.is_empty()
    }

    /// Every alias and its path, in no particular order. The alias is assembled per item.
    pub fn iter(&self) -> impl Iterator<Item = (String, &Path)> {
        let table = &*self.table;
        table.lookup.iter().map(move |&i| {
            let entry = &table.entries[i as usize];
            let (prefix, rest) = table.alias(entry);
            (format!("{prefix}{rest}"), Path::new(table.path(entry)))
        })
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

    /// The layout's cases: an alias sharing its path's stem, one sharing nothing, an override of
    /// either, and a removal, all reading back whole.
    #[test]
    fn every_alias_reads_back_whatever_it_shares_with_its_path() {
        let mut index = ContentIndex::default();
        let entries = [
            (
                "islandmap::height_L8_0_0",
                "basegame/islandmap/height/L8/height_L8_0_0.png",
            ),
            (
                "islandmap::height_L8_1_0",
                "basegame/islandmap/height/L8/height_L8_1_0.png",
            ),
            ("core::props/barrel", "basegame/core/props/barrel.glb"),
            ("core::sky", "basegame/core/backdrop.sky.ron"),
            ("noprefix", "basegame/core/other.png"),
        ];
        for (alias, path) in entries {
            assert_eq!(index.insert(alias, path).unwrap(), None);
        }
        for (alias, path) in entries {
            assert_eq!(index.resolve(alias), Some(Path::new(path)), "{alias}");
        }
        assert_eq!(index.resolve("islandmap::height_L8_0"), None);
        assert_eq!(index.resolve("islandmap::height_L8_0_0x"), None);

        index
            .insert("core::sky", "mods/sky/backdrop.sky.ron")
            .unwrap();
        index.remove("islandmap::height_L8_1_0");
        let mut listed: Vec<(String, &Path)> = index.iter().collect();
        listed.sort();
        assert_eq!(
            listed,
            [
                (
                    "core::props/barrel".to_owned(),
                    Path::new("basegame/core/props/barrel.glb")
                ),
                (
                    "core::sky".to_owned(),
                    Path::new("mods/sky/backdrop.sky.ron")
                ),
                (
                    "islandmap::height_L8_0_0".to_owned(),
                    Path::new("basegame/islandmap/height/L8/height_L8_0_0.png")
                ),
                ("noprefix".to_owned(), Path::new("basegame/core/other.png")),
            ]
        );
        assert_eq!(index.len(), 4);
    }

    /// A clone shares the storage, and changing one leaves the other as it was.
    #[test]
    fn a_clone_is_independent() {
        let mut index = ContentIndex::default();
        index.insert("core::a", "basegame/core/a.png").unwrap();
        let copy = index.clone();
        index.insert("core::b", "basegame/core/b.png").unwrap();
        assert_eq!(copy.len(), 1);
        assert_eq!(index.len(), 2);
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
