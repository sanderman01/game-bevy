//! The three identity concepts a scene carries, deliberately not conflated: a stable id, a
//! membership tag keyed by that id, and a cosmetic display name. See `scratch/scenes-spec.md`.

use bevy::prelude::*;
use uuid::Uuid;

/// A scene's persistent identity. Lives on the scene's root entity and round-trips through the
/// file on every save/reload. Minted once, the first time a scene is saved.
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug, Reflect)]
#[reflect(Component)]
pub struct SceneId(pub Uuid);

/// Marks an entity as belonging to the scene identified by the wrapped [`SceneId`]. This is the
/// only thing [`save_scene`](crate::scene::save_scene) looks at to decide what to extract, so an
/// entity without it (editor UI, gizmos, the pointer) is never captured.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Reflect)]
#[reflect(Component)]
pub struct SceneMembership(pub SceneId);

/// Cosmetic display name for a scene, stamped on its root entity when the scene is opened.
/// Computed from the path/alias it was opened with, never persisted in the file itself: renaming
/// the file (or re-aliasing the scene) changes the displayed name for free on next load.
#[derive(Component, Clone, Debug, Reflect)]
#[reflect(Component)]
pub struct SceneName(pub String);

/// Recorded on the transient container entity by a [`SceneFormat`](crate::scene::SceneFormat)'s
/// `spawn_root`, so the `WorldInstanceReady` observer can compute [`SceneName`] once the scene's
/// content has finished spawning. Describes the container, not scene content, so it is never
/// `Reflect` and never saved.
#[derive(Component, Clone, Debug)]
pub struct SourcePath(pub String);
