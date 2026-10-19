//! When a deliberately opened stage counts as open, for [`StageOpened`].
//!
//! Not quite at its first spawn: a stage can hold nested loaded content (a `WorldAssetRoot` or
//! `DynamicWorldRoot` authored inside it), which spawns frames later. Membership tagging marks
//! those containers `StageLoading` too, and the stage is open once none of its own is still to
//! spawn (`ContentState::still_to_spawn`, which the `stage` settle probe uses too): a failed load
//! or a handle with neither an asset nor a load behind it is not waited for.
//!
//! A hot reload respawns a stage whose [`Opening`] is gone, so it is never an open.

use bevy::{
    prelude::*,
    world_serialization::{DynamicWorldRoot, WorldAssetRoot},
};

use super::{StageId, StageLoading, StageMember, loading::ContentState};

/// A stage was opened: one loaded by [`open_stage`](super::open_stage) or
/// [`open_stage_additive`](super::open_stage_additive) has spawned, nested content included, or
/// [`new_stage`](super::new_stage) created one. Not triggered when a loaded stage's file is
/// hot-reloaded and Bevy respawns it, so a reaction to this sees only deliberate opens.
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

/// Triggers [`StageOpened`] for every opening stage that has finished loading.
pub(super) fn announce_opened_stages(
    mut commands: Commands,
    openings: Query<(Entity, &Opening), Without<StageLoading>>,
    loading: Query<LoadingContent, With<StageLoading>>,
    content: ContentState,
) {
    for (container, opening) in &openings {
        let Some((root, id)) = opening.stage else {
            continue;
        };
        let nested_loading = loading.iter().any(|(member, dynamic, asset)| {
            member.0 == id && content.still_to_spawn(dynamic, asset)
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
