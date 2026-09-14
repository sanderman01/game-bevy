//! The three identity concepts a stage carries, deliberately not conflated: a stable id, a
//! membership tag keyed by that id, and a cosmetic display name. See `scratch/scenes-spec.md`.

use bevy::prelude::*;
use uuid::Uuid;

/// A stage's persistent identity. Lives on the stage's root entity and round-trips through the
/// file on every save/reload. Minted once, the first time a stage is saved.
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug, Reflect)]
#[reflect(Component)]
pub struct StageId(pub Uuid);

/// Marks an entity as belonging to the stage identified by the wrapped [`StageId`]. This is the
/// only thing [`save_stage`](crate::stage::save_stage) looks at to decide what to extract, so an
/// entity without it (editor UI, gizmos, the pointer) is never captured.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Reflect)]
#[reflect(Component)]
pub struct StageMembership(pub StageId);

/// Cosmetic display name for a stage, stamped on its root entity when the stage is opened.
/// Computed from the path/alias it was opened with, never persisted in the file itself: renaming
/// the file (or re-aliasing the stage) changes the displayed name for free on next load.
#[derive(Component, Clone, Debug, Reflect)]
#[reflect(Component)]
pub struct StageName(pub String);

/// Recorded on the transient container entity by a [`StageFormat`](crate::stage::StageFormat)'s
/// `spawn_root`, so the `WorldInstanceReady` observer can compute [`StageName`] once the stage's
/// content has finished spawning. Describes the container, not stage content, so it is never
/// `Reflect` and never saved.
#[derive(Component, Clone, Debug)]
pub struct SourcePath(pub String);
