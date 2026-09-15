//! The top menu bar: `File` dropdown for stage operations (new/open/open-additive/save), and
//! `View` dropdown for camera/viewport toggles (freeze origin). The stage operations themselves
//! live in `ename_engine::stage`; this module only draws the buttons, resolves the currently
//! selected stage from the Hierarchy panel's selection, and asks the OS for a file when needed.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy_inspector_egui::bevy_inspector::hierarchy::SelectedEntities;
use ename_engine::bigspace::{CellCoord, FrozenOrigin, set_origin_frozen};
use ename_engine::stage::{
    AssetRoot, SaveStageError, StageFormats, StageId, new_stage, open_stage, open_stage_additive,
    save_stage, stage_of, write_stage_file,
};

/// Draws the top menu bar's `File` and `View` menus: `File` for stage operations
/// (new/open/open-additive/save), and `View` for the camera's Freeze Origin toggle.
pub(crate) fn ui(ui: &mut egui::Ui, world: &mut World, selected: &mut SelectedEntities) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui.button("Open New Stage").clicked() {
                ui.close();
                let root = new_stage(world);
                selected.select_replace(root);
            }
            if ui.button("Open Stage").clicked() {
                ui.close();
                if let Some(path) = pick_stage_to_open(world) {
                    open_stage(world, &path);
                }
            }
            if ui.button("Open Stage Additive").clicked() {
                ui.close();
                if let Some(path) = pick_stage_to_open(world) {
                    open_stage_additive(world, &path);
                }
            }

            let current = stage_of(world, selected.as_slice());
            ui.add_enabled_ui(current.is_some(), |ui| {
                if ui.button("Save Stage").clicked()
                    && let Some(id) = current
                {
                    ui.close();
                    save_current_stage(world, id);
                }
            });
        });
        ui.menu_button("View", |ui| {
            let Ok(camera) = world
                .query_filtered::<Entity, With<crate::camera::EditorCamera>>()
                .single(world)
            else {
                return;
            };
            let frozen = world.get::<FrozenOrigin>(camera).is_some();
            let has_grid = world.get::<CellCoord>(camera).is_some();

            ui.add_enabled_ui(has_grid, |ui| {
                let mut checked = frozen;
                if ui
                    .checkbox(&mut checked, "Freeze Camera Origin")
                    .on_hover_text(
                        "Keep flying without recentering the world around the camera. \
                         Disabled with no Grid loaded -- there is nothing to freeze.",
                    )
                    .changed()
                {
                    set_origin_frozen(world, camera, checked);
                    ui.close();
                }
            });
        });
    });
}

/// Saves `id` back to its recorded source, or -- for a stage that has never been saved -- asks
/// the user where to save it and writes it there.
fn save_current_stage(world: &mut World, id: StageId) {
    match save_stage(world, id) {
        Ok(()) => {}
        Err(SaveStageError::NoSource) => {
            let Some(path) = pick_stage_save_path(world) else {
                return;
            };
            if let Err(err) = write_stage_file(&path.to_string_lossy(), world, id) {
                error!("failed to save stage: {err}");
            }
        }
        Err(err) => error!("failed to save stage: {err}"),
    }
}

/// Opens a native "pick a file" dialog rooted at the asset directory, filtered to the registered
/// stage format's extension, and returns the picked file as an asset-relative path -- or `None`,
/// logging why, if it can't be resolved to one or doesn't match a registered `StageFormat`.
/// Checking the format here, before returning, is what keeps a bad pick from ever reaching
/// `open_stage`/`open_stage_additive`, which would otherwise unload the current stage and then
/// panic on the mismatch.
fn pick_stage_to_open(world: &World) -> Option<String> {
    let formats = world.resource::<StageFormats>();
    let extension = formats.default_format()?.extension().to_owned();
    let filter_extension = extension.rsplit('.').next().unwrap_or(&extension);

    let dialog = rfd::FileDialog::new()
        .add_filter("Stage", &[filter_extension])
        .set_directory(canonical_or_self(&world.resource::<AssetRoot>().0));
    let picked = dialog.pick_file()?;
    let relative = asset_relative_path(world, &picked)?;

    if world
        .resource::<StageFormats>()
        .for_path(&relative)
        .is_none()
    {
        warn!("{relative:?} is not a recognized stage file (expected a .{extension} file)");
        return None;
    }
    Some(relative)
}

/// Opens a native "save a file" dialog the same way, and returns the absolute path the user
/// picked -- `write_stage_file` needs a real filesystem destination, and derives the stage's
/// asset-relative `StageSource` from it itself. Rejects (rather than silently rewrites) a picked
/// name that doesn't match the registered format, so the OS's own overwrite confirmation always
/// applies to the exact file this code goes on to write.
fn pick_stage_save_path(world: &World) -> Option<PathBuf> {
    let formats = world.resource::<StageFormats>();
    let extension = formats.default_format()?.extension().to_owned();
    let filter_extension = extension.rsplit('.').next().unwrap_or(&extension);

    let dialog = rfd::FileDialog::new()
        .add_filter("Stage", &[filter_extension])
        .set_directory(canonical_or_self(&world.resource::<AssetRoot>().0))
        .set_file_name(format!("new_stage.{extension}"));
    let picked = dialog.save_file()?;

    if world
        .resource::<StageFormats>()
        .for_path(&picked.to_string_lossy())
        .is_none()
    {
        warn!(
            "{picked:?} is not a recognized stage file (expected a .{extension} file); not saving there"
        );
        return None;
    }
    Some(picked)
}

/// Converts an absolute path, as a native file dialog returns, into the asset-relative path
/// `AssetServer` expects. Canonicalizes both sides first, so a symlinked `assets/` directory
/// (this project's own dev setup, per `docs/design.md`) still resolves correctly; a save target
/// that doesn't exist yet canonicalizes its parent instead and reattaches the file name. Returns
/// `None` and logs a warning if `absolute` falls outside the asset root.
fn asset_relative_path(world: &World, absolute: &Path) -> Option<String> {
    let root = canonical_or_self(&world.resource::<AssetRoot>().0);
    let absolute = if absolute.exists() {
        canonical_or_self(absolute)
    } else {
        let file_name = absolute.file_name()?;
        canonical_or_self(absolute.parent()?).join(file_name)
    };
    match absolute.strip_prefix(&root) {
        Ok(relative) => Some(relative.to_string_lossy().replace('\\', "/")),
        Err(_) => {
            warn!(
                "{absolute:?} is outside the asset root {root:?}; stages must live under assets/"
            );
            None
        }
    }
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}
