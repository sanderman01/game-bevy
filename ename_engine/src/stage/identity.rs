//! The two identity concepts a stage carries, deliberately not conflated: a stable id and a
//! membership tag keyed by that id. Display naming is not a third concept here -- it's the
//! stage root's ordinary `bevy_ecs::name::Name`, stamped from the path/alias the stage was
//! opened with, same as any other named entity. See `scratch/scenes-spec.md`.

use bevy::prelude::*;
use std::path::PathBuf;
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
pub struct StageMember(pub StageId);

/// Recorded on the transient container entity by a [`StageFormat`](crate::stage::StageFormat)'s
/// `spawn_root`, so the `WorldInstanceReady` observer can compute the stage root's `Name` once
/// the stage's content has finished spawning. Describes the container, not stage content, so it
/// is never `Reflect` and never saved.
#[derive(Component, Clone, Debug)]
pub struct SourcePath(pub String);

/// Where a stage root was last loaded from or saved to, as an asset-relative path (relative to
/// whatever [`AssetRoot`] resolves to) -- never an `alias://` URL, even if that's how the stage
/// was opened; see `commands.rs`'s ready-observer for the normalization.
/// [`save_stage`](crate::stage::save_stage) reads this to find a file to write back to, and
/// [`open_stage_additive`](crate::stage::open_stage_additive) reads it to tell whether a
/// requested path is already loaded. Never `Reflect`, so it never round-trips through a save --
/// same reasoning as [`SourcePath`], and for the same reason: a stage's own file must not embed
/// the path it happened to be opened from.
#[derive(Component, Clone, Debug)]
pub struct StageSource(pub String);

/// The filesystem directory [`StageSource`]'s asset-relative paths resolve against for reading
/// and writing actual files. [`StagePlugin`](super::StagePlugin) inserts a production default
/// computed from `bevy_asset`'s own base-path heuristic (see `commands.rs`'s
/// `default_asset_root`); a test that configures `AssetPlugin` with a different `file_path` must
/// insert its own value *after* adding `StagePlugin`, so it overrides that default rather than
/// being overridden by it.
#[derive(Resource, Clone, Debug)]
pub struct AssetRoot(pub PathBuf);
