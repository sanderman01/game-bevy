//! Loading and saving scene content from `.scn.ron` asset files -- a stopgap until Bevy ships
//! scene *assets* for its new BSN system. Built on `bevy_world_serialization`
//! (`bevy::world_serialization`), the classic reflection-based scene serializer, renamed in this
//! Bevy version to free the `bevy_scene` name for BSN. See `scratch/scenes-spec.md` for the full
//! design.

mod alias;
mod commands;
mod dynamic_world_format;
mod format;
mod identity;

pub use alias::{open_scene_by_alias, save_scene_by_alias};
pub use commands::{SaveSceneError, open_scene, save_scene};
pub use dynamic_world_format::DynamicWorldFormat;
pub use format::{SceneAppExt, SceneFormat, SceneFormatError, SceneFormats};
pub use identity::{SceneId, SceneMembership, SceneName, SourcePath};

use bevy::prelude::*;

/// Registers scene identity types for reflection and tags loaded scene content with its identity.
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<SceneId>()
            .register_type::<SceneMembership>()
            .register_type::<SceneName>()
            .register_type::<bevy::world_serialization::WorldAssetRoot>()
            .register_type::<bevy::world_serialization::DynamicWorldRoot>()
            // `Camera::viewport`'s `Range<f32>` lacks default serde data, breaking real save/load.
            .register_type_data::<std::ops::Range<f32>, bevy::reflect::ReflectSerialize>()
            .register_type_data::<std::ops::Range<f32>, bevy::reflect::ReflectDeserialize>()
            .add_observer(commands::tag_membership_on_ready);
    }
}
