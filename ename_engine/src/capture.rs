//! Capture sources by name: what `screenshot.capture` and `video.record` may point at.
//!
//! The engine registers `window` and `game_camera`. A layer above adds its own, as the editor
//! adds `editor_camera`, so one command serves every source without knowing them. Each name
//! resolves to a [`CaptureSource`] when a command runs, and may carry a hint for when its camera
//! is not rendering, written by the crate that knows how to make it render.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use schemars::Schema;
use serde_json::Value;

use crate::{
    camera::single_main_camera,
    command::{CommandError, ErrorClass},
};

pub use crate::readback::CaptureSource;
pub use crate::readback::ReadbackSystems as CaptureSystems;

/// Finds the source a name stands for, at the moment a command runs.
pub type ResolveSource = fn(&mut World) -> Result<CaptureSource, CommandError>;

/// Every named capture source, and the hints for when one is not rendering.
///
/// Add sources in `Plugin::build`: the capture commands read the names in `finish`, to put them
/// in their schemas.
#[derive(Resource, Default)]
pub struct CaptureSources {
    resolvers: BTreeMap<String, ResolveSource>,
    /// Kept apart from `resolvers`, so a hint may be set before its source exists.
    inactive_hints: HashMap<String, String>,
}

impl CaptureSources {
    /// Adds or replaces a source.
    pub fn insert(&mut self, name: impl Into<String>, resolve: ResolveSource) {
        self.resolvers.insert(name.into(), resolve);
    }

    /// Added to a `camera_inactive` failure of that source.
    pub fn set_inactive_hint(&mut self, name: impl Into<String>, hint: impl Into<String>) {
        self.inactive_hints.insert(name.into(), hint.into());
    }

    pub fn names(&self) -> Vec<String> {
        self.resolvers.keys().cloned().collect()
    }

    /// The source `name` stands for right now.
    pub fn resolve(world: &mut World, name: &str) -> Result<CaptureSource, CommandError> {
        let resolve = world
            .get_resource::<Self>()
            .and_then(|sources| sources.resolvers.get(name).copied());
        let Some(resolve) = resolve else {
            let names = world
                .get_resource::<Self>()
                .map(Self::names)
                .unwrap_or_default();
            return Err(CommandError::invalid(
                "unknown_source",
                format!(
                    "no capture source `{name}`; this build has {}",
                    names.join(", ")
                ),
            ));
        };
        resolve(world)
    }

    /// A capture failure of source `name` as a command error, with the source's hint when its
    /// camera was not rendering.
    pub fn error(&self, name: &str, error: &impl CaptureError) -> CommandError {
        let kind = error.kind();
        let class = if error.is_invalid() {
            ErrorClass::Invalid
        } else {
            ErrorClass::Failed
        };
        let mut command_error = match class {
            ErrorClass::Invalid => CommandError::invalid(kind, error.to_string()),
            ErrorClass::Failed => CommandError::failed(kind, error.to_string()),
        };
        if kind == "camera_inactive"
            && let Some(hint) = self.inactive_hints.get(name)
        {
            command_error = command_error.with_hint(hint.clone());
        }
        command_error
    }
}

/// A screenshot or video failure, as [`CaptureSources::error`] reads it.
pub trait CaptureError: std::fmt::Display {
    /// Stable snake_case name, such as `camera_inactive`.
    fn kind(&self) -> &'static str;
    /// The request was wrong rather than the capture failing.
    fn is_invalid(&self) -> bool;
}

/// Limits a params schema's `source` property to the registered names, for
/// [`CommandSpec::adjust_params_schema`](crate::command::CommandSpec::adjust_params_schema). With
/// no names registered it leaves the schema alone: an empty enum would forbid every value.
pub fn restrict_source(names: Vec<String>) -> impl FnOnce(&mut Schema) + 'static {
    move |schema| {
        if names.is_empty() {
            return;
        }
        if let Some(source) = schema
            .get_mut("properties")
            .and_then(|properties| properties.get_mut("source"))
            .and_then(Value::as_object_mut)
        {
            source.insert("enum".into(), Value::from(names));
        }
    }
}

/// Registers [`CaptureSources`] with `window` and `game_camera`.
pub struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        let mut sources = app.world_mut().get_resource_or_init::<CaptureSources>();
        sources.insert("window", |_| Ok(CaptureSource::PrimaryWindow));
        sources.insert("game_camera", game_camera);
    }
}

fn game_camera(world: &mut World) -> Result<CaptureSource, CommandError> {
    match single_main_camera(world) {
        Ok(camera) => Ok(CaptureSource::Camera(camera)),
        Err(0) => Err(CommandError::failed(
            "no_main_camera",
            "the loaded stage has no MainCamera",
        )),
        Err(count) => Err(CommandError::failed(
            "no_main_camera",
            format!("there are {count} MainCameras, so which one is the game camera is ambiguous"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::RenderTarget;
    use serde_json::json;

    use super::*;
    use crate::camera::MainCamera;

    struct Inactive;
    impl std::fmt::Display for Inactive {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("dark")
        }
    }
    impl CaptureError for Inactive {
        fn kind(&self) -> &'static str {
            "camera_inactive"
        }
        fn is_invalid(&self) -> bool {
            false
        }
    }

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<CaptureSources>();
        let mut sources = world.resource_mut::<CaptureSources>();
        sources.insert("window", |_| Ok(CaptureSource::PrimaryWindow));
        sources.insert("game_camera", game_camera);
        world
    }

    #[test]
    fn resolves_the_window_and_the_one_main_camera() {
        let mut world = world();
        assert_eq!(
            CaptureSources::resolve(&mut world, "window"),
            Ok(CaptureSource::PrimaryWindow)
        );
        let err = CaptureSources::resolve(&mut world, "game_camera").unwrap_err();
        assert_eq!(err.kind, "no_main_camera");
        let camera = world
            .spawn((MainCamera, Camera::default(), RenderTarget::default()))
            .id();
        assert_eq!(
            CaptureSources::resolve(&mut world, "game_camera"),
            Ok(CaptureSource::Camera(camera))
        );
        world.spawn((MainCamera, Camera::default(), RenderTarget::default()));
        let err = CaptureSources::resolve(&mut world, "game_camera").unwrap_err();
        assert!(err.message.contains("ambiguous"), "{}", err.message);
    }

    #[test]
    fn an_unknown_source_lists_the_names() {
        let mut world = world();
        let err = CaptureSources::resolve(&mut world, "moon").unwrap_err();
        assert_eq!(err.kind, "unknown_source");
        assert!(
            err.message.contains("game_camera, window"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_hint_set_before_its_source_still_applies_and_only_when_inactive() {
        let mut sources = CaptureSources::default();
        sources.set_inactive_hint("editor_camera", "show the Scene View");
        sources.insert("editor_camera", |_| Ok(CaptureSource::PrimaryWindow));
        let err = sources.error("editor_camera", &Inactive);
        assert_eq!(err.hint.as_deref(), Some("show the Scene View"));
        assert_eq!(sources.error("window", &Inactive).hint, None);
    }

    #[test]
    fn restricts_the_source_property() {
        let mut schema = Schema::try_from(json!({
            "type": "object",
            "properties": { "source": { "type": "string" } }
        }))
        .unwrap();
        let unrestricted = schema.clone();
        restrict_source(Vec::new())(&mut schema);
        assert_eq!(schema, unrestricted);
        restrict_source(vec!["a".into(), "b".into()])(&mut schema);
        assert_eq!(
            schema.get("properties").unwrap()["source"]["enum"],
            json!(["a", "b"])
        );
    }
}
