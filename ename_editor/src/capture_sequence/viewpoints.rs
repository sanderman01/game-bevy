//! `capture.viewpoints`: one PNG per named viewpoint, in one directory with a manifest.

use std::{collections::BTreeSet, path::PathBuf};

use bevy::prelude::*;
use ename_engine::{
    camera::{ground::GroundGuardMode, viewpoint},
    command::{Call, CommandAppExt, CommandError, CommandSpec, JobHandle},
    wait::SettleParams,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    CommonParams, Output, SequencePlan, SequenceResult, SequenceSource, Step, file_name_for,
    start_sequence,
};
use crate::viewpoints::viewpoints_file;

pub(super) fn register(app: &mut App) {
    app.register_job_command(
        CommandSpec::new(
            "capture.viewpoints",
            "Capture named viewpoints into one directory: per viewpoint, place the camera, settle \
             if asked and write <name>.png, plus a manifest.json with each frame's pose and size.",
        ),
        capture_viewpoints,
    );
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewpointsParams {
    /// Viewpoint names, as `viewpoint.list` shows them, in capture order.
    viewpoints: Vec<String>,
    /// The output directory. Relative paths resolve against the caller's working directory. Must
    /// be absent or empty, unless `replace`.
    dir: PathBuf,
    /// The camera to move and capture: "editor_camera" (Scene View) or "game_camera" (Game
    /// View). The view is shown for the run and switched back afterwards.
    source: SequenceSource,
    /// Settle before each capture, as `wait.settled` does; `{}` uses its defaults. A viewpoint
    /// that does not settle in time is not captured and is listed under `failed`. Left out, each
    /// capture is the first frame rendered with its pose.
    #[serde(default)]
    settle: Option<SettleParams>,
    /// Pause virtual time for the run, so time-driven shading does not change between
    /// viewpoints. Defaults to true.
    #[serde(default)]
    freeze_time: Option<bool>,
    /// Put the camera back where it was afterwards. Defaults to true.
    #[serde(default)]
    restore: Option<bool>,
    /// Keep the editor's debug overlays, transform gizmo and pick marker in the captures.
    /// Defaults to false.
    #[serde(default)]
    overlays: Option<bool>,
    /// Apply each viewpoint's facets before capturing it. Defaults to true. Facets are not put
    /// back afterwards.
    #[serde(default)]
    facets: Option<bool>,
    /// Which ground keeps the camera clear of it, as on `camera.look_at`. Defaults to the game's
    /// setting.
    #[serde(default)]
    ground: Option<GroundGuardMode>,
    /// Reuse `dir`: delete the files its old manifest.json lists, and the manifest, first.
    #[serde(default)]
    replace: bool,
}

fn capture_viewpoints(
    In(call): In<Call<ViewpointsParams>>,
    world: &mut World,
) -> Result<JobHandle<SequenceResult>, CommandError> {
    let params = &call.params;
    if params.viewpoints.is_empty() {
        return Err(CommandError::invalid(
            "invalid_params",
            "name at least one viewpoint",
        ));
    }
    let mut seen = BTreeSet::new();
    if let Some(twice) = params
        .viewpoints
        .iter()
        .find(|name| !seen.insert(name.trim()))
    {
        return Err(CommandError::invalid(
            "duplicate_viewpoint",
            format!("viewpoint `{twice}` is listed twice"),
        ));
    }
    let path = viewpoints_file(world)?;
    let file = viewpoint::load(&path)?.unwrap_or_default();
    let unknown: Vec<&str> = params
        .viewpoints
        .iter()
        .map(|name| name.trim())
        .filter(|name| !file.viewpoints.iter().any(|v| v.name == *name))
        .collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = file.viewpoints.iter().map(|v| v.name.as_str()).collect();
        return Err(CommandError::failed(
            "unknown_viewpoint",
            format!(
                "{} has no viewpoint {}; it has: {}",
                path.display(),
                unknown
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ),
        ));
    }
    let with_facets = params.facets.unwrap_or(true);
    let mut taken = Vec::new();
    let steps = params
        .viewpoints
        .iter()
        .map(|name| {
            let found = file
                .viewpoints
                .iter()
                .find(|v| v.name == name.trim())
                .expect("checked above");
            Step {
                pose: found.pose(),
                viewpoint: Some(found.name.clone()),
                file: Some(file_name_for(&found.name, &mut taken)),
                facets: with_facets.then(|| found.clone()),
            }
        })
        .collect();
    let dir = call.resolve_path(&params.dir)?;
    let ground = params.ground.unwrap_or_else(|| {
        world
            .get_resource::<ename_engine::camera::ground::CameraGroundGuard>()
            .copied()
            .unwrap_or_default()
            .ground
    });
    let plan = SequencePlan {
        common: CommonParams {
            source: params.source,
            settle: params.settle.clone(),
            freeze_time: params.freeze_time.unwrap_or(true),
            restore: params.restore.unwrap_or(true),
            overlays: params.overlays.unwrap_or(false),
        },
        steps,
        output: Output::Stills { dir },
        ground,
        replace: params.replace,
    };
    start_sequence(world, &call, plan)
}
