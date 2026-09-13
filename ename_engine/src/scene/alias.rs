//! Alias-addressed convenience wrappers over the path-based [`open_scene`]/[`save_scene`]
//! primitives. This is where `ename_asset_alias`/`ename_asset_content` are used -- the
//! primitives themselves never resolve an alias.

use bevy::prelude::*;
use ename_asset_alias::{ALIAS_EXTENSION, ALIAS_SOURCE, AliasFile, AliasOrigin};
use ename_asset_content::ContentIndex;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::{SaveSceneError, SceneFormats, SceneId, save_scene};

/// Opens the scene addressed by `alias`, through the sole registered
/// [`SceneFormat`](super::SceneFormat) (see the design note on this in the plan: alias resolution
/// happens lazily inside the asset pipeline, never synchronously here, so this cannot dispatch by
/// a resolved path's extension the way [`open_scene`](super::open_scene) does). The load itself
/// goes through `alias://`, preserving override transparency.
pub fn open_scene_by_alias(
    commands: &mut Commands,
    asset_server: &AssetServer,
    formats: &SceneFormats,
    alias: &str,
) -> Entity {
    let format = formats
        .default_format()
        .unwrap_or_else(|| panic!("no SceneFormat registered"));
    format.spawn_root(commands, asset_server, &format!("{ALIAS_SOURCE}://{alias}"))
}

/// Saves the scene identified by `id` under `alias`. If `alias` already resolves to a file, that
/// file is overwritten in whatever format it already used. If `alias` has no file yet, one is
/// created under a conventional path (`basegame/scenes/<alias, `::` replaced with `/`>.<ext>`)
/// and a matching `.alias` sidecar is written alongside it, so a scan of the asset tree resolves
/// `alias` to the new file from then on.
pub fn save_scene_by_alias(
    world: &mut World,
    asset_root: &Path,
    alias: &str,
    id: SceneId,
) -> Result<(), SaveSceneError> {
    let existing = world
        .resource::<ContentIndex>()
        .resolve(alias)
        .map(Path::to_path_buf);

    let (relative_path, is_new) = match existing {
        Some(path) => (path, false),
        None => {
            let extension = world
                .resource::<SceneFormats>()
                .default_format()
                .map(|format| format.extension().to_owned())
                .ok_or_else(|| SaveSceneError::UnknownFormat(alias.to_owned()))?;
            (default_scene_path_for_alias(alias, &extension), true)
        }
    };

    let absolute_path = asset_root.join(&relative_path);
    if let Some(parent) = absolute_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    save_scene(&absolute_path.to_string_lossy(), world, id)?;

    if is_new {
        write_alias_sidecar(&absolute_path, alias)?;
    }
    Ok(())
}

/// The conventional path a brand-new scene alias gets: `basegame/scenes/<alias>.<extension>`,
/// with `::` in the alias turned into path separators (`core::demo_scene` ->
/// `basegame/scenes/core/demo_scene.<extension>`).
fn default_scene_path_for_alias(alias: &str, extension: &str) -> PathBuf {
    PathBuf::from("basegame/scenes")
        .join(alias.replace("::", "/"))
        .with_extension(extension)
}

/// Writes a `.alias` sidecar next to `scene_path`, claiming `alias` for it. Mirrors the sidecar
/// convention `ename_asset_alias` already uses for every other asset type
/// (`airship.glb.alias` next to `airship.glb`).
fn write_alias_sidecar(scene_path: &Path, alias: &str) -> Result<(), SaveSceneError> {
    let sidecar_path = PathBuf::from(format!("{}.{ALIAS_EXTENSION}", scene_path.display()));
    let file = AliasFile {
        guid: Some(Uuid::new_v4()),
        alias: Some(alias.to_owned()),
        alias_origin: AliasOrigin::Derived,
    };
    let toml = toml::to_string_pretty(&file).map_err(std::io::Error::other)?;
    std::fs::write(sidecar_path, toml)?;
    Ok(())
}
