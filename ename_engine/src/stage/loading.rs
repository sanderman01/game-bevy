//! The `stage` settle probe: stage files still loading.

use bevy::{
    asset::LoadState,
    prelude::*,
    world_serialization::{DynamicWorldRoot, WorldAssetRoot},
};

use super::StageLoading;
use crate::wait::Activity;

/// Counts load containers still waiting for `WorldInstanceReady`. A container whose file failed
/// to load never gets there and is not pending work, so it does not count.
pub(super) fn stage_probe(
    server: Res<AssetServer>,
    containers: Query<(Option<&DynamicWorldRoot>, Option<&WorldAssetRoot>), With<StageLoading>>,
) -> Activity {
    let loading = containers
        .iter()
        .filter(|&(dynamic, asset)| still_to_spawn(&server, dynamic, asset))
        .count();
    Activity::idle().pending("stages_loading", loading)
}

/// Whether a `StageLoading` container's content is still to come. Not when its load failed, and
/// not for a handle with no load behind it (a default or placeholder handle, or one never given a
/// path): neither ever spawns. Loaded counts as still to come until the spawn clears the marker.
pub(super) fn still_to_spawn(
    server: &AssetServer,
    dynamic: Option<&DynamicWorldRoot>,
    asset: Option<&WorldAssetRoot>,
) -> bool {
    let Some(id) = dynamic
        .map(|root| root.id().untyped())
        .or_else(|| asset.map(|root| root.id().untyped()))
    else {
        return true;
    };
    match server.get_load_state(id) {
        Some(LoadState::Loading | LoadState::Loaded) => true,
        Some(LoadState::NotLoaded) => server.get_path(id).is_some(),
        Some(LoadState::Failed(_)) | None => false,
    }
}
