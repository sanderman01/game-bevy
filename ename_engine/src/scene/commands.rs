//! `open_scene`: the path-based entry point for loading a scene, and the observer that stamps
//! scene identity onto whatever a `WorldInstanceReady` event just finished spawning.
//!
//! Two cases share one `WorldInstanceReady` handler: opening a scene directly (the container
//! carries `SourcePath`, and its one child carries the file's own `SceneId`), and loading nested
//! content under an already-open scene (e.g. a `WorldAssetRoot`-addressed glTF model authored
//! inside a scene file) -- there the container already carries `SceneMembership` from having been
//! tagged as a descendant of its parent scene, and that membership is what propagates further
//! down. See `scratch/scenes-spec.md`.

use bevy::{platform::collections::HashMap, prelude::*};
use ename_asset_alias::ALIAS_SOURCE;

use super::{SceneFormats, SceneId, SceneMembership, SceneName, SourcePath};

/// Resolves `path` to a registered [`SceneFormat`](super::SceneFormat) by extension and spawns
/// its container entity. Panics if no format is registered for `path`'s extension -- a missing
/// format is a startup wiring bug, not a runtime condition to recover from.
pub fn open_scene(
    commands: &mut Commands,
    asset_server: &AssetServer,
    formats: &SceneFormats,
    path: &str,
) -> Entity {
    let format = formats
        .for_path(path)
        .unwrap_or_else(|| panic!("no SceneFormat registered for path {path:?}"));
    format.spawn_root(commands, asset_server, path)
}

/// Derives a scene's cosmetic name from the path it was opened with: the filename minus
/// extension for a plain path, or the alias's own name segment (the part after the last `::`)
/// for an `alias://` path, since there is no filename to read once the alias has resolved.
pub(super) fn scene_name_from_path(path: &str) -> String {
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

/// Stamps `SceneMembership` on everything a `WorldInstanceReady` event just finished spawning.
/// See the module doc for the two cases this handles.
pub(super) fn tag_membership_on_ready(
    trigger: On<bevy::world_serialization::WorldInstanceReady>,
    children_of: Query<&Children>,
    child_of: Query<(Entity, &ChildOf)>,
    ids: Query<&SceneId>,
    membership: Query<&SceneMembership>,
    source_paths: Query<&SourcePath>,
    mut commands: Commands,
) {
    let container = trigger.event().entity;
    if container == Entity::PLACEHOLDER {
        return;
    }

    let scene_id = if let Ok(SourcePath(path)) = source_paths.get(container) {
        // Case 1: this container is a scene we opened ourselves. Its file's data is exactly one
        // top-level entity (the scene root), carrying the file's own SceneId.
        let Ok(children) = children_of.get(container) else {
            return;
        };
        let [root] = &children[..] else {
            panic!(
                "scene file at {path:?} did not have exactly one top-level entity \
                 (the save-time invariant that guarantees this was violated)"
            );
        };
        let id = *ids
            .get(*root)
            .unwrap_or_else(|_| panic!("scene root entity in {path:?} is missing SceneId"));
        commands
            .entity(*root)
            .insert(SceneName(scene_name_from_path(path)));
        id
    } else if let Ok(&SceneMembership(id)) = membership.get(container) {
        // Case 2: nested content (e.g. a WorldAssetRoot-addressed model authored inside a scene)
        // that already belongs to a scene. Propagate that same membership to its new children.
        id
    } else {
        // Not scene-related content at all.
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
        tag_recursive(child, scene_id, &children_of, &child_map, &mut commands);
    }
}

fn tag_recursive(
    entity: Entity,
    id: SceneId,
    children_of: &Query<&Children>,
    child_map: &HashMap<Entity, Vec<Entity>>,
    commands: &mut Commands,
) {
    commands.entity(entity).insert(SceneMembership(id));
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
