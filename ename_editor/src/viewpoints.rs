//! `viewpoint.list`, `viewpoint.goto` and `viewpoint.save`: named camera poses in the project's
//! viewpoints file (`ename_engine::camera::viewpoint`), read on every call.
//!
//! The file path is development configuration the editor binary hands over with
//! `EditorPlugins::with_viewpoints_file`; without one the commands fail `no_viewpoints_file`.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use ename_engine::{
    camera::{
        ground::GroundGuardMode,
        viewpoint::{self, Viewpoint, apply_facets, capture_facets, facets_as_json},
    },
    command::{Call, CommandAppExt, CommandError, CommandSpec, JobHandle},
    wait::SettleParams,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::camera_commands::{
    Aim, CameraChoice, PlaceOptions, PoseReport, PoseRequest, ReportExtras, check_placement,
    find_camera, read_pose, round_vec, round2, start_placement,
};

/// Registers the viewpoint commands over the configured file.
pub(crate) struct ViewpointsPlugin {
    pub file: Option<PathBuf>,
}

impl Plugin for ViewpointsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ViewpointsFile(self.file.clone()))
            .register_command(
                CommandSpec::new(
                    "viewpoint.list",
                    "The named camera viewpoints in the project's viewpoints file, one line each.",
                )
                .read_only(),
                list,
            )
            .register_job_command(
                CommandSpec::new(
                    "viewpoint.goto",
                    "Move a camera to a named viewpoint and apply its facets, as camera.look_at \
                     places it.",
                ),
                goto,
            )
            .register_command(
                CommandSpec::new(
                    "viewpoint.save",
                    "Save a camera's pose, and the named facets, as a named viewpoint in the \
                     project's viewpoints file.",
                ),
                save,
            );
    }
}

/// Where the viewpoints file is, if the binary configured one.
#[derive(Resource, Clone, Debug)]
struct ViewpointsFile(Option<PathBuf>);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GotoParams {
    /// The viewpoint's name, as `viewpoint.list` shows it.
    name: String,
    /// Which camera to place. Defaults to the one the shown view renders.
    camera: Option<CameraChoice>,
    /// Apply the viewpoint's facets. Defaults to true.
    facets: Option<bool>,
    /// As on `camera.look_at`: "none", "colliders", "meshes" or "both"; defaults to the game's
    /// setting.
    ground: Option<GroundGuardMode>,
    /// As on `camera.look_at`.
    min_clearance: Option<f64>,
    /// As on `camera.look_at`.
    settle: Option<SettleParams>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveParams {
    /// ASCII letters, digits, space, `.`, `_` and `-`, not starting with `.`. Case-sensitive.
    name: String,
    /// What the viewpoint shows; the file keeps no comments, so this is where to say it.
    #[serde(default)]
    note: String,
    /// Which camera's pose to save. Defaults to the one the shown view renders.
    camera: Option<CameraChoice>,
    /// Facets to capture with the pose, by name. None by default.
    #[serde(default)]
    include: Vec<String>,
    /// Overwrite a viewpoint of the same name, keeping its place in the file.
    #[serde(default)]
    replace: bool,
}

#[derive(Serialize, JsonSchema)]
struct ListResult {
    file: String,
    version: u32,
    viewpoints: Vec<ListEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
struct ListEntry {
    name: String,
    note: String,
    position: [f64; 3],
    yaw: f64,
    pitch: f64,
    /// The facets the viewpoint carries.
    facets: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
struct SaveResult {
    file: String,
    name: String,
    note: String,
    position: [f64; 3],
    yaw: f64,
    pitch: f64,
    /// The captured facets, as JSON.
    facets: Map<String, Value>,
}

/// The configured viewpoints file, or `no_viewpoints_file`.
pub(crate) fn viewpoints_file(world: &World) -> Result<PathBuf, CommandError> {
    world
        .get_resource::<ViewpointsFile>()
        .and_then(|file| file.0.clone())
        .ok_or_else(|| {
            CommandError::failed(
                "no_viewpoints_file",
                "this build configured no viewpoints file",
            )
            .with_hint("The editor binary sets one with `EditorPlugins::with_viewpoints_file`.")
        })
}

fn list(
    In(_): In<Call<ename_engine::command::NoParams>>,
    world: &mut World,
) -> Result<ListResult, CommandError> {
    let path = viewpoints_file(world)?;
    let (file, notes) = match viewpoint::load(&path)? {
        Some(file) => (file, Vec::new()),
        None => (
            viewpoint::ViewpointFile::default(),
            vec!["the file does not exist yet; `viewpoint.save` creates it".to_string()],
        ),
    };
    Ok(ListResult {
        file: path.display().to_string(),
        version: file.version,
        viewpoints: file
            .viewpoints
            .into_iter()
            .map(|v| ListEntry {
                facets: v.facets.keys().cloned().collect(),
                name: v.name,
                note: v.note,
                position: v.position,
                yaw: v.yaw,
                pitch: v.pitch,
            })
            .collect(),
        notes,
    })
}

fn goto(
    In(call): In<Call<GotoParams>>,
    world: &mut World,
) -> Result<JobHandle<PoseReport>, CommandError> {
    crate::capture_sequence::check_not_busy(world)?;
    let path = viewpoints_file(world)?;
    let params = &call.params;
    let found = find_viewpoint(&path, &params.name)?;
    let options = PlaceOptions::new(
        world,
        params.ground,
        params.min_clearance,
        params.settle.clone(),
    )?;
    apply_viewpoint(
        world,
        &call,
        params.camera,
        &found,
        params.facets.unwrap_or(true),
        options,
    )
}

/// Applies the viewpoint's facets (all checked before any is applied), then places the camera
/// as `camera.look_at` does. For any command that goes to a viewpoint.
pub(crate) fn apply_viewpoint<P>(
    world: &mut World,
    call: &Call<P>,
    camera: Option<CameraChoice>,
    viewpoint: &Viewpoint,
    with_facets: bool,
    options: PlaceOptions,
) -> Result<JobHandle<PoseReport>, CommandError> {
    let pose = viewpoint.pose();
    let request = PoseRequest {
        position: Some(pose.position),
        aim: Aim::Angles {
            yaw: Some(pose.yaw),
            pitch: Some(pose.pitch),
        },
        advance: 0.0,
    };
    // Everything that can refuse runs before a facet changes the world, so a refused goto
    // leaves nothing half applied. The pose is finite: loading refuses one that is not.
    check_placement(world, camera, &options)?;
    let facets_applied = if with_facets {
        apply_facets(world, &viewpoint.facets)?
    } else {
        Map::new()
    };
    start_placement(
        world,
        call,
        camera,
        &request,
        options,
        ReportExtras {
            viewpoint: Some(viewpoint.name.clone()),
            facets_applied: Some(facets_applied),
        },
    )
}

fn find_viewpoint(path: &Path, name: &str) -> Result<Viewpoint, CommandError> {
    let file = viewpoint::load(path)?.unwrap_or_default();
    let names: Vec<String> = file.viewpoints.iter().map(|v| v.name.clone()).collect();
    file.viewpoints
        .into_iter()
        .find(|v| v.name == name.trim())
        .ok_or_else(|| {
            let known = if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            };
            CommandError::failed(
                "unknown_viewpoint",
                format!(
                    "{} has no viewpoint `{name}`; it has: {known}",
                    path.display()
                ),
            )
        })
}

fn save(In(call): In<Call<SaveParams>>, world: &mut World) -> Result<SaveResult, CommandError> {
    let path = viewpoints_file(world)?;
    let params = call.params;
    let name = viewpoint::validate_name(&params.name)?;
    let choice = params.camera.unwrap_or_else(|| CameraChoice::shown(world));
    let camera = find_camera(world, choice)?;
    let (position, rotation) = read_pose(world, camera)
        .ok_or_else(|| CommandError::failed("not_on_grid", "the camera has no world position"))?;
    let pose = ename_engine::camera::pose::CameraPose::from_rotation(position, rotation);
    let facets = capture_facets(world, &params.include)?;
    let facets_json = facets_as_json(world, &facets)?;
    let saved = Viewpoint {
        name: name.clone(),
        note: params.note.trim().to_string(),
        position: round_vec(pose.position),
        yaw: round2(pose.yaw),
        pitch: round2(pose.pitch),
        facets,
    };
    let result = SaveResult {
        file: path.display().to_string(),
        name,
        note: saved.note.clone(),
        position: saved.position,
        yaw: saved.yaw,
        pitch: saved.pitch,
        facets: facets_json,
    };
    viewpoint::save(&path, saved, params.replace)?;
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use ename_engine::{camera::viewpoint::ViewpointFacetsAppExt, command::Reply};
    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::camera_commands::tests::{base_app, call, editor_camera, finish, pose, run};

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct Fog {
        density: f64,
    }

    #[derive(Resource, Default)]
    struct FogState(f64);

    fn capture_fog(world: &mut World) -> Result<Fog, CommandError> {
        Ok(Fog {
            density: world.resource::<FogState>().0,
        })
    }

    fn apply_fog(world: &mut World, fog: Fog) -> Result<(), CommandError> {
        world.resource_mut::<FogState>().0 = fog.density;
        Ok(())
    }

    fn app(dir: &TempDir) -> App {
        let mut app = base_app();
        app.add_plugins(ViewpointsPlugin {
            file: Some(dir.path().join("viewpoints.ron")),
        })
        .init_resource::<FogState>()
        .register_viewpoint_facet("fog", capture_fog, apply_fog);
        finish(app)
    }

    fn done(app: &mut App, name: &str, params: Value) -> Value {
        match call(app, name, params).unwrap() {
            Reply::Done(value) => value,
            Reply::Job(job) => panic!("a job: {job}"),
        }
    }

    fn place(app: &mut App, params: Value) {
        let Reply::Job(job) = call(app, "camera.look_at", params).unwrap() else {
            panic!("a job");
        };
        assert_eq!(run(app, &job, 10).0.state, "succeeded");
    }

    #[test]
    fn goto_applies_a_facet_then_the_pose() {
        let dir = tempdir().unwrap();
        let mut app = app(&dir);
        place(
            &mut app,
            json!({ "position": [10.0, 20.0, 30.0], "yaw": -60.0, "pitch": 5.0 }),
        );
        app.world_mut().resource_mut::<FogState>().0 = 0.25;
        let saved = done(
            &mut app,
            "viewpoint.save",
            json!({ "name": "foggy", "include": ["fog"] }),
        );
        assert_eq!(saved["facets"], json!({ "fog": { "density": 0.25 } }));

        place(
            &mut app,
            json!({ "position": [0.0, 5.0, 0.0], "yaw": 0.0, "pitch": 0.0 }),
        );
        app.world_mut().resource_mut::<FogState>().0 = 0.0;
        let Reply::Job(job) = call(&mut app, "viewpoint.goto", json!({ "name": "foggy" })).unwrap()
        else {
            panic!("a job");
        };
        let (status, _) = run(&mut app, &job, 10);
        let result = status.result.unwrap();
        assert_eq!(result["viewpoint"], "foggy");
        assert_eq!(
            result["facets_applied"],
            json!({ "fog": { "density": 0.25 } })
        );
        assert_eq!(result["position"], json!([10.0, 20.0, 30.0]));
        assert_eq!(app.world().resource::<FogState>().0, 0.25);
        let camera = editor_camera(&mut app);
        let (_, yaw, pitch) = pose(&app, camera);
        assert!((yaw + 60.0).abs() < 0.01 && (pitch - 5.0).abs() < 0.01);

        let err = call(&mut app, "viewpoint.goto", json!({ "name": "nope" })).unwrap_err();
        assert_eq!(err.kind, "unknown_viewpoint");
        assert!(err.message.contains("foggy"), "{err}");
    }

    #[test]
    fn an_unknown_facet_applies_nothing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("viewpoints.ron");
        std::fs::write(
            &path,
            "(version: 1, viewpoints: [(name: \"odd\", position: (9.0, 9.0, 9.0), yaw: 0.0, \
             pitch: 0.0, facets: { \"fog\": (density: 0.5), \"moon\": (phase: 1) })])",
        )
        .unwrap();
        let mut app = app(&dir);
        place(&mut app, json!({ "position": [1.0, 1.0, 1.0] }));
        let err = call(&mut app, "viewpoint.goto", json!({ "name": "odd" })).unwrap_err();
        assert_eq!(err.kind, "unknown_facet");
        assert_eq!(app.world().resource::<FogState>().0, 0.0);
        let camera = editor_camera(&mut app);
        let (position, _, _) = pose(&app, camera);
        assert!(
            (position - bevy::math::DVec3::ONE).length() < 1e-4,
            "{position}"
        );
    }

    #[test]
    fn a_refused_goto_applies_no_facet_and_moves_no_camera() {
        let dir = tempdir().unwrap();
        let mut app = app(&dir);
        place(&mut app, json!({ "position": [10.0, 20.0, 30.0] }));
        app.world_mut().resource_mut::<FogState>().0 = 0.25;
        done(
            &mut app,
            "viewpoint.save",
            json!({ "name": "foggy", "include": ["fog"] }),
        );
        place(&mut app, json!({ "position": [1.0, 1.0, 1.0] }));
        app.world_mut().resource_mut::<FogState>().0 = 0.0;
        for (params, kind) in [
            (
                json!({ "name": "foggy", "settle": { "stable_frames": 0 } }),
                "invalid_params",
            ),
            (
                json!({ "name": "foggy", "settle": { "probes": ["moon"] } }),
                "unknown_probe",
            ),
            (
                json!({ "name": "foggy", "camera": "game_camera" }),
                "no_game_camera",
            ),
        ] {
            let err = call(&mut app, "viewpoint.goto", params).unwrap_err();
            assert_eq!(err.kind, kind);
            assert_eq!(app.world().resource::<FogState>().0, 0.0, "{kind}");
            let camera = editor_camera(&mut app);
            let (position, _, _) = pose(&app, camera);
            assert!(
                (position - bevy::math::DVec3::ONE).length() < 1e-4,
                "{kind}"
            );
        }
    }
}
