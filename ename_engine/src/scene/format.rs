//! The scene-format seam: one on-disk representation per registered [`SceneFormat`], dispatched
//! by file extension so `open_scene`/`save_scene` never need to know which format they're using.

use bevy::{platform::collections::HashMap, prelude::*};

/// One on-disk scene representation: how to spawn it, and how to serialize a tagged subset of
/// the `World` back into bytes for the same extension.
///
/// Implementations must be cheap to construct: [`SceneAppExt::register_scene_format`] takes one
/// by value and boxes it once, at startup.
pub trait SceneFormat: Send + Sync + 'static {
    /// The file extension this format claims, without a leading dot (e.g. `"scn.ron"`). Must
    /// match a registered `AssetLoader`'s extension.
    fn extension(&self) -> &str;

    /// Spawns the container entity for `path`, wired so the format's own spawner populates it
    /// with the file's content as children. Returns the container immediately -- loading is
    /// async, so the content is not there yet.
    fn spawn_root(&self, commands: &mut Commands, asset_server: &AssetServer, path: &str)
    -> Entity;

    /// Extracts `entities` into bytes for this format, using `world`'s `AppTypeRegistry`.
    fn serialize(&self, world: &World, entities: &[Entity]) -> Result<Vec<u8>, SceneFormatError>;
}

/// A [`SceneFormat`] failed to turn a set of entities into bytes.
#[derive(Debug, thiserror::Error)]
pub enum SceneFormatError {
    #[error("failed to serialize scene: {0}")]
    Ron(#[from] ron::Error),
}

/// The registered [`SceneFormat`]s, keyed by extension. Exactly one format ships today
/// (`DynamicWorldFormat`); a second one is added with a single
/// [`register_scene_format`](SceneAppExt::register_scene_format) call, no changes to existing
/// files or call sites required.
#[derive(Resource, Default)]
pub struct SceneFormats(HashMap<String, Box<dyn SceneFormat>>);

impl SceneFormats {
    fn insert<F: SceneFormat>(&mut self, format: F) {
        self.0
            .insert(format.extension().to_owned(), Box::new(format));
    }

    /// The registered format whose extension `path` ends with. When more than one extension
    /// matches (e.g. `"ron"` and `"scn.ron"` both registered), the longest wins.
    pub fn for_path(&self, path: &str) -> Option<&dyn SceneFormat> {
        self.0
            .iter()
            .filter(|(extension, _)| path.ends_with(&format!(".{extension}")))
            .max_by_key(|(extension, _)| extension.len())
            .map(|(_, format)| format.as_ref())
    }

    /// The format to use when creating a brand-new scene with no existing file to infer one
    /// from. Unambiguous with the one format this project ships; if a second is ever added, the
    /// first-registered one wins arbitrarily -- revisit if that starts to matter.
    pub fn default_format(&self) -> Option<&dyn SceneFormat> {
        self.0.values().next().map(Box::as_ref)
    }
}

/// Extends [`App`] with scene-format registration.
pub trait SceneAppExt {
    /// Registers `format`, so `open_scene`/`save_scene` dispatch to it for paths ending in its
    /// extension.
    fn register_scene_format<F: SceneFormat>(&mut self, format: F) -> &mut Self;
}

impl SceneAppExt for App {
    fn register_scene_format<F: SceneFormat>(&mut self, format: F) -> &mut Self {
        self.world_mut()
            .get_resource_or_insert_with(SceneFormats::default)
            .insert(format);
        self
    }
}
