//! `open_stage`: the path-based entry point for loading a stage, and the observer that stamps
//! stage identity onto whatever a `WorldInstanceReady` event just finished spawning.
//!
//! Two cases share one `WorldInstanceReady` handler: opening a stage directly (the container
//! carries `SourcePath`, and its one child carries the file's own `StageId`), and loading nested
//! content under an already-open stage (e.g. a `WorldAssetRoot`-addressed glTF model authored
//! inside a stage file) -- there the container already carries `StageMembership` from having been
//! tagged as a descendant of its parent stage, and that membership is what propagates further
//! down. See `scratch/scenes-spec.md`.

use bevy::{platform::collections::HashMap, prelude::*};
use ename_asset_alias::ALIAS_SOURCE;

use super::{SourcePath, StageFormats, StageId, StageMembership, StageName};

/// Resolves `path` to a registered [`StageFormat`](super::StageFormat) by extension and spawns
/// its container entity. Panics if no format is registered for `path`'s extension -- a missing
/// format is a startup wiring bug, not a runtime condition to recover from.
pub fn open_stage(
    commands: &mut Commands,
    asset_server: &AssetServer,
    formats: &StageFormats,
    path: &str,
) -> Entity {
    let format = formats
        .for_path(path)
        .unwrap_or_else(|| panic!("no StageFormat registered for path {path:?}"));
    format.spawn_root(commands, asset_server, path)
}

/// Derives a stage's cosmetic name from the path it was opened with: the filename minus
/// extension for a plain path, or the alias's own name segment (the part after the last `::`)
/// for an `alias://` path, since there is no filename to read once the alias has resolved.
pub(super) fn stage_name_from_path(path: &str) -> String {
    let path = path
        .strip_prefix(&format!("{ALIAS_SOURCE}://"))
        .unwrap_or(path);
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = file_name
        .strip_suffix(".scn.ron")
        .or_else(|| file_name.strip_suffix(".scn"))
        .unwrap_or(file_name);
    stem.rsplit("::").next().unwrap_or(stem).to_owned()
}

/// Stamps `StageMembership` on everything a `WorldInstanceReady` event just finished spawning.
/// See the module doc for the two cases this handles.
pub(super) fn tag_stage_membership_on_ready(
    trigger: On<bevy::world_serialization::WorldInstanceReady>,
    children_of: Query<&Children>,
    child_of: Query<(Entity, &ChildOf)>,
    ids: Query<&StageId>,
    membership: Query<&StageMembership>,
    source_paths: Query<&SourcePath>,
    mut commands: Commands,
) {
    let container = trigger.event().entity;
    if container == Entity::PLACEHOLDER {
        return;
    }

    let stage_id = if let Ok(SourcePath(path)) = source_paths.get(container) {
        // Case 1: this container is a stage we opened ourselves. Its file's data is exactly one
        // top-level entity (the stage root), carrying the file's own StageId.
        let Ok(children) = children_of.get(container) else {
            return;
        };
        let [root] = &children[..] else {
            panic!(
                "stage file at {path:?} did not have exactly one top-level entity \
                 (the save-time invariant that guarantees this was violated)"
            );
        };
        let id = *ids
            .get(*root)
            .unwrap_or_else(|_| panic!("stage root entity in {path:?} is missing StageId"));
        commands
            .entity(*root)
            .insert(StageName(stage_name_from_path(path)))
            // A loaded stage root must be top-level; big_space validates floating origins
            // against the ultimate hierarchy ancestor, not this transient load container.
            .remove::<ChildOf>();
        id
    } else if let Ok(&StageMembership(id)) = membership.get(container) {
        // Case 2: nested content (e.g. a WorldAssetRoot-addressed model authored inside a stage)
        // that already belongs to a stage. Propagate that same membership to its new children.
        id
    } else {
        // Not stage-related content at all.
        return;
    };

    let Ok(children) = children_of.get(container) else {
        return;
    };
    let child_map = child_of.iter().fold(
        HashMap::<Entity, Vec<Entity>>::default(),
        |mut child_map, (child, parent)| {
            child_map.entry(parent.parent()).or_default().push(child);
            child_map
        },
    );
    for &child in children {
        tag_recursive(child, stage_id, &children_of, &child_map, &mut commands);
    }
}

fn tag_recursive(
    entity: Entity,
    id: StageId,
    children_of: &Query<&Children>,
    child_map: &HashMap<Entity, Vec<Entity>>,
    commands: &mut Commands,
) {
    commands.entity(entity).insert(StageMembership(id));
    if let Ok(children) = children_of.get(entity) {
        for &child in children {
            tag_recursive(child, id, children_of, child_map, commands);
        }
    } else if let Some(children) = child_map.get(&entity) {
        for &child in children {
            tag_recursive(child, id, children_of, child_map, commands);
        }
    }
}

use bevy::platform::collections::HashSet;

/// A [`save_stage`] call failed.
#[derive(Debug, thiserror::Error)]
pub enum SaveStageError {
    #[error("no StageFormat registered for {0:?}")]
    UnknownFormat(String),
    #[error(transparent)]
    Format(#[from] super::StageFormatError),
    #[error("failed to write stage file: {0}")]
    Io(#[from] std::io::Error),
}

/// Saves every entity tagged `StageMembership(id)` to `path`, choosing a format by `path`'s
/// extension. `world` is mutated: if the tagged entities have no single natural root (a lone
/// top-level entity with no `ChildOf` into the tagged set), a synthetic root is created and the
/// orphans reparented under it, so the saved file always has exactly one top-level entity -- the
/// invariant [`open_stage`]'s membership tagging depends on. See `scratch/scenes-spec.md`.
pub fn save_stage(path: &str, world: &mut World, id: StageId) -> Result<(), SaveStageError> {
    world.resource_scope(
        |world, formats: Mut<StageFormats>| -> Result<(), SaveStageError> {
            let mut query = world.query::<(Entity, &StageMembership)>();
            let tagged: Vec<Entity> = query
                .iter(world)
                .filter(|(_, membership)| membership.0 == id)
                .map(|(entity, _)| entity)
                .collect();

            let tagged_set: HashSet<Entity> = tagged.iter().copied().collect();
            let top_level: Vec<Entity> = tagged
                .iter()
                .copied()
                .filter(|&entity| {
                    world
                        .get::<ChildOf>(entity)
                        .is_none_or(|child_of| !tagged_set.contains(&child_of.0))
                })
                .collect();

            let root = match top_level.as_slice() {
                [single] => {
                    let single = *single;
                    if world.get::<StageId>(single).is_none() {
                        world.entity_mut(single).insert(id);
                    }
                    single
                }
                _ => {
                    let synthetic = world.spawn((id, StageMembership(id))).id();
                    for &orphan in &top_level {
                        world.entity_mut(orphan).insert(ChildOf(synthetic));
                    }
                    synthetic
                }
            };

            let mut entities = tagged;
            if !entities.contains(&root) {
                entities.push(root);
            }

            let format = formats
                .for_path(path)
                .ok_or_else(|| SaveStageError::UnknownFormat(path.to_owned()))?;
            let bytes = format.serialize(world, &entities)?;
            std::fs::write(path, bytes)?;
            Ok(())
        },
    )
}
