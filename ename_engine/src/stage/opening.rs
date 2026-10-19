//! When a deliberately opened stage counts as open, for [`StageOpened`].
//!
//! Not at its first spawn, for two reasons:
//! - Opening a file that is already loaded asks the asset server for a reload, but the container
//!   spawns the cached copy first; the reloaded file then respawns it. Until the reload has
//!   arrived ([`AwaitingReload`]) the container stays `StageLoading`.
//! - A stage can hold nested loaded content (a `WorldAssetRoot` or `DynamicWorldRoot` authored
//!   inside it), which spawns frames later. Membership tagging marks those containers
//!   `StageLoading` too, and the stage is open once none of its own is still loading. A load that
//!   failed is not waited for, as in the `stage` settle probe.
//!
//! A hot reload respawns a stage whose [`Opening`] is gone, so it is never an open.

use bevy::{
    asset::{AssetEvent, AssetLoadFailedEvent, LoadState, UntypedAssetId},
    prelude::*,
    world_serialization::{DynamicWorld, DynamicWorldRoot, WorldAssetRoot},
};

use super::{StageId, StageLoading, StageMember};

/// A stage was opened: one loaded by [`open_stage`](super::open_stage) or
/// [`open_stage_additive`](super::open_stage_additive) has spawned from the file as it is on disk,
/// nested content included, or [`new_stage`](super::new_stage) created one. Not triggered when a
/// loaded stage's file is hot-reloaded and Bevy respawns it, so a reaction to this sees only
/// deliberate opens.
#[derive(Event, Debug, Clone, Copy)]
pub struct StageOpened {
    /// The stage's root entity.
    pub root: Entity,
    pub id: StageId,
    /// Whether opening it unloaded every other stage ([`open_stage`](super::open_stage),
    /// [`new_stage`](super::new_stage)), rather than adding to them.
    pub replaced_others: bool,
}

/// Loaded content a stage holds inside it, which spawns after the stage itself.
pub(super) type NestedContent = Or<(With<WorldAssetRoot>, With<DynamicWorldRoot>)>;

type LoadingContent = (
    &'static StageMember,
    Option<&'static DynamicWorldRoot>,
    Option<&'static WorldAssetRoot>,
);

/// On a load container from the open until [`StageOpened`] is triggered for it.
#[derive(Component, Clone, Copy, Debug)]
pub(super) struct Opening {
    pub(super) replaced_others: bool,
    /// The stage root and id its latest spawn produced; `None` until the first.
    pub(super) stage: Option<(Entity, StageId)>,
}

/// On a load container opened while its file was already loaded, until the requested reload has
/// arrived: the spawns before that are of the cached copy.
#[derive(Component, Clone, Copy, Debug)]
pub(super) struct AwaitingReload(pub(super) UntypedAssetId);

/// Clears [`AwaitingReload`] when its reload arrives. A container that already spawned the
/// cached copy is told to spawn again, from the reloaded asset: whether Bevy's own respawn on the
/// asset change follows depends on its debounce, so it is not relied on. A failed reload leaves
/// the cached copy as the stage, so it ends the load there.
pub(super) fn finish_reloads(
    mut commands: Commands,
    mut events: MessageReader<AssetEvent<DynamicWorld>>,
    mut failures: MessageReader<AssetLoadFailedEvent<DynamicWorld>>,
    mut awaiting: Query<(Entity, &AwaitingReload, &Opening, &mut DynamicWorldRoot)>,
) {
    for event in events.read() {
        // The reload finishing. Not `Added` or `Modified`: `Assets` flushes those on its own
        // schedule, so one can arrive frames late, from the load before.
        let AssetEvent::LoadedWithDependencies { id } = event else {
            continue;
        };
        for (container, reload, opening, mut root) in &mut awaiting {
            if reload.0 != id.untyped() {
                continue;
            }
            commands.entity(container).remove::<AwaitingReload>();
            if opening.stage.is_some() {
                root.set_changed();
            }
        }
    }
    for failure in failures.read() {
        for (container, reload, opening, _) in &awaiting {
            if reload.0 != failure.id.untyped() {
                continue;
            }
            let mut container = commands.entity(container);
            container.remove::<AwaitingReload>();
            if opening.stage.is_some() {
                container.remove::<StageLoading>();
            }
        }
    }
}

/// Triggers [`StageOpened`] for every opening stage that has finished loading.
pub(super) fn announce_opened_stages(
    mut commands: Commands,
    openings: Query<(Entity, &Opening), Without<StageLoading>>,
    loading: Query<LoadingContent, With<StageLoading>>,
    server: Res<AssetServer>,
) {
    for (container, opening) in &openings {
        let Some((root, id)) = opening.stage else {
            continue;
        };
        let nested_loading = loading.iter().any(|(member, dynamic, asset)| {
            let asset_id = dynamic
                .map(|root| root.id().untyped())
                .or_else(|| asset.map(|root| root.id().untyped()));
            member.0 == id
                && !asset_id.is_some_and(|asset_id| {
                    matches!(server.get_load_state(asset_id), Some(LoadState::Failed(_)))
                })
        });
        if nested_loading {
            continue;
        }
        commands.entity(container).remove::<Opening>();
        commands.trigger(StageOpened {
            root,
            id,
            replaced_others: opening.replaced_others,
        });
    }
}
