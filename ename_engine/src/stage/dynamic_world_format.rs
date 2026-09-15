//! [`StageFormat`] backed by `bevy_world_serialization`'s `DynamicWorld`/`DynamicWorldRoot`
//! (`bevy::world_serialization` in this Bevy version -- the crate was renamed to free
//! `bevy_scene` for BSN, see `scratch/scenes-spec.md`).

use bevy::{log::warn, prelude::*, world_serialization::DynamicWorldBuilder};

use super::{SourcePath, StageFormat, StageFormatError};

/// The one stage format this project ships today: Bevy's classic reflection-based scene
/// serializer, `.scn`/`.scn.ron`.
///
/// Procedurally-created, non-file-backed assets are not preserved by this format. Serialization
/// warns when it detects the placeholder asset ID, but callers must author those assets as files.
pub struct DynamicWorldFormat;

impl StageFormat for DynamicWorldFormat {
    fn extension(&self) -> &str {
        "scn.ron"
    }

    fn spawn_root(
        &self,
        commands: &mut Commands,
        asset_server: &AssetServer,
        path: &str,
    ) -> Entity {
        commands
            .spawn((
                DynamicWorldRoot(asset_server.load(path.to_owned())),
                SourcePath(path.to_owned()),
            ))
            .id()
    }

    fn serialize(&self, world: &World, entities: &[Entity]) -> Result<Vec<u8>, StageFormatError> {
        let type_registry = world.resource::<AppTypeRegistry>().read();
        let dynamic_world = DynamicWorldBuilder::from_world(world, &type_registry)
            // `VisibilityClass` made the real stage save fail: its `TypeId`s cannot serialize,
            // and Bevy's visibility system recomputes it on load, so excluding it loses no data.
            .deny_component::<bevy::camera::visibility::VisibilityClass>()
            // Bevy marks these defaulted camera settings opaque; defaults are restored on load.
            .deny_component::<bevy::camera::CameraMainTextureUsages>()
            .deny_component::<bevy::camera::Exposure>()
            // Camera requirements recreate this runtime-interned render graph selection on load.
            .deny_component::<bevy::render::camera::CameraRenderGraph>()
            // `Children` is a derived list `ChildOf`'s insert hook rebuilds on load; serializing
            // it verbatim writes entity ids that may not be in `entities` (e.g. a non-StageMember
            // entity, like the editor's GridFollowCamera, temporarily parented under a stage's
            // Grid) with no corresponding entry in the file's own entity map -- a dangling
            // reference that resolves to a phantom entity on load. Denying it loses no
            // information.
            .deny_component::<Children>()
            .extract_entities(entities.iter().copied())
            .build();
        let bytes = dynamic_world.serialize(&type_registry)?.into_bytes();
        warn_on_placeholder_asset_ids(&bytes);
        Ok(bytes)
    }
}

/// `AssetId::<A>::DEFAULT_UUID` is what a `Handle` serializes to when it has no `AssetPath` --
/// e.g. a mesh or material built with `Assets::add(...)` at runtime rather than loaded from a
/// file. Such a handle carries no usable data in the saved file and will not resolve to real
/// content on reload. This cannot be caught structurally without walking arbitrary reflected data
/// for `Handle<T>` fields, so it is a cheap textual check instead -- a warning, not a fix: the
/// caller must author the asset as a real file and address it by path or alias.
fn warn_on_placeholder_asset_ids(bytes: &[u8]) {
    let placeholder = bevy::asset::AssetId::<bevy::prelude::Mesh>::DEFAULT_UUID.to_string();
    if let Ok(text) = std::str::from_utf8(bytes)
        && text.contains(&placeholder)
    {
        warn!(
            "stage save contains a Handle with no AssetPath (serialized as the placeholder id \
             {placeholder}) -- this asset was created at runtime rather than loaded from a file, \
             and will not survive a reload. Author it as a real asset file instead."
        );
    }
}
