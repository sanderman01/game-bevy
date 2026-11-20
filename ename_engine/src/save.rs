//! Save sections: the state a game saves, as named sections, each owned by one system, with its
//! own version and migrations. See `docs/save-sections.md`.
//!
//! A system registers a section with [`SaveSectionsAppExt::register_save_section`]: a capture
//! that reads the world into a save struct, and an apply that puts one back. [`capture_sections`]
//! captures every section in one frame; [`CapturedSections::serialize`] turns them into
//! [`SavedSections`] (RON, one raw value per section), off the main thread if the caller likes.
//! [`check_sections`] reads saved sections into their structs, running migrations, before
//! anything changes; [`apply_sections`] then applies them in registration order, so a game
//! registers sections in the order they depend on each other. Apply cannot fail and nothing rolls
//! back. A section no registered system knows is kept, so a game can write it back.
//!
//! Where the bytes go is the game's: a record in user storage, say.

use std::{any::Any, collections::BTreeMap, fmt};

use bevy::prelude::*;
use ename_asset_content::ron_from_bytes_lenient;
use ron::{ser::PrettyConfig, value::RawValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The format of [`SavedSections`] this engine writes, and the newest it reads.
pub const SECTIONS_FORMAT: u32 = 1;

/// One saved section: its version and its value as RON.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SavedSection {
    pub version: u32,
    pub value: Box<RawValue>,
}

/// Saved sections by name, as written: `(format: 1, sections: {"name": (version: 1, value: ..)})`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SavedSections {
    format: u32,
    pub sections: BTreeMap<String, SavedSection>,
}

impl Default for SavedSections {
    fn default() -> Self {
        Self {
            format: SECTIONS_FORMAT,
            sections: BTreeMap::new(),
        }
    }
}

/// Just the format, read before the rest decides how to read it.
#[derive(Deserialize)]
struct Format {
    #[serde(default)]
    format: u32,
}

impl SavedSections {
    /// RON, a line per section.
    pub fn to_ron(&self) -> Result<String, SectionError> {
        ron::ser::to_string_pretty(self, PrettyConfig::new().indentor("  "))
            .map_err(|err| SectionError::Serialize(err.to_string()))
    }

    /// Reads RON written by [`Self::to_ron`]. Refuses a format above [`SECTIONS_FORMAT`].
    pub fn from_ron(bytes: &[u8]) -> Result<Self, SectionError> {
        let Format { format } =
            ron::de::from_bytes(bytes).map_err(|err| SectionError::Unreadable(err.to_string()))?;
        match format {
            0 => Err(SectionError::NoFormat),
            SECTIONS_FORMAT => {
                let mut saved: Self = ron::de::from_bytes(bytes)
                    .map_err(|err| SectionError::Unreadable(err.to_string()))?;
                // A raw value keeps the whitespace before it, which would grow with each write.
                for section in saved.sections.values_mut() {
                    let text = section.value.get_ron();
                    if text.trim().len() != text.len() {
                        section.value = RawValue::from_boxed_ron(text.trim().into())
                            .map_err(|err| SectionError::Unreadable(err.to_string()))?;
                    }
                }
                Ok(saved)
            }
            found => Err(SectionError::NewerFormat {
                found,
                reads: SECTIONS_FORMAT,
            }),
        }
    }

    /// Adds each of `kept` that is not here already: sections an earlier read did not know.
    pub fn keep(&mut self, kept: &BTreeMap<String, SavedSection>) {
        for (name, section) in kept {
            self.sections
                .entry(name.clone())
                .or_insert_with(|| section.clone());
        }
    }
}

/// Why saved sections cannot be written or read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SectionError {
    #[error("the saved sections hold no format")]
    NoFormat,
    #[error("the saved sections are format {found}; this game reads up to {reads}")]
    NewerFormat { found: u32, reads: u32 },
    #[error("the saved sections do not parse: {0}")]
    Unreadable(String),
    #[error("the section `{section}` is version {found}; this game reads up to {reads}")]
    NewerVersion {
        section: String,
        found: u32,
        reads: u32,
    },
    #[error("the section `{section}` is version {from}, which this game cannot migrate")]
    NoMigration { section: String, from: u32 },
    #[error("the section `{section}` does not migrate from version {from}: {message}")]
    Migration {
        section: String,
        from: u32,
        message: String,
    },
    #[error("the section `{section}` does not parse: {message}")]
    Invalid { section: String, message: String },
    #[error("the sections do not serialize: {0}")]
    Serialize(String),
    #[error("the section `{section}` cannot be captured: {message}")]
    Capture { section: String, message: String },
}

impl SectionError {
    /// A stable name for the error, for a command's error kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NewerFormat { .. } | Self::NewerVersion { .. } => "newer_version",
            Self::NoMigration { .. } | Self::Migration { .. } => "migration_failed",
            Self::Serialize(_) => "serialize_failed",
            Self::Capture { .. } => "capture_failed",
            Self::NoFormat | Self::Unreadable(_) | Self::Invalid { .. } => "unreadable",
        }
    }
}

/// Turns the RON of a section at one version into the RON of the next, with the fields the read
/// of the old version skipped.
type MigrateFn =
    Box<dyn Fn(&str, &str) -> Result<(String, Vec<String>), SectionError> + Send + Sync>;

/// A section to register: its `name`, its current `version` (1 or more), how to `capture` it
/// (`Ok(None)` saves no section, `Err` fails the save) and how to `apply` it, adding lines to a
/// report for the player. Apply cannot fail and nothing rolls back: whatever can refuse a save is
/// checked by [`check_sections`] before anything changes.
pub struct SaveSection<T> {
    name: &'static str,
    version: u32,
    capture: fn(&mut World) -> Result<Option<T>, String>,
    apply: fn(&mut World, T, &mut Vec<String>),
    migrations: BTreeMap<u32, MigrateFn>,
}

impl<T> SaveSection<T> {
    pub fn new(
        name: &'static str,
        version: u32,
        capture: fn(&mut World) -> Result<Option<T>, String>,
        apply: fn(&mut World, T, &mut Vec<String>),
    ) -> Self {
        Self {
            name,
            version,
            capture,
            apply,
            migrations: BTreeMap::new(),
        }
    }

    /// Adds the migration from version `from` to `from + 1`: `Old` is the section's struct at
    /// `from`, read leniently from the saved RON, and `Next` the one at `from + 1`. Typed, not a
    /// `ron::Value`, which loses enum variant names.
    pub fn migration<Old, Next>(
        mut self,
        from: u32,
        migrate: fn(Old) -> Result<Next, String>,
    ) -> Self
    where
        Old: DeserializeOwned + 'static,
        Next: Serialize + 'static,
    {
        assert!(
            from >= 1 && from < self.version,
            "section `{}`: a migration from version {from} to a section at version {}",
            self.name,
            self.version
        );
        let step: MigrateFn = Box::new(move |name, text| {
            let (old, skipped) =
                ron_from_bytes_lenient::<Old>(text.as_bytes(), &ron::Options::default()).map_err(
                    |err| SectionError::Invalid {
                        section: name.to_owned(),
                        message: format!("as version {from}: {err}"),
                    },
                )?;
            let failed = |message| SectionError::Migration {
                section: name.to_owned(),
                from,
                message,
            };
            let next = migrate(old).map_err(failed)?;
            Ok((to_line(&next).map_err(failed)?, skipped))
        });
        assert!(
            self.migrations.insert(from, step).is_none(),
            "section `{}`: two migrations from version {from}",
            self.name
        );
        self
    }
}

type Serializer = Box<dyn FnOnce() -> Result<Box<RawValue>, String> + Send>;
type Checked = Box<dyn Any + Send + Sync>;
type CaptureFn = Box<dyn Fn(&mut World) -> Result<Option<Serializer>, String> + Send + Sync>;
type CheckFn =
    Box<dyn Fn(&SavedSection) -> Result<(Checked, Vec<String>), SectionError> + Send + Sync>;
type ApplyFn = Box<dyn Fn(&mut World, Checked, &mut Vec<String>) + Send + Sync>;

struct Entry {
    name: &'static str,
    version: u32,
    capture: CaptureFn,
    check: CheckFn,
    apply: ApplyFn,
}

/// Every registered section, in registration order, which is the order they apply in.
#[derive(Resource, Default)]
struct SaveSections(Vec<Entry>);

/// Registering save sections on an [`App`].
pub trait SaveSectionsAppExt {
    /// Sections apply in the order they are registered. A name is unique; a duplicate panics.
    fn register_save_section<T>(&mut self, section: SaveSection<T>) -> &mut Self
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static;
}

impl SaveSectionsAppExt for App {
    fn register_save_section<T>(&mut self, section: SaveSection<T>) -> &mut Self
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        let SaveSection {
            name,
            version,
            capture,
            apply,
            migrations,
        } = section;
        assert!(version >= 1, "section `{name}`: versions start at 1");
        assert!(!name.is_empty(), "a save section needs a name");
        let entry = Entry {
            name,
            version,
            capture: Box::new(move |world| {
                Ok(capture(world)?.map(|value| Box::new(move || to_raw(&value)) as Serializer))
            }),
            check: Box::new(move |saved| {
                let (value, unknown) = read_section::<T>(name, version, &migrations, saved)?;
                Ok((Box::new(value) as Checked, unknown))
            }),
            apply: Box::new(move |world, value, report| {
                // `check` made it, from this entry.
                let Ok(value) = value.downcast::<T>() else {
                    return;
                };
                apply(world, *value, report);
            }),
        };
        let mut sections = self.world_mut().get_resource_or_init::<SaveSections>();
        assert!(
            sections.0.iter().all(|other| other.name != name),
            "save section `{name}` is registered twice"
        );
        sections.0.push(entry);
        self
    }
}

/// One line per section: `(location: (1.0, 2.0), name: "a")`.
fn to_line<T: Serialize>(value: &T) -> Result<String, String> {
    let config = PrettyConfig::new()
        .compact_structs(true)
        .compact_maps(true)
        .compact_arrays(true);
    ron::ser::to_string_pretty(value, config).map_err(|err| err.to_string())
}

fn to_raw<T: Serialize>(value: &T) -> Result<Box<RawValue>, String> {
    RawValue::from_boxed_ron(to_line(value)?.into_boxed_str()).map_err(|err| err.to_string())
}

/// Reads `saved` into the current version of `T`, migrating an older one, leniently: the fields
/// it skipped come back as `name: path`.
fn read_section<T: DeserializeOwned>(
    name: &str,
    version: u32,
    migrations: &BTreeMap<u32, MigrateFn>,
    saved: &SavedSection,
) -> Result<(T, Vec<String>), SectionError> {
    let invalid = |message: String| SectionError::Invalid {
        section: name.to_owned(),
        message,
    };
    let (value, unknown) = match saved.version {
        0 => return Err(invalid("it holds version 0".to_owned())),
        found if found > version => {
            return Err(SectionError::NewerVersion {
                section: name.to_owned(),
                found,
                reads: version,
            });
        }
        found if found == version => {
            ron_from_bytes_lenient(saved.value.get_ron().as_bytes(), &ron::Options::default())
                .map_err(|err| invalid(err.to_string()))?
        }
        found => {
            let mut text = saved.value.get_ron().to_owned();
            let mut unknown = Vec::new();
            for from in found..version {
                let migrate = migrations
                    .get(&from)
                    .ok_or_else(|| SectionError::NoMigration {
                        section: name.to_owned(),
                        from,
                    })?;
                let (next, skipped) = migrate(name, &text)?;
                text = next;
                unknown.extend(skipped);
            }
            let (value, skipped) =
                ron_from_bytes_lenient(text.as_bytes(), &ron::Options::default())
                    .map_err(|err| invalid(err.to_string()))?;
            unknown.extend(skipped);
            (value, unknown)
        }
    };
    let unknown = unknown
        .into_iter()
        .map(|path| format!("{name}: {path}"))
        .collect();
    Ok((value, unknown))
}

/// Sections captured in one frame, to serialize later.
pub struct CapturedSections(Vec<(&'static str, u32, Serializer)>);

impl fmt::Debug for CapturedSections {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, ..)| name))
            .finish()
    }
}

impl CapturedSections {
    /// RON for each section. Runs anywhere, a task included.
    pub fn serialize(self) -> Result<SavedSections, SectionError> {
        let mut saved = SavedSections::default();
        for (name, version, serialize) in self.0 {
            let value = serialize()
                .map_err(|err| SectionError::Serialize(format!("section `{name}`: {err}")))?;
            saved
                .sections
                .insert(name.to_owned(), SavedSection { version, value });
        }
        Ok(saved)
    }
}

/// Captures every registered section, in one go. A capture that gives `None` saves no section;
/// one that fails fails the whole, so a save is never written without a section it needs.
pub fn capture_sections(world: &mut World) -> Result<CapturedSections, SectionError> {
    world.init_resource::<SaveSections>();
    world.resource_scope(|world, sections: Mut<SaveSections>| {
        let mut captured = Vec::new();
        for entry in &sections.0 {
            let serializer = (entry.capture)(world).map_err(|message| SectionError::Capture {
                section: entry.name.to_owned(),
                message,
            })?;
            if let Some(serializer) = serializer {
                captured.push((entry.name, entry.version, serializer));
            }
        }
        Ok(CapturedSections(captured))
    })
}

/// Saved sections read into their structs, ready to apply.
pub struct CheckedSections {
    values: BTreeMap<String, Checked>,
    /// Sections no registered system knows, kept as they were.
    pub unknown: BTreeMap<String, SavedSection>,
    /// Fields the sections hold that this game does not know, skipped: `name: path`.
    pub skipped: Vec<String>,
}

impl fmt::Debug for CheckedSections {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedSections")
            .field("values", &self.values.keys().collect::<Vec<_>>())
            .field("unknown", &self.unknown.keys().collect::<Vec<_>>())
            .field("skipped", &self.skipped)
            .finish()
    }
}

impl CheckedSections {
    /// The section `name`, if the save holds it and it is a `T`.
    pub fn get<T: 'static>(&self, name: &str) -> Option<&T> {
        self.values.get(name)?.downcast_ref()
    }

    /// Whether the save holds the section `name`, known to this game.
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }
}

/// Reads every section of `saved` into its struct, migrating older versions, without changing
/// the world. Any section that does not read fails the whole; one no system knows is kept.
pub fn check_sections(
    world: &World,
    saved: &SavedSections,
) -> Result<CheckedSections, SectionError> {
    let empty = SaveSections::default();
    let sections = world.get_resource::<SaveSections>().unwrap_or(&empty);
    let mut checked = CheckedSections {
        values: BTreeMap::new(),
        unknown: BTreeMap::new(),
        skipped: Vec::new(),
    };
    for (name, section) in &saved.sections {
        let Some(entry) = sections.0.iter().find(|entry| entry.name == name) else {
            checked.unknown.insert(name.clone(), section.clone());
            continue;
        };
        let (value, skipped) = (entry.check)(section)?;
        checked.values.insert(name.clone(), value);
        checked.skipped.extend(skipped);
    }
    Ok(checked)
}

/// Applies the sections `checked` holds, in registration order; each may add lines to `report`.
pub fn apply_sections(world: &mut World, mut checked: CheckedSections, report: &mut Vec<String>) {
    world.init_resource::<SaveSections>();
    world.resource_scope(|world, sections: Mut<SaveSections>| {
        for entry in &sections.0 {
            if let Some(value) = checked.values.remove(entry.name) {
                (entry.apply)(world, value, report);
            }
        }
    });
}

#[cfg(test)]
mod tests;
