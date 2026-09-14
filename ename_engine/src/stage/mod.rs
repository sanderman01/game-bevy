//! Loading and saving stage content from `.scn.ron` asset files -- a stopgap until Bevy ships
//! scene *assets* for its new BSN system. Built on `bevy_world_serialization`
//! (`bevy::world_serialization`), the classic reflection-based scene serializer, renamed in this
//! Bevy version to free the `bevy_scene` name for BSN. See `scratch/scenes-spec.md` for the full
//! design.
//!
//! Called a "stage" rather than a "scene" in this project's own types, deliberately: Bevy's own
//! `Scene`/`bsn!`/`Template` (in `bevy_scene`, the new BSN system) already claims that name.

mod commands;
mod dynamic_world_format;
mod format;
mod identity;
mod workflow;

pub use commands::{SaveStageError, write_stage_file};
pub use dynamic_world_format::DynamicWorldFormat;
pub use format::{StageAppExt, StageFormat, StageFormatError, StageFormats};
pub use identity::{AssetRoot, SourcePath, StageId, StageMember, StageSource};
pub use workflow::{new_stage, open_stage, open_stage_additive, save_stage, stage_of};

use bevy::prelude::*;

/// Registers stage identity types for reflection, inserts a production-default `AssetRoot`, and
/// tags loaded stage content with its identity.
pub struct StagePlugin;

impl Plugin for StagePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AssetRoot(commands::default_asset_root()))
            .register_type::<StageId>()
            .register_type::<StageMember>()
            .register_type::<bevy::world_serialization::WorldAssetRoot>()
            .register_type::<bevy::world_serialization::DynamicWorldRoot>()
            // `Camera::viewport`'s `Range<f32>` lacks default serde data, breaking real save/load.
            .register_type_data::<std::ops::Range<f32>, bevy::reflect::ReflectSerialize>()
            .register_type_data::<std::ops::Range<f32>, bevy::reflect::ReflectDeserialize>()
            .add_observer(commands::tag_stage_membership_on_ready);
    }
}
