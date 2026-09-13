//! [`SceneFormat`] backed by `bevy_world_serialization`'s `DynamicWorld`/`DynamicWorldRoot`
//! (`bevy::world_serialization` in this Bevy version -- the crate was renamed to free
//! `bevy_scene` for BSN, see `scratch/scenes-spec.md`).

use bevy::{prelude::*, world_serialization::DynamicWorldBuilder};

use super::{SceneFormat, SceneFormatError, SourcePath};

/// The one scene format this project ships today: Bevy's classic reflection-based scene
/// serializer, `.scn`/`.scn.ron`.
pub struct DynamicWorldFormat;

impl SceneFormat for DynamicWorldFormat {
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

    fn serialize(&self, world: &World, entities: &[Entity]) -> Result<Vec<u8>, SceneFormatError> {
        let type_registry = world.resource::<AppTypeRegistry>().read();
        let dynamic_world = DynamicWorldBuilder::from_world(world, &type_registry)
            .extract_entities(entities.iter().copied())
            .build();
        Ok(dynamic_world.serialize(&type_registry)?.into_bytes())
    }
}
