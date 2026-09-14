//! The stage primitives: `spawn_stage_root` starts a load (by plain path or `alias://` URL) and
//! `write_stage_file` extracts a tagged subset of the `World` back to bytes. Both are internal --
//! `ename_engine::stage::{new_stage, open_stage, open_stage_additive, save_stage}` in
//! `workflow.rs` are the public entry points everything outside this module should use.
//!
//! Also owns the `WorldInstanceReady` observer that stamps stage identity onto whatever a
//! `StageFormat` just finished spawning. Two cases share one handler: opening a stage directly
//! (the container carries `SourcePath`, and its one child carries the file's own `StageId`), and
//! loading nested content under an already-open stage (e.g. a `WorldAssetRoot`-addressed glTF
//! model authored inside a stage file) -- there the container already carries `StageMember` from
//! having been tagged as a descendant of its parent stage, and that membership is what propagates
//! further down. See `scratch/scenes-spec.md`.

use bevy::{platform::collections::HashMap, prelude::*};
use ename_asset_alias::{ALIAS_SOURCE, ContentIndex};

use super::{AssetRoot, SourcePath, StageFormats, StageId, StageMember, StageSource};

/// Starts loading `path` (a plain asset path, or an `alias://`-prefixed URL) and spawns its
/// container entity. Returns the container immediately -- loading is async, so the content is not
/// there yet.
///
/// Alias URLs resolve lazily inside the asset pipeline, never synchronously here, so their
/// extension is unknown at this point and format dispatch falls back to the one format this
/// project ships (see [`StageFormats::default_format`]) instead of matching by suffix.
///
/// Forces a reload of `path` first: `AssetServer::load` returns a cached handle for a path that's
/// already loaded, which would silently ignore an on-disk edit made since the last time this
/// exact path was opened (e.g. via `open_stage_additive`'s reload). `AssetServer::reload` is a
/// no-op if `path` was never loaded before.
///
/// Panics if no format is registered for `path` -- a missing format is a startup wiring bug, not
/// a runtime condition to recover from. Callers taking a path from user input (the editor's file
/// picker) must validate it against a registered `StageFormat` themselves before calling this.
pub(super) fn spawn_stage_root(world: &mut World, path: &str) -> Entity {
    let asset_server = world.resource::<AssetServer>().clone();
    asset_server.reload(path.to_owned());
    world.resource_scope(|world, formats: Mut<StageFormats>| {
        let format = if path.starts_with(&format!("{ALIAS_SOURCE}://")) {
            formats.default_format()
        } else {
            formats.for_path(path)
        }
        .unwrap_or_else(|| panic!("no StageFormat registered for path {path:?}"));

        let mut commands = world.commands();
        let container = format.spawn_root(&mut commands, &asset_server, path);
        world.flush();
        container
    })
}

/// The filesystem directory `AssetPlugin` reads assets from when `AssetPlugin::file_path` is left
/// at its default (`"assets"`, true everywhere this project ships) -- `StagePlugin` uses this to
/// give [`AssetRoot`] a working default with no configuration.
pub(super) fn default_asset_root() -> std::path::PathBuf {
    bevy::asset::io::file::FileAssetReader::get_base_path().join("assets")
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

/// Normalizes `path` (as recorded in `SourcePath`) to a plain asset-relative address for
/// `StageSource`. An `alias://` URL resolves through `ContentIndex` to the concrete file it
/// currently points at, so two ways of opening the same file -- by alias, or by picking that same
/// file directly -- agree on one `StageSource` value. Falls back to the raw URL if there is no
/// `ContentIndex` (e.g. a headless test) or the alias does not resolve, rather than losing the
/// source entirely.
fn resolve_source_path(path: &str, content_index: Option<&ContentIndex>) -> String {
    let Some(alias) = path.strip_prefix(&format!("{ALIAS_SOURCE}://")) else {
        return path.to_owned();
    };
    content_index
        .and_then(|index| index.resolve(alias))
        .map(|resolved| resolved.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_owned())
}

/// Stamps `StageMember` on everything a `WorldInstanceReady` event just finished spawning.
/// See the module doc for the two cases this handles.
// Bevy systems take one parameter per `Query`/`Res`/etc.; splitting this up would only hide the
// count behind a helper struct, not reduce it.
#[allow(clippy::too_many_arguments)]
pub(super) fn tag_stage_membership_on_ready(
    trigger: On<bevy::world_serialization::WorldInstanceReady>,
    children_of: Query<&Children>,
    child_of: Query<(Entity, &ChildOf)>,
    ids: Query<&StageId>,
    membership: Query<&StageMember>,
    source_paths: Query<&SourcePath>,
    content_index: Option<Res<ContentIndex>>,
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
        let name = stage_name_from_path(path);
        let source = resolve_source_path(path, content_index.as_deref());
        commands
            .entity(*root)
            // `Name`: every entity-addressing tool in this codebase (the editor's Hierarchy
            // panel, ename_mcp) keys off it, so the stage root must carry one to be discoverable
            // by the stage's name at all. This overwrites whatever `Name` the root already had
            // (e.g. a reused content entity like big_space's `Grid`) -- nothing in this codebase
            // depends on such a pre-existing name surviving a load.
            .insert(Name::new(name))
            // Where `save_stage` and `open_stage_additive` find this stage again -- see
            // identity.rs's doc on `StageSource`.
            .insert(StageSource(source))
            // A loaded stage root must be top-level; big_space validates floating origins
            // against the ultimate hierarchy ancestor, not this transient load container.
            .remove::<ChildOf>();
        id
    } else if let Ok(&StageMember(id)) = membership.get(container) {
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
    commands.entity(entity).insert(StageMember(id));
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

/// A [`save_stage`](super::save_stage) or [`write_stage_file`] call failed.
#[derive(Debug, thiserror::Error)]
pub enum SaveStageError {
    #[error("stage has no recorded source to save back to -- save it to a path first")]
    NoSource,
    #[error("no StageFormat registered for {0:?}")]
    UnknownFormat(String),
    #[error(transparent)]
    Format(#[from] super::StageFormatError),
    #[error("failed to write stage file: {0}")]
    Io(#[from] std::io::Error),
}

/// Saves every entity tagged `StageMember(id)` to `path` -- a literal filesystem destination,
/// not an asset-relative address -- choosing a format by `path`'s extension. Records the
/// resolved root's new [`StageSource`], derived from `path` via [`AssetRoot`] when `path` falls
/// under it (the normal case for a save driven by [`save_stage`](super::save_stage) or the
/// editor's Save-As dialog), or `path` verbatim otherwise. `world` is mutated: if the tagged
/// entities have no single natural root (a lone top-level entity with no `ChildOf` into the
/// tagged set), a synthetic root is created and the orphans reparented under it, so the saved
/// file always has exactly one top-level entity -- the invariant [`spawn_stage_root`]'s
/// membership tagging depends on. See `scratch/scenes-spec.md`.
pub fn write_stage_file(path: &str, world: &mut World, id: StageId) -> Result<(), SaveStageError> {
    let root = world.resource_scope(
        |world, formats: Mut<StageFormats>| -> Result<Entity, SaveStageError> {
            let mut query = world.query::<(Entity, &StageMember)>();
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
                    let synthetic = world.spawn((id, StageMember(id))).id();
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
            Ok(root)
        },
    )?;

    let source = world
        .get_resource::<AssetRoot>()
        .and_then(|root| std::path::Path::new(path).strip_prefix(&root.0).ok())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_owned());
    if world
        .get::<StageSource>(root)
        .is_none_or(|existing| existing.0 != source)
    {
        world.entity_mut(root).insert(StageSource(source));
    }
    Ok(())
}
