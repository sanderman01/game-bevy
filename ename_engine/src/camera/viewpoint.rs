//! Named camera viewpoints in a versioned RON file, and the facets a viewpoint can carry.
//!
//! The file is read on every call, so a hand edit, a checkout or another process's save takes
//! effect at once. Writes are atomic and go through a lock file, so two writers changing
//! different entries both land. Order in the file is kept.
//!
//! A facet is a named piece of state a crate registers with a capture and an apply function,
//! such as a game's sun direction, so a viewpoint can restore more than the camera. Facets are
//! stored as raw RON and shown to callers as JSON.

use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use bevy::{math::DVec3, prelude::*};
use ron::{ser::PrettyConfig, value::RawValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::pose::CameraPose;
use crate::command::CommandError;

/// The newest viewpoints file version this build reads.
const VIEWPOINTS_VERSION: u32 = 1;

/// Written above the viewpoints on every save.
const HEADER: &str = "// Named camera viewpoints. Read by `viewpoint.list` / `viewpoint.goto`; \
                      `viewpoint.save` rewrites\n// this file and drops comments, so describe a \
                      viewpoint in its `note`.\n";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ViewpointFile {
    pub version: u32,
    pub viewpoints: Vec<Viewpoint>,
}

impl Default for ViewpointFile {
    fn default() -> Self {
        Self {
            version: VIEWPOINTS_VERSION,
            viewpoints: Vec::new(),
        }
    }
}

/// One viewpoint. The pose fields are written out rather than flattened from [`CameraPose`]:
/// ron writes a struct with a flattened field as a map, and flattened content cannot hold a
/// `RawValue`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Viewpoint {
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// Absolute metres, Y up.
    pub position: [f64; 3],
    /// Degrees.
    pub yaw: f64,
    pub pitch: f64,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "trimmed_facets"
    )]
    pub facets: BTreeMap<String, Box<RawValue>>,
}

/// A raw value keeps the whitespace around it; without a trim every rewrite would add some.
fn trimmed_facets<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Box<RawValue>>, D::Error> {
    let facets = BTreeMap::<String, Box<RawValue>>::deserialize(deserializer)?;
    Ok(facets
        .into_iter()
        .map(|(name, raw)| (name, raw.trim_boxed()))
        .collect())
}

impl Viewpoint {
    pub fn pose(&self) -> CameraPose {
        CameraPose {
            position: DVec3::from(self.position),
            yaw: self.yaw,
            pitch: self.pitch,
        }
    }
}

/// Reads a RON file. A parse error names the file, line and column.
pub fn read_ron<T: DeserializeOwned>(path: &Path) -> Result<T, CommandError> {
    let text = std::fs::read_to_string(path).map_err(|err| io_error(path, "read", &err))?;
    parse_ron(path, &text)
}

fn parse_ron<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T, CommandError> {
    ron::from_str(text).map_err(|err| {
        CommandError::failed(
            "invalid_file",
            format!(
                "{}:{}:{}: {}",
                path.display(),
                err.span.start.line,
                err.span.start.col,
                err.code
            ),
        )
    })
}

/// Writes `value` as pretty RON after `header`, replacing the file whole or not at all.
fn write_ron_atomic<T: Serialize>(
    path: &Path,
    value: &T,
    header: &str,
) -> Result<(), CommandError> {
    let text = ron::ser::to_string_pretty(value, PrettyConfig::new()).map_err(|err| {
        CommandError::failed(
            "handler_failed",
            format!("could not write {}: {err}", path.display()),
        )
    })?;
    let parent = parent_dir(path);
    std::fs::create_dir_all(&parent).map_err(|err| io_error(&parent, "create", &err))?;
    let mut file =
        tempfile::NamedTempFile::new_in(&parent).map_err(|err| io_error(path, "write", &err))?;
    file.write_all(header.as_bytes())
        .and_then(|()| file.write_all(text.as_bytes()))
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|err| io_error(path, "write", &err))?;
    file.persist(path)
        .map_err(|err| io_error(path, "replace", &err.error))?;
    Ok(())
}

/// Read-modify-write under an exclusive lock on `<path>.lock`: reads the file (or `T::default()`
/// when it is missing), applies `change`, and writes it back atomically. Several processes can
/// share the file; each change lands.
pub fn update_ron_locked<T, R>(
    path: &Path,
    header: &str,
    change: impl FnOnce(&mut T) -> Result<R, CommandError>,
) -> Result<R, CommandError>
where
    T: Default + Serialize + DeserializeOwned,
{
    let parent = parent_dir(path);
    std::fs::create_dir_all(&parent).map_err(|err| io_error(&parent, "create", &err))?;
    let lock_path = lock_path(path);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|err| io_error(&lock_path, "open", &err))?;
    lock.lock()
        .map_err(|err| io_error(&lock_path, "lock", &err))?;
    let result = (|| {
        let mut value = match std::fs::read_to_string(path) {
            Ok(text) => parse_ron(path, &text)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => T::default(),
            Err(err) => return Err(io_error(path, "read", &err)),
        };
        let result = change(&mut value)?;
        write_ron_atomic(path, &value, header)?;
        Ok(result)
    })();
    // Dropping the file releases the lock too; unlocking first makes the order explicit.
    let _ = lock.unlock();
    result
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn io_error(path: &Path, action: &str, err: &std::io::Error) -> CommandError {
    CommandError::failed(
        "file_error",
        format!("could not {action} {}: {err}", path.display()),
    )
}

/// The viewpoints in `path`, or `None` when the file does not exist. Fails on a newer version
/// (`unsupported_version`), a parse error (`invalid_file`, with line and column) and a name that
/// breaks the naming rule (`invalid_name`).
pub fn load(path: &Path) -> Result<Option<ViewpointFile>, CommandError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(path, "read", &err)),
    };
    let file = parse_viewpoints(path, &text)?;
    Ok(Some(file))
}

fn parse_viewpoints(path: &Path, text: &str) -> Result<ViewpointFile, CommandError> {
    #[derive(Deserialize)]
    struct VersionOnly {
        version: u32,
    }
    // The version first, so a newer file is refused by version rather than by whatever field
    // this build does not know.
    let VersionOnly { version } = parse_ron(path, text)?;
    check_version(path, version)?;
    let file: ViewpointFile = parse_ron(path, text)?;
    for viewpoint in &file.viewpoints {
        validate_name(&viewpoint.name).map_err(|err| CommandError {
            message: format!("{}: {}", path.display(), err.message),
            ..err
        })?;
        // RON reads `NaN` and `inf`, so a parsed pose is not yet a valid one.
        let finite = viewpoint.position.iter().all(|v| v.is_finite())
            && viewpoint.yaw.is_finite()
            && viewpoint.pitch.is_finite();
        if !finite {
            return Err(CommandError::failed(
                "invalid_pose",
                format!(
                    "{}: viewpoint `{}` has a position, yaw or pitch that is not a finite number",
                    path.display(),
                    viewpoint.name
                ),
            ));
        }
    }
    Ok(file)
}

fn check_version(path: &Path, version: u32) -> Result<(), CommandError> {
    if version == VIEWPOINTS_VERSION {
        return Ok(());
    }
    Err(CommandError::failed(
        "unsupported_version",
        format!(
            "{} is viewpoints version {version}; this build reads version {VIEWPOINTS_VERSION}",
            path.display()
        ),
    ))
}

/// Adds `viewpoint` at the end of the file, or replaces the one with its name in place when
/// `replace` is set (`viewpoint_exists` otherwise). Creates the file when it is missing.
pub fn save(path: &Path, viewpoint: Viewpoint, replace: bool) -> Result<(), CommandError> {
    let name = validate_name(&viewpoint.name)?;
    let viewpoint = Viewpoint { name, ..viewpoint };
    update_ron_locked(path, HEADER, |file: &mut ViewpointFile| {
        check_version(path, file.version)?;
        match file
            .viewpoints
            .iter_mut()
            .find(|existing| existing.name == viewpoint.name)
        {
            Some(existing) if replace => *existing = viewpoint,
            Some(_) => {
                return Err(CommandError::failed(
                    "viewpoint_exists",
                    format!(
                        "{} already has a viewpoint `{}`",
                        path.display(),
                        viewpoint.name
                    ),
                )
                .with_hint("Pass `replace: true` to overwrite it, or pick another name."));
            }
            None => file.viewpoints.push(viewpoint),
        }
        Ok(())
    })
}

/// The trimmed name, if it is non-empty and file-safe: ASCII letters, digits, space, `.`, `_`
/// and `-`, not starting with `.`.
pub fn validate_name(name: &str) -> Result<String, CommandError> {
    let trimmed = name.trim();
    let valid = !trimmed.is_empty()
        && !trimmed.starts_with('.')
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'));
    if valid {
        Ok(trimmed.to_string())
    } else {
        Err(CommandError::invalid(
            "invalid_name",
            format!(
                "viewpoint name `{name}` must be non-empty ASCII letters, digits, spaces, `.`, `_` \
                 or `-`, not starting with `.`"
            ),
        ))
    }
}

type CaptureFn = Box<dyn Fn(&mut World) -> Result<Box<RawValue>, CommandError> + Send + Sync>;
type ApplyFn = Box<dyn Fn(&mut World, &RawValue) -> Result<Value, CommandError> + Send + Sync>;
type CheckFn = Box<dyn Fn(&RawValue) -> Result<Value, CommandError> + Send + Sync>;

struct Facet {
    capture: CaptureFn,
    /// Parses the stored value and returns it as JSON, without applying it.
    check: CheckFn,
    apply: ApplyFn,
}

/// Every registered facet, by name.
#[derive(Resource, Default)]
struct ViewpointFacets(BTreeMap<&'static str, Facet>);

/// Registering viewpoint facets on an [`App`].
pub trait ViewpointFacetsAppExt {
    /// `name` is unique; a duplicate panics at startup.
    fn register_viewpoint_facet<T>(
        &mut self,
        name: &'static str,
        capture: fn(&mut World) -> Result<T, CommandError>,
        apply: fn(&mut World, T) -> Result<(), CommandError>,
    ) -> &mut Self
    where
        T: Serialize + DeserializeOwned + 'static;
}

impl ViewpointFacetsAppExt for App {
    fn register_viewpoint_facet<T>(
        &mut self,
        name: &'static str,
        capture: fn(&mut World) -> Result<T, CommandError>,
        apply: fn(&mut World, T) -> Result<(), CommandError>,
    ) -> &mut Self
    where
        T: Serialize + DeserializeOwned + 'static,
    {
        let parse = move |raw: &RawValue| -> Result<T, CommandError> {
            raw.into_rust::<T>().map_err(|err| {
                CommandError::failed(
                    "invalid_facet",
                    format!("facet `{name}` does not parse: {err}"),
                )
            })
        };
        let facet = Facet {
            capture: Box::new(move |world| {
                let value = capture(world)?;
                // One line, readable: `(elevation: 12.0, azimuth: 230.19)`.
                let config = PrettyConfig::new()
                    .compact_structs(true)
                    .compact_maps(true)
                    .compact_arrays(true);
                ron::ser::to_string_pretty(&value, config)
                    .map_err(|err| err.to_string())
                    .and_then(|text| {
                        RawValue::from_boxed_ron(text.into_boxed_str()).map_err(|e| e.to_string())
                    })
                    .map_err(|err| {
                        CommandError::failed(
                            "handler_failed",
                            format!("facet `{name}` does not serialize: {err}"),
                        )
                    })
            }),
            check: Box::new(move |raw| to_json(name, &parse(raw)?)),
            apply: Box::new(move |world, raw| {
                let value = parse(raw)?;
                let json = to_json(name, &value)?;
                apply(world, value)?;
                Ok(json)
            }),
        };
        let mut facets = self.world_mut().get_resource_or_init::<ViewpointFacets>();
        assert!(
            facets.0.insert(name, facet).is_none(),
            "viewpoint facet `{name}` is registered twice"
        );
        self
    }
}

fn to_json<T: Serialize>(name: &str, value: &T) -> Result<Value, CommandError> {
    serde_json::to_value(value).map_err(|err| {
        CommandError::failed(
            "handler_failed",
            format!("facet `{name}` is not JSON: {err}"),
        )
    })
}

fn unknown_facet(facets: &ViewpointFacets, name: &str) -> CommandError {
    let known: Vec<&str> = facets.0.keys().copied().collect();
    let known = if known.is_empty() {
        "none".to_string()
    } else {
        known.join(", ")
    };
    CommandError::failed(
        "unknown_facet",
        format!("no viewpoint facet `{name}` in this build; it has: {known}"),
    )
}

/// Captures the named facets. `unknown_facet` for a name this build does not register.
pub fn capture_facets(
    world: &mut World,
    names: &[String],
) -> Result<BTreeMap<String, Box<RawValue>>, CommandError> {
    world.init_resource::<ViewpointFacets>();
    world.resource_scope(|world, registry: Mut<ViewpointFacets>| {
        names
            .iter()
            .map(|name| {
                let facet = registry
                    .0
                    .get(name.as_str())
                    .ok_or_else(|| unknown_facet(&registry, name))?;
                Ok((name.clone(), (facet.capture)(world)?))
            })
            .collect()
    })
}

/// `facets` as JSON, without applying them. `unknown_facet` and `invalid_facet` as on apply.
pub fn facets_as_json(
    world: &World,
    facets: &BTreeMap<String, Box<RawValue>>,
) -> Result<serde_json::Map<String, Value>, CommandError> {
    let empty = ViewpointFacets::default();
    let registry = world.get_resource::<ViewpointFacets>().unwrap_or(&empty);
    facets
        .iter()
        .map(|(name, raw)| {
            let facet = registry
                .0
                .get(name.as_str())
                .ok_or_else(|| unknown_facet(registry, name))?;
            Ok((name.clone(), (facet.check)(raw)?))
        })
        .collect()
}

/// Applies `facets` and returns each applied value as JSON. Every facet is looked up and parsed
/// before any is applied, so an unknown or malformed one applies nothing.
pub fn apply_facets(
    world: &mut World,
    facets: &BTreeMap<String, Box<RawValue>>,
) -> Result<serde_json::Map<String, Value>, CommandError> {
    if facets.is_empty() {
        return Ok(serde_json::Map::new());
    }
    world.init_resource::<ViewpointFacets>();
    world.resource_scope(|world, registry: Mut<ViewpointFacets>| {
        for (name, raw) in facets {
            let facet = registry
                .0
                .get(name.as_str())
                .ok_or_else(|| unknown_facet(&registry, name))?;
            (facet.check)(raw)?;
        }
        facets
            .iter()
            .map(|(name, raw)| Ok((name.clone(), (registry.0[name.as_str()].apply)(world, raw)?)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
// Named camera viewpoints.
(
    version: 1,
    viewpoints: [
        (
            name: "airship side",
            note: "Airship from the side at hull height, ground in the lower third.",
            position: (40.0, 25.0, 120.0),
            yaw: 0.0,
            pitch: -3.0,
        ),
        (
            name: "high",
            note: "High over the stage, looking down at the airship with a low sun.",
            position: (80.0, 150.0, 80.0),
            yaw: 50.0,
            pitch: -40.0,
            facets: {
                "sun": (elevation: 12.0, azimuth: 230.19),
            },
        ),
    ],
)
"#;

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
    struct Sun {
        elevation: f64,
        azimuth: f64,
    }

    #[derive(Resource, Default, Debug, PartialEq)]
    struct SunState(Option<Sun>);

    fn capture_sun(world: &mut World) -> Result<Sun, CommandError> {
        Ok(world.resource::<SunState>().0.clone().unwrap_or(Sun {
            elevation: 45.0,
            azimuth: 0.0,
        }))
    }

    fn apply_sun(world: &mut World, sun: Sun) -> Result<(), CommandError> {
        world.resource_mut::<SunState>().0 = Some(sun);
        Ok(())
    }

    fn app_with_sun() -> App {
        let mut app = App::new();
        app.init_resource::<SunState>()
            .register_viewpoint_facet("sun", capture_sun, apply_sun);
        app
    }

    fn temp_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("viewpoints.ron")
    }

    #[test]
    fn parses_the_example() {
        let file = parse_viewpoints(Path::new("viewpoints.ron"), EXAMPLE).unwrap();
        assert_eq!(file.version, 1);
        assert_eq!(file.viewpoints.len(), 2);
        assert_eq!(file.viewpoints[0].name, "airship side");
        assert_eq!(
            file.viewpoints[0].pose().position,
            DVec3::new(40.0, 25.0, 120.0)
        );
        assert!(file.viewpoints[0].facets.is_empty());
        assert_eq!(
            file.viewpoints[1].facets["sun"].get_ron().trim(),
            "(elevation: 12.0, azimuth: 230.19)"
        );
    }

    #[test]
    fn a_newer_version_is_refused_by_version() {
        let text = "(version: 2, viewpoints: [], shiny_new_field: 3)";
        let err = parse_viewpoints(Path::new("v.ron"), text).unwrap_err();
        assert_eq!(err.kind, "unsupported_version");
        assert!(err.message.contains("v.ron"), "{err}");
    }

    #[test]
    fn a_parse_error_names_line_and_column() {
        let text = "(\n    version: 1,\n    viewpoints: [\n        (name: \"a\" position: (0.0, 0.0, 0.0)),\n    ],\n)";
        let err = parse_viewpoints(Path::new("v.ron"), text).unwrap_err();
        assert_eq!(err.kind, "invalid_file");
        assert!(err.message.starts_with("v.ron:4:"), "{err}");
    }

    #[test]
    fn a_non_finite_pose_is_invalid_pose() {
        for pose in [
            "position: (NaN, 1.0, 0.0), yaw: 0.0, pitch: 0.0",
            "position: (0.0, 1.0, 0.0), yaw: inf, pitch: 0.0",
            "position: (0.0, 1.0, 0.0), yaw: 0.0, pitch: NaN",
        ] {
            let text = format!("(version: 1, viewpoints: [(name: \"bad\", {pose})])");
            let err = parse_viewpoints(Path::new("v.ron"), &text).unwrap_err();
            assert_eq!(err.kind, "invalid_pose", "{pose}");
            assert!(
                err.message.contains("v.ron") && err.message.contains("bad"),
                "{err}"
            );
        }
    }

    fn viewpoint(name: &str, x: f64) -> Viewpoint {
        Viewpoint {
            name: name.into(),
            note: format!("note {name}"),
            position: [x, 1.0, 2.0],
            yaw: 10.0,
            pitch: -5.0,
            facets: BTreeMap::new(),
        }
    }

    #[test]
    fn a_save_writes_struct_syntax_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut app = app_with_sun();
        let mut with_sun = viewpoint("sunny", 1.0);
        with_sun.facets = capture_facets(app.world_mut(), &["sun".to_string()]).unwrap();
        save(&path, with_sun, false).unwrap();
        save(&path, viewpoint("plain", 2.0), false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("// Named camera viewpoints."), "{text}");
        assert!(text.contains("name: \"sunny\""), "{text}");
        assert!(!text.contains("\"name\":"), "a map, not a struct: {text}");
        assert!(
            text.contains("\"sun\": (elevation: 45.0, azimuth: 0.0)"),
            "{text}"
        );
        save(&path, viewpoint("third", 3.0), false).unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert!(
            again.contains("\"sun\": (elevation: 45.0, azimuth: 0.0)"),
            "{again}"
        );
        // The plain one has no empty facets map.
        assert_eq!(text.matches("facets").count(), 1, "{text}");
        // Replacing one keeps it in place.
        save(&path, viewpoint("plain", 20.0), true).unwrap();
        let file = load(&path).unwrap().unwrap();
        let names: Vec<_> = file.viewpoints.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["sunny", "plain", "third"]);
        assert_eq!(file.viewpoints[1].position[0], 20.0);
    }

    #[test]
    fn two_threads_updating_both_land() {
        #[derive(Serialize, Deserialize, Default)]
        struct Counters(BTreeMap<String, u32>);

        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let threads: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|key| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        update_ron_locked(&path, "", |counters: &mut Counters| {
                            *counters.0.entry(key.to_string()).or_default() += 1;
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let counters: Counters = read_ron(&path).unwrap();
        assert_eq!(counters.0["a"], 25);
        assert_eq!(counters.0["b"], 25);
    }
}
