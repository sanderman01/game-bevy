//! Debug switch registration, validation and the reflected reads and writes behind them.

use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::Arc,
};

use bevy::{
    ecs::component::Mutable,
    prelude::*,
    reflect::{
        GetPath, TypeInfo, Typed,
        enums::{DynamicEnum, DynamicVariant, VariantInfo},
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A key with exact modifiers: it fires only with exactly these of Shift, Ctrl and Alt held (left
/// and right count the same) and no Super. `Display` is `Ctrl+Shift+F4`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub(crate) key: KeyCode,
    pub(crate) shift: bool,
    pub(crate) ctrl: bool,
    pub(crate) alt: bool,
}

impl KeyChord {
    pub const fn new(key: KeyCode) -> Self {
        Self {
            key,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    pub const fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    pub const fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    pub const fn alt(mut self) -> Self {
        self.alt = true;
        self
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.shift, "Shift+"),
            (self.alt, "Alt+"),
        ] {
            if held {
                f.write_str(name)?;
            }
        }
        write!(f, "{:?}", self.key)
    }
}

/// How a debug switch is described and which keys reach it. Built with [`DebugSwitchSpec::new`].
pub struct DebugSwitchSpec {
    category: &'static str,
    name: &'static str,
    description: &'static str,
    keys: Keys,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Keys {
    pub(crate) editor: Option<KeyChord>,
    pub(crate) release: Option<KeyChord>,
}

impl DebugSwitchSpec {
    /// `category` is one snake_case word. `name` follows the command pattern, `namespace.field`.
    /// `description` is one sentence. No keys until set.
    pub fn new(category: &'static str, name: &'static str, description: &'static str) -> Self {
        Self {
            category,
            name,
            description,
            keys: Keys::default(),
        }
    }

    /// The same chord in editor and release builds.
    pub fn key(mut self, chord: KeyChord) -> Self {
        self.keys = Keys {
            editor: Some(chord),
            release: Some(chord),
        };
        self
    }

    /// Overrides the editor chord; `None` means no key in editor builds.
    pub fn editor_key(mut self, chord: Option<KeyChord>) -> Self {
        self.keys.editor = chord;
        self
    }

    /// Overrides the release chord; `None` means no key in release builds.
    pub fn release_key(mut self, chord: Option<KeyChord>) -> Self {
        self.keys.release = chord;
        self
    }
}

/// A debug switch's value: a bool, or an enum variant as snake_case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DebugSwitchValue {
    Bool(bool),
    Variant(String),
}

impl fmt::Display for DebugSwitchValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Variant(name) => f.write_str(name),
        }
    }
}

/// Why a debug switch could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DebugSwitchError {
    #[error("no debug switch `{0}`")]
    Unknown(String),
    #[error("`{value}` is not a value of debug switch `{name}`; allowed: {}", list(.allowed))]
    InvalidValue {
        name: String,
        value: String,
        allowed: Vec<DebugSwitchValue>,
    },
    #[error("debug switch `{0}` is not available now")]
    Unavailable(String),
}

fn list(values: &[DebugSwitchValue]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What values a debug switch takes.
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    Bool,
    /// Unit variants in declaration order: (Rust name, snake_case name).
    Enum(Vec<(&'static str, String)>),
}

impl Kind {
    pub(crate) fn allowed(&self) -> Vec<DebugSwitchValue> {
        match self {
            Self::Bool => vec![DebugSwitchValue::Bool(false), DebugSwitchValue::Bool(true)],
            Self::Enum(variants) => variants
                .iter()
                .map(|(_, snake)| DebugSwitchValue::Variant(snake.clone()))
                .collect(),
        }
    }

    /// The value a key press moves to: a bool toggles, an enum takes its next variant, wrapping.
    pub(crate) fn next(&self, current: &DebugSwitchValue) -> DebugSwitchValue {
        match (self, current) {
            (Self::Bool, DebugSwitchValue::Bool(value)) => DebugSwitchValue::Bool(!value),
            (Self::Enum(variants), DebugSwitchValue::Variant(name)) => {
                let index = variants
                    .iter()
                    .position(|(_, snake)| snake == name)
                    .map_or(0, |index| (index + 1) % variants.len());
                DebugSwitchValue::Variant(variants[index].1.clone())
            }
            _ => current.clone(),
        }
    }

    fn rust_variant(&self, snake: &str) -> Option<&'static str> {
        let Self::Enum(variants) = self else {
            return None;
        };
        variants
            .iter()
            .find(|(_, name)| name == snake)
            .map(|(rust, _)| *rust)
    }

    fn of(info: &TypeInfo, name: &str) -> Self {
        match info {
            TypeInfo::Opaque(opaque) if opaque.type_id() == TypeId::of::<bool>() => Self::Bool,
            TypeInfo::Enum(info) => {
                let mut variants = Vec::new();
                let mut seen = BTreeSet::new();
                for variant in info.iter() {
                    let VariantInfo::Unit(unit) = variant else {
                        panic!(
                            "debug switch `{name}`: `{}` has the non-unit variant `{}`; only bools \
                             and enums of unit variants are supported",
                            info.type_path(),
                            variant.name()
                        );
                    };
                    let snake = snake_case(unit.name());
                    assert!(
                        seen.insert(snake.clone()),
                        "debug switch `{name}`: two variants of `{}` both become `{snake}`",
                        info.type_path()
                    );
                    variants.push((unit.name(), snake));
                }
                assert!(
                    !variants.is_empty(),
                    "debug switch `{name}`: `{}` has no variants",
                    info.type_path()
                );
                Self::Enum(variants)
            }
            other => panic!(
                "debug switch `{name}`: `{}` is neither a bool nor an enum of unit variants",
                other.type_path()
            ),
        }
    }

    fn read(&self, value: &dyn PartialReflect) -> Option<DebugSwitchValue> {
        match self {
            Self::Bool => value
                .try_downcast_ref::<bool>()
                .copied()
                .map(DebugSwitchValue::Bool),
            Self::Enum(_) => match value.reflect_ref() {
                bevy::reflect::ReflectRef::Enum(value) => {
                    Some(DebugSwitchValue::Variant(snake_case(value.variant_name())))
                }
                _ => None,
            },
        }
    }

    /// Writes an already validated value.
    fn write(&self, target: &mut dyn PartialReflect, value: &DebugSwitchValue) {
        match value {
            DebugSwitchValue::Bool(value) => {
                if let Some(target) = target.try_downcast_mut::<bool>() {
                    *target = *value;
                }
            }
            DebugSwitchValue::Variant(name) => {
                let Some(variant) = self.rust_variant(name) else {
                    return;
                };
                if let Err(err) = target.try_apply(&DynamicEnum::new(variant, DynamicVariant::Unit))
                {
                    error!("could not set variant `{variant}`: {err}");
                }
            }
        }
    }
}

/// `FieldOnly` to `field_only`, `HDROn` to `hdr_on`.
pub(crate) fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

type Read = Arc<dyn Fn(&World) -> Option<DebugSwitchValue> + Send + Sync>;
type Write = Arc<dyn Fn(&mut World, &DebugSwitchValue) + Send + Sync>;

/// One registered debug switch.
pub(crate) struct Entry {
    pub(crate) category: &'static str,
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) keys: Keys,
    pub(crate) kind: Kind,
    /// Type path of the resource, for listing. `None` for a switch over callbacks.
    pub(crate) resource: Option<&'static str>,
    pub(crate) path: Option<&'static str>,
    read: Read,
    write: Write,
}

/// Registrations collected while the app builds, turned into [`DebugSwitchRegistry`] in
/// `DebugSwitchPlugin::finish`.
#[derive(Resource, Default)]
pub(crate) struct PendingDebugSwitches {
    entries: Vec<Entry>,
    rebinds: Vec<(&'static str, Keys)>,
}

/// Every registered debug switch, validated in `DebugSwitchPlugin::finish`.
#[derive(Resource)]
pub struct DebugSwitchRegistry {
    switches: BTreeMap<&'static str, Entry>,
    editor_keys: HashMap<KeyChord, &'static str>,
    release_keys: HashMap<KeyChord, &'static str>,
    launch: HashMap<&'static str, DebugSwitchValue>,
}

/// Which chord of each debug switch is live. `EditorPlugins` inserts `Editor`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DebugSwitchTarget {
    Editor,
    #[default]
    Release,
}

impl DebugSwitchRegistry {
    /// Validates the registrations and applies the rebinds. Panics on any mistake, so a typo
    /// fails at startup.
    pub(crate) fn build(pending: PendingDebugSwitches) -> Self {
        let mut switches = BTreeMap::new();
        for entry in pending.entries {
            check_category(entry.category);
            check_name(entry.name);
            let name = entry.name;
            assert!(
                switches.insert(name, entry).is_none(),
                "debug switch `{name}` is registered twice"
            );
        }
        for (name, keys) in pending.rebinds {
            let Some(entry) = switches.get_mut(name) else {
                panic!("`rebind_debug_switch` names `{name}`, which is not a debug switch");
            };
            entry.keys = keys;
        }
        let mut editor_keys = HashMap::new();
        let mut release_keys = HashMap::new();
        for entry in switches.values() {
            for (chord, map, target) in [
                (entry.keys.editor, &mut editor_keys, "editor"),
                (entry.keys.release, &mut release_keys, "release"),
            ] {
                let Some(chord) = chord else { continue };
                if let Some(other) = map.insert(chord, entry.name) {
                    panic!(
                        "debug switches `{other}` and `{}` both use {chord} in {target} builds",
                        entry.name
                    );
                }
            }
        }
        Self {
            switches,
            editor_keys,
            release_keys,
            launch: HashMap::new(),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Entry> {
        self.switches.get(name)
    }

    /// Every debug switch, sorted by name.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.switches.values()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.switches.keys().copied()
    }

    /// Every category, sorted.
    pub fn categories(&self) -> Vec<&'static str> {
        self.switches
            .values()
            .map(|entry| entry.category)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn launch_value(&self, name: &str) -> Option<&DebugSwitchValue> {
        self.launch.get(name)
    }

    pub(crate) fn by_chord(&self, target: DebugSwitchTarget, chord: &KeyChord) -> Option<&str> {
        match target {
            DebugSwitchTarget::Editor => self.editor_keys.get(chord),
            DebugSwitchTarget::Release => self.release_keys.get(chord),
        }
        .copied()
    }

    pub(crate) fn missing_launch_values(&self) -> bool {
        self.launch.len() < self.switches.len()
    }

    /// Names close to `name`, for an `unknown_switch` hint.
    pub(crate) fn closest(&self, name: &str) -> Vec<&'static str> {
        let mut scored: Vec<_> = self
            .switches
            .keys()
            .map(|candidate| (edit_distance(name, candidate), *candidate))
            .filter(|(distance, candidate)| {
                *distance <= candidate.len().max(name.len()) / 3 + 1
                    || candidate.contains(name)
                    || name.contains(*candidate)
            })
            .collect();
        scored.sort();
        scored.into_iter().take(3).map(|(_, name)| name).collect()
    }
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(current + 1);
            prev = current;
        }
    }
    row[b.len()]
}

fn check_category(category: &str) {
    let valid = category.starts_with(|c: char| c.is_ascii_lowercase())
        && category
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    assert!(
        valid,
        "debug switch category `{category}` must be one lowercase snake_case word"
    );
}

fn check_name(name: &str) {
    let part_ok = |part: &str| {
        part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let valid = name
        .split_once('.')
        .is_some_and(|(namespace, field)| part_ok(namespace) && part_ok(field));
    assert!(
        valid,
        "debug switch name `{name}` must be `namespace.field`: one dot, lowercase snake_case"
    );
}

/// The type info at a dotted path of struct field names, from `root`.
fn field_info(root: &'static TypeInfo, path: &str, name: &str) -> &'static TypeInfo {
    let mut info = root;
    for field in path.split('.') {
        let TypeInfo::Struct(structure) = info else {
            panic!(
                "debug switch `{name}`: path `{path}` reaches `{}`, which is not a struct",
                info.type_path()
            );
        };
        let Some(next) = structure.field(field).and_then(|field| field.type_info()) else {
            panic!(
                "debug switch `{name}`: `{}` has no reflected field `{field}` (path `{path}`)",
                structure.type_path()
            );
        };
        info = next;
    }
    info
}

/// Registering debug switches on an [`App`]. Register in `build` only; a registration after
/// `DebugSwitchPlugin::finish` panics.
pub trait DebugSwitchAppExt {
    /// The whole resource is the value (a quality enum).
    fn add_resource_debug_switch<R: Resource<Mutability = Mutable> + Reflect + Typed>(
        &mut self,
        spec: DebugSwitchSpec,
    ) -> &mut Self;

    /// One field of the resource, by a dotted path of struct field names (`"physics"`).
    fn add_field_debug_switch<R: Resource<Mutability = Mutable> + Reflect + Typed>(
        &mut self,
        spec: DebugSwitchSpec,
        path: &'static str,
    ) -> &mut Self;

    /// A bool that is not a resource field, such as a marker component on the editor camera.
    /// `get` returns `None` when the switch does not apply now.
    fn add_bool_debug_switch(
        &mut self,
        spec: DebugSwitchSpec,
        get: fn(&World) -> Option<bool>,
        set: fn(&mut World, bool),
    ) -> &mut Self;

    /// Replaces both chords of a debug switch registered elsewhere. Applied in `finish`, after
    /// every registration; an unknown name panics there.
    fn rebind_debug_switch(
        &mut self,
        name: &'static str,
        editor_key: Option<KeyChord>,
        release_key: Option<KeyChord>,
    ) -> &mut Self;
}

impl DebugSwitchAppExt for App {
    fn add_resource_debug_switch<R: Resource<Mutability = Mutable> + Reflect + Typed>(
        &mut self,
        spec: DebugSwitchSpec,
    ) -> &mut Self {
        let kind = Kind::of(R::type_info(), spec.name);
        let read_kind = kind.clone();
        let write_kind = kind.clone();
        push(
            self,
            Entry {
                category: spec.category,
                name: spec.name,
                description: spec.description,
                keys: spec.keys,
                kind,
                resource: Some(R::type_path()),
                path: None,
                read: Arc::new(move |world| {
                    let resource = world.get_resource::<R>()?;
                    read_kind.read(resource.as_partial_reflect())
                }),
                write: Arc::new(move |world, value| {
                    if let Some(mut resource) = world.get_resource_mut::<R>() {
                        write_kind.write(resource.as_partial_reflect_mut(), value);
                    }
                }),
            },
        )
    }

    fn add_field_debug_switch<R: Resource<Mutability = Mutable> + Reflect + Typed>(
        &mut self,
        spec: DebugSwitchSpec,
        path: &'static str,
    ) -> &mut Self {
        let kind = Kind::of(field_info(R::type_info(), path, spec.name), spec.name);
        let read_kind = kind.clone();
        let write_kind = kind.clone();
        push(
            self,
            Entry {
                category: spec.category,
                name: spec.name,
                description: spec.description,
                keys: spec.keys,
                kind,
                resource: Some(R::type_path()),
                path: Some(path),
                read: Arc::new(move |world| {
                    let resource = world.get_resource::<R>()?;
                    read_kind.read(resource.reflect_path(path).ok()?)
                }),
                write: Arc::new(move |world, value| {
                    if let Some(mut resource) = world.get_resource_mut::<R>()
                        && let Ok(field) = resource.reflect_path_mut(path)
                    {
                        write_kind.write(field, value);
                    }
                }),
            },
        )
    }

    fn add_bool_debug_switch(
        &mut self,
        spec: DebugSwitchSpec,
        get: fn(&World) -> Option<bool>,
        set: fn(&mut World, bool),
    ) -> &mut Self {
        push(
            self,
            Entry {
                category: spec.category,
                name: spec.name,
                description: spec.description,
                keys: spec.keys,
                kind: Kind::Bool,
                resource: None,
                path: None,
                read: Arc::new(move |world| get(world).map(DebugSwitchValue::Bool)),
                write: Arc::new(move |world, value| {
                    if let DebugSwitchValue::Bool(value) = value {
                        set(world, *value);
                    }
                }),
            },
        )
    }

    fn rebind_debug_switch(
        &mut self,
        name: &'static str,
        editor_key: Option<KeyChord>,
        release_key: Option<KeyChord>,
    ) -> &mut Self {
        pending(self).rebinds.push((
            name,
            Keys {
                editor: editor_key,
                release: release_key,
            },
        ));
        self
    }
}

fn pending(app: &mut App) -> Mut<'_, PendingDebugSwitches> {
    assert!(
        !app.world().contains_resource::<DebugSwitchRegistry>(),
        "debug switches must be registered in a plugin's `build`; `DebugSwitchPlugin::finish` \
         has already run"
    );
    app.world_mut()
        .get_resource_or_init::<PendingDebugSwitches>()
}

fn push(app: &mut App, entry: Entry) -> &mut App {
    pending(app).entries.push(entry);
    app
}

/// Read and write debug switches by name from Rust, with the same values as the commands.
pub trait DebugSwitchWorldExt {
    /// `Ok(None)` when the switch does not apply now.
    fn debug_switch(&self, name: &str) -> Result<Option<DebugSwitchValue>, DebugSwitchError>;

    /// Sets a debug switch and logs the change, naming `source` (`command`, `menu`, a chord).
    /// Returns the previous value, or `None` when it already had this value.
    fn set_debug_switch(
        &mut self,
        name: &str,
        value: DebugSwitchValue,
        source: &str,
    ) -> Result<Option<DebugSwitchValue>, DebugSwitchError>;
}

impl DebugSwitchWorldExt for World {
    fn debug_switch(&self, name: &str) -> Result<Option<DebugSwitchValue>, DebugSwitchError> {
        let read = self
            .get_resource::<DebugSwitchRegistry>()
            .and_then(|registry| registry.get(name))
            .map(|entry| entry.read.clone())
            .ok_or_else(|| DebugSwitchError::Unknown(name.to_owned()))?;
        Ok(read(self))
    }

    fn set_debug_switch(
        &mut self,
        name: &str,
        value: DebugSwitchValue,
        source: &str,
    ) -> Result<Option<DebugSwitchValue>, DebugSwitchError> {
        let (name, read, write) = {
            let entry = self
                .get_resource::<DebugSwitchRegistry>()
                .and_then(|registry| registry.get(name))
                .ok_or_else(|| DebugSwitchError::Unknown(name.to_owned()))?;
            check_value(entry, &value)?;
            (entry.name, entry.read.clone(), entry.write.clone())
        };
        let Some(current) = read(self) else {
            return Err(DebugSwitchError::Unavailable(name.to_owned()));
        };
        record_launch(self, name, &current);
        if current == value {
            return Ok(None);
        }
        write(self, &value);
        if read(self).as_ref() != Some(&value) {
            return Err(DebugSwitchError::Unavailable(name.to_owned()));
        }
        info!("debug switch {name} = {value} ({source})");
        Ok(Some(current))
    }
}

/// Checks `value` is one `entry` takes.
pub(crate) fn check_value(entry: &Entry, value: &DebugSwitchValue) -> Result<(), DebugSwitchError> {
    let allowed = entry.kind.allowed();
    if allowed.contains(value) {
        return Ok(());
    }
    Err(DebugSwitchError::InvalidValue {
        name: entry.name.to_owned(),
        value: match value {
            DebugSwitchValue::Bool(value) => value.to_string(),
            DebugSwitchValue::Variant(name) => format!("\"{name}\""),
        },
        allowed,
    })
}

fn record_launch(world: &mut World, name: &'static str, value: &DebugSwitchValue) {
    let mut registry = world.resource_mut::<DebugSwitchRegistry>();
    if !registry.launch.contains_key(name) {
        // Untracked: the launch table is bookkeeping, not state anything reacts to.
        registry
            .bypass_change_detection()
            .launch
            .insert(name, value.clone());
    }
}

/// Records the launch value of every switch that has none yet and can be read now. Runs while
/// any is missing, because some switches only apply later (the editor camera gets its grid after
/// startup).
pub(crate) fn record_launch_values(world: &mut World) {
    let missing: Vec<(&'static str, Read)> = world
        .resource::<DebugSwitchRegistry>()
        .iter()
        .filter(|entry| {
            !world
                .resource::<DebugSwitchRegistry>()
                .launch
                .contains_key(entry.name)
        })
        .map(|entry| (entry.name, entry.read.clone()))
        .collect();
    for (name, read) in missing {
        if let Some(value) = read(world) {
            record_launch(world, name, &value);
        }
    }
}

pub(crate) fn launch_values_missing(registry: Res<DebugSwitchRegistry>) -> bool {
    registry.missing_launch_values()
}
