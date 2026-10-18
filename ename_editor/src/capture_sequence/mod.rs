//! `capture.viewpoints` and `capture.fly_through`: a camera moved through a list of poses and
//! captured at each, as one job.
//!
//! Both commands build a [`Sequence`] on the job's work entity and leave the rest to one driver:
//! show the view the camera renders in, then per step place the pose, settle if asked, capture,
//! and finally write the manifest and put back what the sequence changed. Sequenced in the game
//! rather than by the caller, because each step needs frame-exact order: the pose is applied a
//! frame before the frame that is captured, and the next pose waits for the capture.
//!
//! # What a sequence changes, and puts back
//!
//! The shown view, virtual time (paused with `freeze_time`), the camera's pose (with `restore`),
//! a `CameraDriver` on the camera (disabled), and the editor overlays (hidden unless
//! `overlays`). One sequence runs at a time, and while it does `editor.set_view` and
//! `viewpoint.goto` are `busy` too. A placement by another command (`camera.look_at`) ends the
//! sequence `superseded`; the pose is then left to that command.
//!
//! # Timing
//!
//! Placements run in `Last`, so transforms propagate in the next frame's `PostUpdate` and the
//! capture is requested in that frame's `Last`, before `CaptureSystems::Request`. In video mode
//! the pose follows the recorder instead: `PreUpdate` applies the pose whose index is the number
//! of frames requested so far, so pose `i` is the `i`-th recorded frame at any frame rate.
//!
//! # Cancelling
//!
//! The job uses `OnCancel::Notify`. A cancelled sequence waits for a capture in flight, stops a
//! recording (its partial file is kept), writes the manifest of what landed, and restores.

mod fly;
mod viewpoints;

use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::Instant,
};

use bevy::{
    diagnostic::FrameCount,
    gizmos::config::GizmoConfigStore,
    math::{DQuat, DVec3},
    prelude::*,
};
use ename_engine::{
    camera::{
        CameraDriver,
        ground::GroundGuardMode,
        pose::{CameraPose, yaw_pitch},
        viewpoint::{Viewpoint, apply_facets},
    },
    capture::{CaptureError, CaptureSource, CaptureSystems},
    command::{CommandError, JobCancelRequested, JobHandle, JobId, Jobs, RunId},
    debug_overlay::DebugOverlays,
    perf::PerfOverlay,
    screenshot::{SavedScreenshot, ScreenshotError, ScreenshotFinished, ScreenshotRequest},
    time::TimeControl,
    video::{
        SavedVideo, VideoError, VideoFinished, VideoProgress, VideoRequest, VideoStopRequested,
    },
    wait::{SettleGate, SettleParams, SettleTimedOut, Settled, WaitSystems},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    camera_commands::{
        Aim, CameraChoice, GROUND_WAIT, PlaceOptions, Placed, PoseRequest, find_camera,
        guard_placed, place_camera_pose, placement_generation, read_pose, resolve_camera,
        round_vec, round2,
    },
    panels::{ActiveViewport, UiState},
    selection::EditorGizmos,
};

/// Frames a sequence waits at most for its view's camera to render.
const SHOW_CAP: u32 = 30;
/// Most steps one call takes.
const MAX_STEPS: usize = 10_000;
/// Longest file name stem a viewpoint name becomes, in bytes (ASCII by then).
const MAX_STEM: usize = 200;

/// Registers both commands and the driver.
pub(crate) struct CaptureSequencePlugin;

impl Plugin for CaptureSequencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RunningSequence>()
            .add_systems(
                PreUpdate,
                follow_recordings.run_if(any_with_component::<Sequence>),
            )
            // After the settle gates opened this frame, and before readbacks are requested, so a
            // capture asked for in the frame its step settles starts that frame.
            .add_systems(
                Last,
                advance_sequences
                    .after(WaitSystems::Resolve)
                    .before(CaptureSystems::Request)
                    .run_if(any_with_component::<Sequence>),
            );
        viewpoints::register(app);
        fly::register(app);
    }
}

/// The one sequence running, if any.
#[derive(Resource, Default)]
pub(crate) struct RunningSequence(Option<JobId>);

/// Present while a sequence hides the editor overlays. The gizmo reads it.
#[derive(Resource)]
pub(crate) struct OverlaysHidden;

/// `busy` while a sequence runs, for every command that would change what it saved.
pub(crate) fn check_not_busy(world: &World) -> Result<(), CommandError> {
    let Some(job) = world
        .get_resource::<RunningSequence>()
        .and_then(|running| running.0.clone())
    else {
        return Ok(());
    };
    // A job that ended without the driver seeing it (its work entity despawned elsewhere) holds
    // nothing; it must not leave the editor busy for the rest of the run.
    let running = world
        .get_resource::<Jobs>()
        .and_then(|jobs| jobs.status(&job, ename_engine::command::CommandScope::Development))
        .is_some_and(|status| status.state == "running");
    if !running {
        return Ok(());
    }
    Err(CommandError::failed(
        "busy",
        format!("capture job `{job}` is moving the camera and switching views"),
    )
    .with_hint("Wait for it with job_wait, or end it early with job_cancel."))
}

/// The camera a sequence moves. A window has no pose, so it is not one.
#[derive(Deserialize, Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SequenceSource {
    /// The Scene View's free camera.
    EditorCamera,
    /// The stage's `MainCamera`, shown in the Game View.
    GameCamera,
}

impl SequenceSource {
    fn choice(self) -> CameraChoice {
        match self {
            Self::EditorCamera => CameraChoice::EditorCamera,
            Self::GameCamera => CameraChoice::GameCamera,
        }
    }

    fn view(self) -> ActiveViewport {
        match self {
            Self::EditorCamera => ActiveViewport::Scene,
            Self::GameCamera => ActiveViewport::Game,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::EditorCamera => "editor_camera",
            Self::GameCamera => "game_camera",
        }
    }
}

/// What both commands take besides their steps and output.
#[derive(Clone, Debug)]
pub(crate) struct CommonParams {
    pub source: SequenceSource,
    pub settle: Option<SettleParams>,
    pub freeze_time: bool,
    pub restore: bool,
    pub overlays: bool,
}

/// One pose to visit.
#[derive(Clone, Debug)]
pub(crate) struct Step {
    pub pose: CameraPose,
    /// The viewpoint the pose came from.
    pub viewpoint: Option<String>,
    /// The PNG to write, relative to the output directory. `None` in video mode.
    pub file: Option<String>,
    /// A viewpoint whose facets to apply before placing.
    pub facets: Option<Viewpoint>,
}

/// Where the captures go.
#[derive(Clone, Debug)]
pub(crate) enum Output {
    Stills {
        dir: PathBuf,
    },
    Video {
        path: PathBuf,
        frame_rate: NonZeroU32,
    },
}

impl Output {
    fn manifest_path(&self) -> PathBuf {
        match self {
            Self::Stills { dir } => dir.join("manifest.json"),
            Self::Video { path, .. } => {
                let mut name = path.as_os_str().to_owned();
                name.push(".manifest.json");
                PathBuf::from(name)
            }
        }
    }
}

/// A sequence ready to start, as a command builds it.
pub(crate) struct SequencePlan {
    pub common: CommonParams,
    pub steps: Vec<Step>,
    pub output: Output,
    /// The ground guard for every placement.
    pub ground: GroundGuardMode,
    /// Stills: delete the files the directory's old manifest lists first.
    pub replace: bool,
}

/// What a sequence finished with.
#[derive(Serialize, JsonSchema, Debug, Default)]
pub(crate) struct SequenceResult {
    /// The stills directory, absolute.
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<PathBuf>,
    /// The video file, absolute.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    manifest: PathBuf,
    /// The first frame's size in physical pixels; the manifest lists each frame's.
    width: Option<u32>,
    height: Option<u32>,
    /// A frame's size differs from the first's: the view was resized mid-run, so the frames
    /// are not comparable with each other.
    size_changed: bool,
    /// Frames captured or recorded.
    frames: u32,
    /// The stills written, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    shots: Vec<Shot>,
    /// Steps that were not captured, with why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failed: Vec<Failure>,
    /// Video only: frames lost on the way to the file; each shows as a jump in the motion.
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped_frames: Option<u32>,
    /// Video only: why the recording ended, such as "completed" or "source_resized".
    #[serde(skip_serializing_if = "Option::is_none")]
    ended: Option<&'static str>,
}

#[derive(Serialize, JsonSchema, Debug, Clone)]
struct Shot {
    #[serde(skip_serializing_if = "Option::is_none")]
    viewpoint: Option<String>,
    file: String,
}

#[derive(Serialize, JsonSchema, Debug, Clone)]
struct Failure {
    step: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewpoint: Option<String>,
    /// The error's kind, such as "not_settled".
    error: String,
    message: String,
    /// For "not_settled": what was still busy.
    #[serde(skip_serializing_if = "Option::is_none")]
    busy: Option<Value>,
}

/// `manifest.json`: authoritative for its directory, so files it does not list are not this
/// run's.
#[derive(Serialize, Deserialize, Debug)]
struct Manifest {
    run: String,
    source: String,
    width: Option<u32>,
    height: Option<u32>,
    /// Elapsed virtual seconds when the run started. Time-driven shading depends on it, so two
    /// runs with different values may differ for that reason alone.
    virtual_time: f64,
    frames: Vec<ManifestFrame>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ManifestFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viewpoint: Option<String>,
    position: [f64; 3],
    yaw: f64,
    pitch: f64,
    /// The frame counter when it was captured, or the pose's index in a video.
    frame: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    /// Why the step has no file, such as "not_settled".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// For "not_settled": what was still busy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    busy: Option<Value>,
}

/// What a sequence changed, to put back.
struct SavedState {
    view: ActiveViewport,
    pose: Option<(DVec3, DQuat)>,
    paused: Option<bool>,
    driver: Option<bool>,
    overlays: Option<SavedOverlays>,
}

struct SavedOverlays {
    debug: Option<DebugOverlays>,
    stats: Option<PerfOverlay>,
    pick_marker: Option<bool>,
}

#[derive(Debug)]
enum Phase {
    ShowView {
        since: u32,
        active_since: Option<u32>,
    },
    Place,
    WaitFrame {
        since: u32,
    },
    /// A ground probe has not loaded the ground below the pose yet.
    Ground {
        since: Instant,
    },
    Settling,
    Settled,
    Ready,
    Capturing,
    Captured(Result<SavedScreenshot, ScreenshotError>),
    Recording {
        request: Entity,
        applied: usize,
        stop_sent: bool,
    },
    Recorded(Result<SavedVideo, VideoError>),
}

/// Why a sequence ends early.
#[derive(Debug)]
enum End {
    Cancelled,
    Superseded,
    Failed(CommandError),
}

/// A sequence in progress, on its job's work entity.
#[derive(Component)]
pub(crate) struct Sequence {
    job: JobHandle<SequenceResult>,
    source: SequenceSource,
    camera: Entity,
    steps: Vec<Step>,
    output: Output,
    settle: Option<SettleParams>,
    options: PlaceOptions,
    restore: bool,
    phase: Phase,
    index: usize,
    /// The placement generation of this sequence's newest placement; 0 before the first.
    generation: u64,
    frames: Vec<ManifestFrame>,
    shots: Vec<Shot>,
    failed: Vec<Failure>,
    first_size: Option<UVec2>,
    size_changed: bool,
    virtual_time: f64,
    saved: SavedState,
    cancel_requested: bool,
    end: Option<End>,
    video: Option<SavedVideo>,
    /// The current step's placement, for the ground guard.
    placed: Option<Placed>,
    /// The manifest entry of the still in flight, taken when it was requested.
    pending: Option<ManifestFrame>,
}

impl Sequence {
    /// Lists the current step as not captured, in the result and the manifest, and moves on.
    fn fail_step(&mut self, error: CommandError, busy: Option<Value>) {
        let index = self.index;
        let step = &self.steps[index];
        self.frames.push(ManifestFrame {
            file: None,
            viewpoint: step.viewpoint.clone(),
            position: round_vec(step.pose.position),
            yaw: round2(step.pose.yaw),
            pitch: round2(step.pose.pitch),
            frame: 0,
            width: None,
            height: None,
            error: Some(error.kind.to_string()),
            busy: busy.clone(),
        });
        self.failed.push(Failure {
            step: index,
            viewpoint: step.viewpoint.clone(),
            error: error.kind.to_string(),
            message: error.message,
            busy,
        });
        self.index += 1;
        self.phase = Phase::Place;
    }
}

/// Checks a plan, changes nothing until it passes, then saves the state, starts the job and
/// begins showing the view.
pub(crate) fn start_sequence<P>(
    world: &mut World,
    call: &ename_engine::command::Call<P>,
    plan: SequencePlan,
) -> Result<JobHandle<SequenceResult>, CommandError> {
    check_not_busy(world)?;
    if plan.steps.is_empty() || plan.steps.len() > MAX_STEPS {
        return Err(CommandError::invalid(
            "invalid_params",
            format!(
                "a sequence has 1 to {MAX_STEPS} steps, not {}",
                plan.steps.len()
            ),
        ));
    }
    if let Some(settle) = &plan.common.settle {
        SettleGate::new(settle, world)?;
    }
    let source = plan.common.source;
    let choice = source.choice();
    let camera = find_camera(world, choice)?;
    let options = PlaceOptions {
        ground: plan.ground,
        min_clearance: PlaceOptions::new(world, Some(plan.ground), None, None)?.min_clearance,
        settle: None,
    };
    let dir_plan = match &plan.output {
        Output::Stills { dir } => {
            let files: Vec<String> = plan
                .steps
                .iter()
                .filter_map(|step| step.file.clone())
                .collect();
            Some(plan_dir(dir, plan.replace, &files)?)
        }
        Output::Video { .. } => None,
    };

    // From here on the world changes, so every failure puts it back.
    let saved = save_state(world, source, camera, &plan.common);
    let prepared = resolve_camera(world, choice)
        .and_then(|_| show_view(world, source))
        .and_then(|()| dir_plan.as_ref().map_or(Ok(()), apply_dir));
    if let Err(err) = prepared {
        restore_state(world, choice, camera, saved, false);
        return Err(err);
    }
    let work = world.spawn_empty().id();
    let job = match call.start_job(world, work, ename_engine::command::OnCancel::Notify) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            restore_state(world, choice, camera, saved, false);
            return Err(err);
        }
    };
    world.resource_mut::<RunningSequence>().0 = Some(job.id().clone());
    let virtual_time = world
        .get_resource::<Time<Virtual>>()
        .map_or(0.0, |time| round2(time.elapsed_secs_f64()));
    let now = frame(world);
    world
        .entity_mut(work)
        .insert(Sequence {
            job: job.clone(),
            source,
            camera,
            steps: plan.steps,
            output: plan.output,
            settle: plan.common.settle,
            options,
            restore: plan.common.restore,
            phase: Phase::ShowView {
                since: now,
                active_since: None,
            },
            index: 0,
            generation: 0,
            frames: Vec::new(),
            shots: Vec::new(),
            failed: Vec::new(),
            first_size: None,
            size_changed: false,
            virtual_time,
            saved,
            cancel_requested: false,
            end: None,
            video: None,
            placed: None,
            pending: None,
        })
        .observe(
            |cancel: On<JobCancelRequested>, mut sequences: Query<&mut Sequence>| {
                if let Ok(mut sequence) = sequences.get_mut(cancel.entity) {
                    sequence.cancel_requested = true;
                }
            },
        )
        .observe(|done: On<Settled>, mut sequences: Query<&mut Sequence>| {
            if let Ok(mut sequence) = sequences.get_mut(done.entity)
                && matches!(sequence.phase, Phase::Settling)
            {
                sequence.phase = Phase::Settled;
            }
        })
        .observe(
            |timeout: On<SettleTimedOut>, mut sequences: Query<&mut Sequence>| {
                let Ok(mut sequence) = sequences.get_mut(timeout.entity) else {
                    return;
                };
                if !matches!(sequence.phase, Phase::Settling) {
                    return;
                }
                let error = timeout.error();
                if matches!(sequence.output, Output::Video { .. }) {
                    sequence.end.get_or_insert(End::Failed(error));
                    sequence.phase = Phase::Place;
                    return;
                }
                sequence.fail_step(error, serde_json::to_value(&timeout.busy).ok());
            },
        );
    Ok(job)
}

/// Saves what the sequence will change, and changes it: time, the driver, the overlays. The view
/// is shown separately, once the job exists.
fn save_state(
    world: &mut World,
    source: SequenceSource,
    camera: Entity,
    common: &CommonParams,
) -> SavedState {
    let view = world
        .get_resource::<UiState>()
        .map_or(ActiveViewport::Scene, |ui| ui.active_viewport);
    let pose = read_pose(world, camera);
    let paused = if common.freeze_time {
        world.resource_scope(|world, mut time: Mut<Time<Virtual>>| {
            let was = time.is_paused();
            if let Some(mut control) = world.get_resource_mut::<TimeControl>() {
                control.pause(&mut time);
            } else {
                time.pause();
            }
            Some(was)
        })
    } else {
        None
    };
    let driver = if source == SequenceSource::GameCamera {
        world.get_mut::<CameraDriver>(camera).map(|mut driver| {
            let was = driver.enabled;
            driver.enabled = false;
            was
        })
    } else {
        None
    };
    let overlays = (!common.overlays).then(|| {
        let debug = world
            .get_resource_mut::<DebugOverlays>()
            .map(|mut overlays| {
                let was = *overlays;
                overlays.physics = false;
                overlays.grids = false;
                was
            });
        let stats = world.get_resource_mut::<PerfOverlay>().map(|mut overlay| {
            let was = *overlay;
            overlay.stats = false;
            was
        });
        let pick_marker = world
            .get_resource_mut::<GizmoConfigStore>()
            .map(|mut store| {
                let config = store.config_mut::<EditorGizmos>().0;
                std::mem::replace(&mut config.enabled, false)
            });
        world.insert_resource(OverlaysHidden);
        SavedOverlays {
            debug,
            stats,
            pick_marker,
        }
    });
    SavedState {
        view,
        pose,
        paused,
        driver,
        overlays,
    }
}

/// Puts back what [`save_state`] changed, and the view and pose when asked.
fn restore_state(
    world: &mut World,
    choice: CameraChoice,
    camera: Entity,
    saved: SavedState,
    pose: bool,
) {
    if pose
        && let Some((position, rotation)) = saved.pose
        && world.get_entity(camera).is_ok()
    {
        let request = PoseRequest {
            position: Some(position),
            aim: Aim::Rotation(rotation),
            advance: 0.0,
        };
        let options = PlaceOptions {
            ground: GroundGuardMode::None,
            min_clearance: 0.0,
            settle: None,
        };
        if let Err(err) = place_camera_pose(world, choice, &request, options) {
            warn!("capture sequence: could not restore the camera's pose: {err}");
        }
    }
    if let Some(enabled) = saved.driver
        && let Some(mut driver) = world.get_mut::<CameraDriver>(camera)
    {
        driver.enabled = enabled;
    }
    if let Some(overlays) = saved.overlays {
        if let Some(debug) = overlays.debug
            && let Some(mut current) = world.get_resource_mut::<DebugOverlays>()
        {
            *current = debug;
        }
        if let Some(stats) = overlays.stats
            && let Some(mut current) = world.get_resource_mut::<PerfOverlay>()
        {
            *current = stats;
        }
        if let Some(enabled) = overlays.pick_marker
            && let Some(mut store) = world.get_resource_mut::<GizmoConfigStore>()
        {
            store.config_mut::<EditorGizmos>().0.enabled = enabled;
        }
        world.remove_resource::<OverlaysHidden>();
    }
    if let Some(was_paused) = saved.paused {
        world.resource_scope(|world, mut time: Mut<Time<Virtual>>| {
            match (was_paused, world.get_resource_mut::<TimeControl>()) {
                (true, Some(mut control)) => control.pause(&mut time),
                (false, Some(mut control)) => control.resume(&mut time),
                (true, None) => time.pause(),
                (false, None) => time.unpause(),
            }
        });
    }
    if let Some(mut ui) = world.get_resource_mut::<UiState>()
        && ui.active_viewport != saved.view
    {
        let _ = ui.show_viewport(saved.view);
    }
}

/// Shows the view the source renders in. `view_unavailable` when its tab cannot be shown.
fn show_view(world: &mut World, source: SequenceSource) -> Result<(), CommandError> {
    let Some(mut ui) = world.get_resource_mut::<UiState>() else {
        return Ok(());
    };
    if ui.active_viewport == source.view() {
        return Ok(());
    }
    ui.show_viewport(source.view())
        .map_err(|message| CommandError::failed("view_unavailable", message))
}

/// What preparing an output directory will delete, checked before anything is.
struct DirPlan {
    dir: PathBuf,
    delete: Vec<PathBuf>,
}

/// `dir` must be absent or empty. With `replace`, it may hold an earlier run: the files its
/// `manifest.json` lists are deleted, and nothing else, so a file it does not list that this run
/// would write is `file_exists` rather than overwritten.
fn plan_dir(dir: &Path, replace: bool, files: &[String]) -> Result<DirPlan, CommandError> {
    let io = |err: std::io::Error| {
        CommandError::failed("write", format!("cannot use `{}`: {err}", dir.display()))
    };
    let mut plan = DirPlan {
        dir: dir.to_path_buf(),
        delete: Vec::new(),
    };
    if !dir.is_dir() {
        return Ok(plan);
    }
    let existing: Vec<String> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    if existing.is_empty() {
        return Ok(plan);
    }
    let not_empty = |why: &str| {
        CommandError::failed(
            "dir_not_empty",
            format!("`{}` is not empty{why}", dir.display()),
        )
    };
    if !replace {
        return Err(not_empty("").with_hint(
            "Use a new directory, or pass replace: true to delete the files its manifest.json \
             lists first.",
        ));
    }
    let manifest_path = dir.join("manifest.json");
    let manifest: Manifest = std::fs::read(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_slice(&text).ok())
        .ok_or_else(|| {
            not_empty(
                " and has no capture manifest.json, so replace cannot tell which files are a \
                       run's",
            )
        })?;
    let mut listed: Vec<String> = manifest
        .frames
        .iter()
        .filter_map(|frame| frame.file.clone())
        // Only plain names: a manifest never lists anything outside its directory.
        .filter(|file| {
            Path::new(file)
                .file_name()
                .is_some_and(|name| name == file.as_str())
        })
        .collect();
    listed.push("manifest.json".to_owned());
    let owned = |name: &str| listed.iter().any(|file| file.eq_ignore_ascii_case(name));
    if let Some(clash) = files
        .iter()
        .find(|file| !owned(file) && existing.iter().any(|name| name.eq_ignore_ascii_case(file)))
    {
        return Err(CommandError::failed(
            "file_exists",
            format!(
                "`{}` exists and the old manifest does not list it, so this run will not \
                 overwrite it",
                dir.join(clash).display()
            ),
        ));
    }
    plan.delete = existing
        .iter()
        .filter(|name| owned(name))
        .map(|name| dir.join(name))
        .collect();
    Ok(plan)
}

/// Deletes what [`plan_dir`] found and creates the directory.
fn apply_dir(plan: &DirPlan) -> Result<(), CommandError> {
    for path in &plan.delete {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CommandError::failed(
                    "write",
                    format!("cannot remove `{}`: {err}", path.display()),
                ));
            }
        }
    }
    std::fs::create_dir_all(&plan.dir).map_err(|err| {
        CommandError::failed(
            "write",
            format!("cannot create `{}`: {err}", plan.dir.display()),
        )
    })
}

/// Moves each recording's camera to the pose of the frame the recorder asks for next.
fn follow_recordings(world: &mut World) {
    let works: Vec<Entity> = world
        .query_filtered::<Entity, With<Sequence>>()
        .iter(world)
        .collect();
    for work in works {
        let Some(mut sequence) = world.entity_mut(work).take::<Sequence>() else {
            continue;
        };
        if let Phase::Recording {
            request, applied, ..
        } = sequence.phase
            && sequence.end.is_none()
            && placement_generation(world, sequence.camera) == sequence.generation
        {
            let requested = world
                .get::<VideoProgress>(request)
                .map_or(0, |progress| progress.requested as usize);
            let next = requested.min(sequence.steps.len() - 1);
            if next != applied {
                match place(world, &mut sequence, next) {
                    Ok(()) => {
                        if let Phase::Recording { applied, .. } = &mut sequence.phase {
                            *applied = next;
                        }
                    }
                    Err(err) => {
                        sequence.end.get_or_insert(End::Failed(err));
                    }
                }
            }
        }
        world.entity_mut(work).insert(sequence);
    }
}

fn advance_sequences(world: &mut World) {
    let works: Vec<Entity> = world
        .query_filtered::<Entity, With<Sequence>>()
        .iter(world)
        .collect();
    for work in works {
        let Some(mut sequence) = world.entity_mut(work).take::<Sequence>() else {
            continue;
        };
        if advance(world, work, &mut sequence) {
            finish(world, work, sequence);
        } else {
            report_progress(world, work, &sequence);
            world.entity_mut(work).insert(sequence);
        }
    }
}

/// One frame of a sequence. True when it is over.
fn advance(world: &mut World, work: Entity, sequence: &mut Sequence) -> bool {
    if sequence.end.is_none() {
        if world.get_entity(sequence.camera).is_err() {
            sequence.end = Some(End::Failed(CommandError::failed(
                "camera_gone",
                "the camera was despawned during the sequence",
            )));
        } else if sequence.generation != 0
            && placement_generation(world, sequence.camera) != sequence.generation
        {
            sequence.end = Some(End::Superseded);
        } else if sequence.cancel_requested {
            sequence.end = Some(End::Cancelled);
        }
    }
    if sequence.end.is_some() {
        return wind_down(world, work, sequence);
    }
    // Several phases can pass in one frame; each arm either waits or moves on.
    loop {
        match std::mem::replace(&mut sequence.phase, Phase::Place) {
            Phase::ShowView {
                since,
                mut active_since,
            } => {
                let now = frame(world);
                if camera_active(world, sequence.camera) {
                    active_since.get_or_insert(now);
                }
                if active_since.is_some_and(|since| now > since) {
                    continue;
                }
                if now.wrapping_sub(since) >= SHOW_CAP {
                    let view = match sequence.source.view() {
                        ActiveViewport::Scene => "Scene View",
                        ActiveViewport::Game => "Game View",
                    };
                    sequence.end = Some(End::Failed(CommandError::failed(
                        "camera_inactive",
                        format!(
                            "the {} did not render within {SHOW_CAP} frames of showing the {view}",
                            sequence.source.name()
                        ),
                    )));
                    return wind_down(world, work, sequence);
                }
                sequence.phase = Phase::ShowView {
                    since,
                    active_since,
                };
                return false;
            }
            Phase::Place => {
                if let Output::Video { .. } = sequence.output
                    && sequence.index > 0
                {
                    // Pose 0 is placed and settled: start recording.
                    start_recording(world, work, sequence);
                    return false;
                }
                if sequence.index >= sequence.steps.len() {
                    return true;
                }
                let index = sequence.index;
                if let Err(err) = apply_step_facets(world, &sequence.steps[index])
                    .and_then(|()| place(world, sequence, index))
                {
                    if matches!(sequence.output, Output::Video { .. }) {
                        sequence.end = Some(End::Failed(err));
                        return wind_down(world, work, sequence);
                    }
                    sequence.fail_step(err, None);
                    continue;
                }
                if sequence.placed.as_ref().is_some_and(Placed::ground_pending) {
                    sequence.phase = Phase::Ground {
                        since: Instant::now(),
                    };
                    return false;
                }
                if let Err(err) = settle_or_wait(world, work, sequence) {
                    sequence.end = Some(End::Failed(err));
                    return wind_down(world, work, sequence);
                }
                return false;
            }
            Phase::Ground { since } => {
                let still_pending = match sequence.placed.as_mut() {
                    Some(placed) => match guard_placed(world, placed) {
                        Ok(()) => placed.ground_pending(),
                        Err(err) => {
                            sequence.end = Some(End::Failed(err));
                            return wind_down(world, work, sequence);
                        }
                    },
                    None => false,
                };
                if still_pending && since.elapsed() < GROUND_WAIT {
                    sequence.phase = Phase::Ground { since };
                    return false;
                }
                if let Err(err) = settle_or_wait(world, work, sequence) {
                    sequence.end = Some(End::Failed(err));
                    return wind_down(world, work, sequence);
                }
                return false;
            }
            Phase::Settled => {
                // Finer ground can stream in while settling: lift again, and give a lift a frame
                // to propagate before capturing.
                let lifted = match sequence.placed.as_mut() {
                    Some(placed) => {
                        let before = placed.lifted_by();
                        if let Err(err) = guard_placed(world, placed) {
                            sequence.end = Some(End::Failed(err));
                            return wind_down(world, work, sequence);
                        }
                        placed.lifted_by() > before
                    }
                    None => false,
                };
                if lifted {
                    sequence.phase = Phase::WaitFrame {
                        since: frame(world),
                    };
                    return false;
                }
                sequence.phase = Phase::Ready;
                continue;
            }
            Phase::WaitFrame { since } => {
                if frame(world) > since {
                    sequence.phase = Phase::Ready;
                    continue;
                }
                sequence.phase = Phase::WaitFrame { since };
                return false;
            }
            Phase::Settling => {
                sequence.phase = Phase::Settling;
                return false;
            }
            Phase::Ready => {
                if let Output::Video { .. } = sequence.output {
                    // Pose 0 is in place; the next `Place` starts the recording.
                    sequence.index = 1;
                    continue;
                }
                request_still(world, work, sequence);
                return false;
            }
            Phase::Capturing => {
                sequence.phase = Phase::Capturing;
                return false;
            }
            Phase::Captured(result) => {
                record_still(world, sequence, result);
                sequence.index += 1;
                continue;
            }
            Phase::Recording {
                request,
                applied,
                stop_sent,
            } => {
                sequence.phase = Phase::Recording {
                    request,
                    applied,
                    stop_sent,
                };
                return false;
            }
            Phase::Recorded(result) => {
                match result {
                    Ok(saved) => sequence.video = Some(saved),
                    Err(err) => {
                        let error = world
                            .get_resource::<ename_engine::capture::CaptureSources>()
                            .map_or_else(
                                || CommandError::failed(err.kind(), err.to_string()),
                                |sources| sources.error(sequence.source.name(), &err),
                            );
                        sequence.end = Some(End::Failed(error));
                    }
                }
                return true;
            }
        }
    }
}

/// After placing: a settle gate when asked for, else a frame for the pose to propagate.
fn settle_or_wait(
    world: &mut World,
    work: Entity,
    sequence: &mut Sequence,
) -> Result<(), CommandError> {
    sequence.phase = match &sequence.settle {
        Some(settle) => {
            let gate = SettleGate::new(settle, world)?;
            world.entity_mut(work).insert(gate);
            Phase::Settling
        }
        None => Phase::WaitFrame {
            since: frame(world),
        },
    };
    Ok(())
}

/// An ending sequence: waits for a capture or recording in flight, then reports done.
fn wind_down(world: &mut World, work: Entity, sequence: &mut Sequence) -> bool {
    match std::mem::replace(&mut sequence.phase, Phase::Place) {
        Phase::Settling => {
            world.entity_mut(work).remove::<SettleGate>();
            true
        }
        Phase::Capturing => {
            sequence.phase = Phase::Capturing;
            false
        }
        Phase::Captured(result) => {
            record_still(world, sequence, result);
            true
        }
        Phase::Recording {
            request,
            applied,
            stop_sent,
        } => {
            if !stop_sent && world.get_entity(request).is_ok() {
                world.trigger(VideoStopRequested { entity: request });
            }
            sequence.phase = Phase::Recording {
                request,
                applied,
                stop_sent: true,
            };
            false
        }
        Phase::Recorded(result) => {
            if let Ok(saved) = result {
                sequence.video = Some(saved);
            }
            true
        }
        _ => true,
    }
}

fn apply_step_facets(world: &mut World, step: &Step) -> Result<(), CommandError> {
    if let Some(viewpoint) = &step.facets
        && !viewpoint.facets.is_empty()
    {
        apply_facets(world, &viewpoint.facets)?;
    }
    Ok(())
}

/// Places the camera at step `index`'s pose.
fn place(world: &mut World, sequence: &mut Sequence, index: usize) -> Result<(), CommandError> {
    let pose = sequence.steps[index].pose;
    let request = PoseRequest {
        position: Some(pose.position),
        aim: Aim::Angles {
            yaw: Some(pose.yaw),
            pitch: Some(pose.pitch),
        },
        advance: 0.0,
    };
    let placed = place_camera_pose(
        world,
        sequence.source.choice(),
        &request,
        sequence.options.clone(),
    )?;
    sequence.camera = placed.camera();
    sequence.generation = placed.generation();
    sequence.placed = Some(placed);
    Ok(())
}

fn request_still(world: &mut World, work: Entity, sequence: &mut Sequence) {
    let Output::Stills { dir } = &sequence.output else {
        return;
    };
    let file = sequence.steps[sequence.index]
        .file
        .clone()
        .unwrap_or_else(|| format!("{}.png", sequence.index));
    let request = ScreenshotRequest {
        source: CaptureSource::Camera(sequence.camera),
        path: dir.join(&file),
    };
    // The pose and frame the capture shows, read now: by the time the PNG is written another
    // call may have moved the camera.
    let (position, rotation) = read_pose(world, sequence.camera).unwrap_or_default();
    let (yaw, pitch) = yaw_pitch(rotation);
    sequence.pending = Some(ManifestFrame {
        file: None,
        viewpoint: sequence.steps[sequence.index].viewpoint.clone(),
        position: round_vec(position),
        yaw: round2(yaw),
        pitch: round2(pitch),
        frame: frame(world),
        width: None,
        height: None,
        error: None,
        busy: None,
    });
    world.spawn(request).observe(
        move |done: On<ScreenshotFinished>, mut sequences: Query<&mut Sequence>| {
            if let Ok(mut sequence) = sequences.get_mut(work) {
                sequence.phase = Phase::Captured(done.result.clone());
            }
        },
    );
    sequence.phase = Phase::Capturing;
}

fn record_still(
    world: &mut World,
    sequence: &mut Sequence,
    result: Result<SavedScreenshot, ScreenshotError>,
) {
    let index = sequence.index;
    let step = &sequence.steps[index];
    let Some(mut entry) = sequence.pending.take() else {
        return;
    };
    match result {
        Ok(saved) => {
            let size = saved.size;
            match sequence.first_size {
                None => sequence.first_size = Some(size),
                Some(first) if first != size => sequence.size_changed = true,
                Some(_) => {}
            }
            let file = step.file.clone().unwrap_or_default();
            entry.file = Some(file.clone());
            entry.width = Some(size.x);
            entry.height = Some(size.y);
            sequence.shots.push(Shot {
                viewpoint: step.viewpoint.clone(),
                file,
            });
        }
        Err(err) => {
            entry.error = Some(err.kind().to_string());
            let error = world
                .get_resource::<ename_engine::capture::CaptureSources>()
                .map_or_else(
                    || CommandError::failed(err.kind(), err.to_string()),
                    |sources| sources.error(sequence.source.name(), &err),
                );
            sequence.failed.push(Failure {
                step: index,
                viewpoint: step.viewpoint.clone(),
                error: error.kind.to_string(),
                message: error.message,
                busy: None,
            });
        }
    }
    sequence.frames.push(entry);
}

fn start_recording(world: &mut World, work: Entity, sequence: &mut Sequence) {
    let Output::Video { path, frame_rate } = &sequence.output else {
        return;
    };
    let steps = NonZeroU32::new(sequence.steps.len() as u32).unwrap_or(NonZeroU32::MIN);
    let mut request = VideoRequest::new(
        CaptureSource::Camera(sequence.camera),
        path.clone(),
        ename_engine::video::VideoLength::Frames(steps),
    );
    request.frame_rate = *frame_rate;
    let request = world
        .spawn(request)
        .observe(
            move |done: On<VideoFinished>, mut sequences: Query<&mut Sequence>| {
                if let Ok(mut sequence) = sequences.get_mut(work) {
                    sequence.phase = Phase::Recorded(done.result.clone());
                }
            },
        )
        .id();
    sequence.phase = Phase::Recording {
        request,
        applied: 0,
        stop_sent: false,
    };
}

fn report_progress(world: &mut World, work: Entity, sequence: &Sequence) {
    let phase = match &sequence.phase {
        Phase::ShowView { .. } => "showing_view",
        Phase::Ground { .. } => "waiting_for_ground",
        Phase::Settling | Phase::Settled => "settling",
        Phase::Place | Phase::WaitFrame { .. } | Phase::Ready => "placing",
        Phase::Capturing | Phase::Captured(_) => "capturing",
        Phase::Recording { .. } | Phase::Recorded(_) => "recording",
    };
    let mut progress = json!({
        "done": sequence.index.min(sequence.steps.len()),
        "total": sequence.steps.len(),
        "phase": phase,
    });
    if let Some(viewpoint) = sequence
        .steps
        .get(sequence.index)
        .and_then(|step| step.viewpoint.as_ref())
    {
        progress["viewpoint"] = json!(viewpoint);
    }
    if let Phase::Recording { request, .. } = sequence.phase
        && let Some(video) = world.get::<VideoProgress>(request)
    {
        progress["done"] = json!(video.frames);
    }
    if let Some(gate) = world.get::<SettleGate>(work) {
        progress["busy"] = json!(gate.progress().busy);
    }
    world
        .resource_mut::<Jobs>()
        .set_progress(sequence.job.id(), progress);
}

/// Writes the manifest, restores, and ends the job as the sequence ended.
fn finish(world: &mut World, work: Entity, mut sequence: Sequence) {
    if let Output::Video { .. } = sequence.output {
        sequence.frames = video_frames(&sequence);
    }
    let manifest_path = sequence.output.manifest_path();
    let run = world
        .get_resource::<RunId>()
        .map_or_else(String::new, |run| run.as_str().to_owned());
    let (width, height) = match (&sequence.video, sequence.first_size) {
        (Some(video), _) => (Some(video.size.x), Some(video.size.y)),
        (None, Some(size)) => (Some(size.x), Some(size.y)),
        (None, None) => (None, None),
    };
    let manifest = Manifest {
        run,
        source: sequence.source.name().to_owned(),
        width,
        height,
        virtual_time: sequence.virtual_time,
        frames: std::mem::take(&mut sequence.frames),
    };
    let written = write_manifest(&manifest_path, &manifest);

    // The pose goes back only while this sequence placed the camera last: a newer placement,
    // even one made while a cancel was winding down, belongs to its caller.
    let ours = sequence.generation == 0
        || placement_generation(world, sequence.camera) == sequence.generation;
    let choice = sequence.source.choice();
    let saved = std::mem::replace(
        &mut sequence.saved,
        SavedState {
            view: ActiveViewport::Scene,
            pose: None,
            paused: None,
            driver: None,
            overlays: None,
        },
    );
    restore_state(
        world,
        choice,
        sequence.camera,
        saved,
        sequence.restore && ours,
    );
    world.resource_mut::<RunningSequence>().0 = None;

    let frames = manifest
        .frames
        .iter()
        .filter(|frame| frame.error.is_none())
        .count() as u32;
    let mut result = SequenceResult {
        manifest: manifest_path.clone(),
        width,
        height,
        size_changed: sequence.size_changed,
        frames,
        shots: sequence.shots,
        failed: sequence.failed,
        ..default()
    };
    match &sequence.output {
        Output::Stills { dir } => result.dir = Some(dir.clone()),
        Output::Video { path, .. } => {
            result.path = Some(path.clone());
            if let Some(video) = &sequence.video {
                result.frames = video.frames;
                result.dropped_frames = Some(video.dropped_frames);
                result.ended = Some(video.ended.name());
            }
        }
    }
    let job = sequence.job.clone();
    let landed = match &written {
        Ok(()) => format!(
            "{} frames landed and are listed in {}",
            result.frames,
            manifest_path.display()
        ),
        Err(err) => format!("{} frames landed; {}", result.frames, err.message),
    };
    let outcome = match sequence.end {
        None => Some(written.map(|()| result)),
        Some(End::Cancelled) => {
            if let Err(err) = &written {
                warn!("cancelled capture sequence: {}", err.message);
            }
            None
        }
        Some(End::Superseded) => Some(Err(CommandError::failed(
            "superseded",
            format!("the camera was placed by another call during the sequence; {landed}"),
        ))),
        Some(End::Failed(mut err)) => {
            err.message = format!("{}. {landed}", err.message);
            Some(Err(err))
        }
    };
    let mut jobs = world.resource_mut::<Jobs>();
    match outcome {
        Some(outcome) => jobs.finish(&job, outcome),
        None => jobs.cancelled(job.id()),
    }
    world.despawn(work);
}

/// One manifest entry per recorded pose of a video, in order.
fn video_frames(sequence: &Sequence) -> Vec<ManifestFrame> {
    // A recording that ended early holds only its first frames.
    let recorded = sequence
        .video
        .as_ref()
        .map_or(0, |video| video.frames as usize);
    sequence
        .steps
        .iter()
        .take(recorded)
        .enumerate()
        .map(|(index, step)| ManifestFrame {
            file: None,
            viewpoint: step.viewpoint.clone(),
            position: round_vec(step.pose.position),
            yaw: round2(step.pose.yaw),
            pitch: round2(step.pose.pitch),
            frame: index as u32,
            width: None,
            height: None,
            error: None,
            busy: None,
        })
        .collect()
}

/// Atomically, as PNGs are written: a reader never sees half a manifest.
fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), CommandError> {
    let fail = |err: String| {
        CommandError::failed("write", format!("cannot write `{}`: {err}", path.display()))
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| fail(err.to_string()))?;
    }
    let text = serde_json::to_vec_pretty(manifest).map_err(|err| fail(err.to_string()))?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, text).map_err(|err| fail(err.to_string()))?;
    std::fs::rename(&temp, path).map_err(|err| fail(err.to_string()))
}

fn camera_active(world: &World, camera: Entity) -> bool {
    world
        .get::<Camera>(camera)
        .is_some_and(|camera| camera.is_active)
}

fn frame(world: &World) -> u32 {
    world
        .get_resource::<FrameCount>()
        .map_or(0, |count| count.0)
}

/// A viewpoint name as a file name that is valid on Linux, macOS and Windows: characters outside
/// `[A-Za-z0-9._-]` become `_`, trailing dots go, a Windows device name gets a `_` prefix, and a
/// name already `taken` (ignoring case, as both default filesystems do) gets `-2`, `-3`.
pub(crate) fn file_name_for(name: &str, taken: &mut Vec<String>) -> String {
    let mut stem: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Room for a suffix and `.png` within every filesystem's 255-byte name limit.
    stem.truncate(MAX_STEM);
    while stem.ends_with('.') {
        stem.pop();
    }
    if stem.is_empty() {
        stem.push('_');
    }
    let device = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((device.starts_with("COM") || device.starts_with("LPT"))
            && device.len() == 4
            && device.as_bytes()[3].is_ascii_digit());
    if reserved {
        stem.insert(0, '_');
    }
    let mut candidate = stem.clone();
    let mut suffix = 2;
    while taken
        .iter()
        .any(|used| used.eq_ignore_ascii_case(&candidate))
    {
        candidate = format!("{stem}-{suffix}");
        suffix += 1;
    }
    taken.push(candidate.clone());
    format!("{candidate}.png")
}

#[cfg(test)]
mod tests;
