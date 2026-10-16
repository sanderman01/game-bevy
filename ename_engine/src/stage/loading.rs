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
        .filter(|(dynamic, asset)| {
            let id = dynamic
                .map(|root| root.id().untyped())
                .or_else(|| asset.map(|root| root.id().untyped()));
            !id.is_some_and(|id| matches!(server.get_load_state(id), Some(LoadState::Failed(_))))
        })
        .count();
    Activity::idle().pending("stages_loading", loading)
}
