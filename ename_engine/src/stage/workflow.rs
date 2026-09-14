//! The four stage operations the editor's File menu (and the game's own startup) drive: unload-
//! then-load (`new_stage`, `open_stage`), keep-others-loaded (`open_stage_additive`), and
//! save-back-to-source (`save_stage`). Built on the load/save primitives in `commands.rs`.

use bevy::prelude::*;
use ename_asset_alias::ContentIndex;
use uuid::Uuid;

use super::{
    AssetRoot, SaveStageError, SourcePath, StageId, StageMember, StageSource,
    commands::{resolve_source_path, spawn_stage_root, write_stage_file},
};

/// Unloads every currently loaded stage and spawns a fresh, empty one. Returns its root entity,
/// so a caller (the editor's File menu) can select it.
pub fn new_stage(world: &mut World) -> Entity {
    despawn_stage_members(world, None);
    let id = StageId(Uuid::new_v4());
    world
        .spawn((id, StageMember(id), Name::new("New Stage")))
        .id()
}

/// Unloads every currently loaded stage, then starts loading `path` (a plain asset path or an
/// `alias://` URL). Returns the load's container entity immediately -- loading is async.
pub fn open_stage(world: &mut World, path: &str) -> Entity {
    despawn_stage_members(world, None);
    spawn_stage_root(world, path)
}

/// Starts loading `path`, leaving every other currently loaded stage in place. If `path` is
/// already loaded (matched by [`StageSource`]), that stage's entities and container are unloaded
/// first, so the result is a clean reload rather than a duplicate. If `path` matches no loaded
/// stage but a container for it is still mid-load (e.g. a rapid double-click before the first one
/// resolved), that pending container is unloaded too.
pub fn open_stage_additive(world: &mut World, path: &str) -> Entity {
    match stage_id_for_source(world, path) {
        Some(id) => despawn_stage_members(world, Some(id)),
        None => despawn_pending_container(world, path),
    }
    spawn_stage_root(world, path)
}

/// Saves the stage identified by `id` back to wherever it was last loaded from or saved to.
/// Returns [`SaveStageError::NoSource`] if it carries no [`StageSource`] yet -- a stage created by
/// [`new_stage`] that has never been saved has nowhere to write back to, and the caller (the
/// editor) is expected to ask the user for a path and call
/// [`write_stage_file`](super::write_stage_file) directly in that case.
pub fn save_stage(world: &mut World, id: StageId) -> Result<(), SaveStageError> {
    let relative = stage_source_path(world, id).ok_or(SaveStageError::NoSource)?;
    let root = world.resource::<AssetRoot>().0.clone();
    write_stage_file(&root.join(&relative).to_string_lossy(), world, id)
}

/// The `StageId` every one of `entities` belongs to, if there is exactly one such id and at least
/// one of `entities` carries `StageMember` at all. Used to turn "what's selected in the Hierarchy
/// panel" into "which stage the Save Stage menu item acts on".
pub fn stage_of(world: &World, entities: &[Entity]) -> Option<StageId> {
    let mut members = entities
        .iter()
        .filter_map(|&entity| world.get::<StageMember>(entity));
    let first = members.next()?.0;
    members.all(|member| member.0 == first).then_some(first)
}

/// Despawns every entity carrying `StageMember(id)` and any load container whose `SourcePath`
/// matches that stage's own recorded `StageSource` -- or, when `id` is `None`, every stage-tagged
/// entity and every load container at all, regardless of which stage (or none yet) it belongs to.
///
/// A container is never `StageMember`-tagged (see this plan's design notes on why it's still part
/// of what an unload must remove), so `id: None` also matches on `SourcePath` directly, and
/// `id: Some(_)` looks up that one stage's source to match its own container specifically.
fn despawn_stage_members(world: &mut World, id: Option<StageId>) {
    let matching_source = id.and_then(|id| stage_source_path(world, id));

    // Collected as owned data first: the query below needs `&mut World` to build, and normalizing
    // a container's `SourcePath` below needs `&ContentIndex` from the same `World` -- releasing
    // the query's borrow here lets both coexist.
    let candidates: Vec<(Entity, Option<StageId>, Option<String>)> = world
        .query::<(Entity, Option<&StageMember>, Option<&SourcePath>)>()
        .iter(world)
        .map(|(entity, member, source)| {
            (
                entity,
                member.map(|member| member.0),
                source.map(|source| source.0.clone()),
            )
        })
        .collect();

    // A container's `SourcePath` is recorded exactly as it was passed to `spawn_stage_root` (an
    // `alias://` URL, for a stage `ename_game` opened by alias), but `StageSource` on that stage's
    // root is always the normalized form (see `resolve_source_path`'s doc). Comparing the two
    // directly would never match an alias-loaded stage's own container to itself.
    let content_index = world.get_resource::<ContentIndex>();
    let stale: Vec<Entity> = candidates
        .into_iter()
        .filter(|(_, member, source)| match id {
            None => member.is_some() || source.is_some(),
            Some(id) => {
                member.is_some_and(|member| member == id)
                    || source.as_deref().is_some_and(|source| {
                        Some(resolve_source_path(source, content_index)) == matching_source
                    })
            }
        })
        .map(|(entity, ..)| entity)
        .collect();
    despawn_all(world, stale);
}

/// Despawns any load container whose `SourcePath` equals `path` exactly -- for a container that
/// has not finished loading yet and so carries neither `StageId` nor `StageSource`.
fn despawn_pending_container(world: &mut World, path: &str) {
    let stale: Vec<Entity> = world
        .query::<(Entity, &SourcePath)>()
        .iter(world)
        .filter(|(_, source)| source.0 == path)
        .map(|(entity, _)| entity)
        .collect();
    despawn_all(world, stale);
}

fn despawn_all(world: &mut World, entities: Vec<Entity>) {
    for entity in entities {
        // A parent's despawn cascades to its children (Bevy's linked-spawn hierarchy
        // relationship), so a descendant collected above may already be gone by its turn.
        if world.entities().contains(entity) {
            world.despawn(entity);
        }
    }
}

/// The asset-relative `StageSource` of the loaded stage identified by `id`, if any.
fn stage_source_path(world: &mut World, id: StageId) -> Option<String> {
    let mut query = world.query::<(&StageId, &StageSource)>();
    query
        .iter(world)
        .find(|(stage_id, _)| **stage_id == id)
        .map(|(_, source)| source.0.clone())
}

/// The `StageId` of whichever loaded stage's `StageSource` equals `path`, if any.
fn stage_id_for_source(world: &mut World, path: &str) -> Option<StageId> {
    let mut query = world.query::<(&StageId, &StageSource)>();
    query
        .iter(world)
        .find(|(_, source)| source.0 == path)
        .map(|(id, _)| *id)
}
