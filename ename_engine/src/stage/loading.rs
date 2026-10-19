//! The `stage` settle probe: stage files still loading.

use bevy::{
    asset::LoadState,
    ecs::system::SystemParam,
    prelude::*,
    world_serialization::{DynamicWorld, DynamicWorldRoot, WorldAsset, WorldAssetRoot},
};

use super::StageLoading;
use crate::wait::Activity;

/// Counts load containers still waiting for `WorldInstanceReady`. A container whose file failed
/// to load never gets there and is not pending work, so it does not count.
pub(super) fn stage_probe(
    content: ContentState,
    containers: Query<(Option<&DynamicWorldRoot>, Option<&WorldAssetRoot>), With<StageLoading>>,
) -> Activity {
    let loading = containers
        .iter()
        .filter(|&(dynamic, asset)| content.still_to_spawn(dynamic, asset))
        .count();
    Activity::idle().pending("stages_loading", loading)
}

/// What decides whether a load container's content is still to come.
#[derive(SystemParam)]
pub(super) struct ContentState<'w> {
    server: Res<'w, AssetServer>,
    dynamic_worlds: Res<'w, Assets<DynamicWorld>>,
    world_assets: Res<'w, Assets<WorldAsset>>,
}

impl ContentState<'_> {
    /// Whether a `StageLoading` container's content is still to come. Not when its load failed,
    /// and not for a handle with neither an asset nor a load behind it (a default or placeholder
    /// handle): neither ever spawns. An asset added in memory has no load state but spawns all
    /// the same. Present or loaded counts as still to come until the spawn clears the marker.
    pub(super) fn still_to_spawn(
        &self,
        dynamic: Option<&DynamicWorldRoot>,
        asset: Option<&WorldAssetRoot>,
    ) -> bool {
        let (id, present) = match (dynamic, asset) {
            (Some(root), _) => (root.id().untyped(), self.dynamic_worlds.contains(root.id())),
            (None, Some(root)) => (root.id().untyped(), self.world_assets.contains(root.id())),
            (None, None) => return true,
        };
        match self.server.get_load_state(id) {
            Some(LoadState::Loading | LoadState::Loaded) => true,
            Some(LoadState::NotLoaded) => present || self.server.get_path(id).is_some(),
            Some(LoadState::Failed(_)) => false,
            None => present,
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::{
        app::TaskPoolPlugin, asset::AssetPlugin, ecs::system::RunSystemOnce,
        world_serialization::WorldSerializationPlugin,
    };

    use super::*;

    fn still_to_spawn(app: &mut App, root: DynamicWorldRoot) -> bool {
        app.world_mut()
            .run_system_once(move |content: ContentState| content.still_to_spawn(Some(&root), None))
            .unwrap()
    }

    #[test]
    fn an_asset_added_in_memory_is_still_to_spawn_and_a_bare_handle_is_not() {
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            AssetPlugin::default(),
            WorldSerializationPlugin,
        ));
        let generated = app
            .world_mut()
            .resource_mut::<Assets<DynamicWorld>>()
            .add(DynamicWorld::default());

        assert!(still_to_spawn(&mut app, DynamicWorldRoot(generated)));
        assert!(!still_to_spawn(
            &mut app,
            DynamicWorldRoot(Handle::default())
        ));
    }
}
