//! `camera.look_at` and `camera.get_pose`: placing a camera with one readable pose, and reading
//! it back in the same shape.
//!
//! A pose is absolute metres plus yaw and pitch in degrees (`ename_engine::camera::pose`). The
//! default camera is the one the shown view renders. Placing a camera clears its leftover fly
//! velocity and moves the orbit pivot to what it looks at.
//!
//! # Ground guard
//!
//! `ground` picks the probes (`ename_engine::camera::ground`): `none`, `colliders`, `meshes` or
//! `both`, defaulting to the game's `CameraGroundGuard`, which itself defaults to `both`. A camera
//! that would sit less than `min_clearance` above the surface below it is lifted, but never through
//! a surface above it. A probe that has not loaded the ground yet is polled every frame for up to
//! [`GROUND_WAIT`]; "no ground" (space, a void) is not waited for. With `none` the guard is inert
//! and no ground or view-centre hit is computed. An optional `settle` runs after the lift, and the
//! guard checks again after it, because streamed ground can rise once finer data arrives.
//!
//! # Timing
//!
//! `camera.look_at` is a job: it finishes once one frame has run with the final pose, as
//! `editor.set_view` does. The newest placement of a camera wins: a waiting job whose camera was
//! placed again finishes `superseded` and writes nothing more. Cancelling stops only the wait.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use bevy::{
    diagnostic::FrameCount,
    math::{DQuat, DVec3},
    prelude::*,
};
use ename_engine::{
    bigspace::grid::{self, PositionError},
    camera::{
        CameraDriver, Tracking, VirtualCamera,
        ground::{
            CEILING_MARGIN, CameraGroundGuard, GroundGuardMode, MAX_RAY_DISTANCE, ProbeKind,
            cast_ground_ray, center_hit, ground_below, has_ground_probes, plan_lift,
        },
        pose::{roll_degrees, rotation_from_yaw_pitch, yaw_pitch, yaw_pitch_towards},
        single_main_camera,
    },
    command::{
        Call, CommandAppExt, CommandError, CommandSpec, EntityTarget, JobHandle, Jobs, OnCancel,
    },
    wait::{SettleGate, SettleParams, SettleTimedOut, Settled, WaitSystems},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    camera::{EditorCamera, MAX_EDITOR_PITCH_DEGREES, reset_editor_camera_motion},
    panels::{ActiveViewport, UiState},
};

/// How long a placement polls a probe whose ground has not loaded yet, in real time.
pub(crate) const GROUND_WAIT: Duration = Duration::from_secs(10);
/// Rounds of settle and re-lift after the first lift, at most.
const MAX_SETTLE_ROUNDS: u32 = 3;

/// Registers `camera.look_at`, `camera.get_pose` and the system that finishes placements.
pub(crate) struct CameraCommandsPlugin;

impl Plugin for CameraCommandsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraPlacements>()
            .register_job_command(
                CommandSpec::new(
                    "camera.look_at",
                    "Place a camera at an absolute position facing a point, an entity, or a \
                     yaw/pitch in degrees, kept above the ground; returns the pose read back.",
                ),
                look_at,
            )
            .register_command(
                CommandSpec::new(
                    "camera.get_pose",
                    "A camera's absolute position, yaw and pitch in degrees, the ground below it \
                     and where its view centre meets the ground.",
                )
                .read_only(),
                get_pose,
            )
            // After the settle gates have opened this frame, so a settled placement re-checks
            // the ground in the same frame.
            .add_systems(
                Last,
                advance_placements
                    .after(WaitSystems::Resolve)
                    .run_if(any_with_component::<Placement>),
            );
    }
}

/// Which camera a command acts on.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CameraChoice {
    /// The Scene View's free camera.
    EditorCamera,
    /// The loaded stage's `MainCamera`, shown in the Game View.
    GameCamera,
}

impl CameraChoice {
    pub(crate) fn shown(world: &World) -> Self {
        match world.get_resource::<UiState>().map(|ui| ui.active_viewport) {
            Some(ActiveViewport::Game) => Self::GameCamera,
            _ => Self::EditorCamera,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LookAtParams {
    /// Which camera to place. Defaults to the one the shown view renders.
    camera: Option<CameraChoice>,
    /// Absolute metres, Y up. Defaults to where the camera is.
    position: Option<[f64; 3]>,
    /// A point to face. Give at most one of `look_at`, `look_at_entity`, `yaw`/`pitch` and
    /// `rotation`; none keeps the current orientation.
    look_at: Option<[f64; 3]>,
    /// An entity to face, at its absolute position.
    look_at_entity: Option<EntityTarget>,
    /// Degrees: 0 looks along -Z, 90 along +X. The other angle keeps its value when one is left
    /// out.
    yaw: Option<f64>,
    /// Degrees, positive up. The editor camera holds at most ±89.
    pitch: Option<f64>,
    /// A quaternion `[x, y, z, w]`, as a `Transform` holds it. Its roll is dropped.
    rotation: Option<[f64; 4]>,
    /// Metres to move along the final view direction, after orienting.
    advance: Option<f64>,
    /// Which ground keeps the camera clear of it: "colliders" (static physics colliders),
    /// "meshes" (render meshes marked as terrain), "both", or "none" for no guard (a deliberate
    /// shot from below). Defaults to the game's setting, `CameraGroundGuard`.
    ground: Option<GroundGuardMode>,
    /// Keep at least this many metres above the ground below. Defaults to the game's setting
    /// (2 unless it changes it). No effect with `ground: "none"`.
    min_clearance: Option<f64>,
    /// Wait for loading and streaming to settle after placing (`wait.settled`'s params; `{}`
    /// for its defaults), then check the ground again.
    settle: Option<SettleParams>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetPoseParams {
    /// Which camera to read. Defaults to the one the shown view renders.
    camera: Option<CameraChoice>,
    /// Which ground to report, as on `camera.look_at`. Defaults to the game's setting.
    ground: Option<GroundGuardMode>,
}

/// How a placement orients the camera.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Aim {
    Keep,
    Point(DVec3),
    Entity(Entity),
    Angles {
        yaw: Option<f64>,
        pitch: Option<f64>,
    },
    Rotation(DQuat),
}

/// Where to put a camera.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoseRequest {
    pub position: Option<DVec3>,
    pub aim: Aim,
    pub advance: f64,
}

/// How to guard and settle a placement.
#[derive(Clone, Debug)]
pub(crate) struct PlaceOptions {
    pub ground: GroundGuardMode,
    pub min_clearance: f64,
    pub settle: Option<SettleParams>,
}

impl PlaceOptions {
    /// The options of a call: what it gives, else the game's [`CameraGroundGuard`].
    pub(crate) fn new(
        world: &World,
        ground: Option<GroundGuardMode>,
        min_clearance: Option<f64>,
        settle: Option<SettleParams>,
    ) -> Result<Self, CommandError> {
        let guard = game_guard(world);
        let ground = ground.unwrap_or(guard.ground);
        let min_clearance = min_clearance.unwrap_or(guard.min_clearance);
        if !(min_clearance.is_finite() && min_clearance >= 0.0) {
            return Err(invalid_pose(format!(
                "`min_clearance` must be a finite number of metres, at least 0, not {min_clearance}"
            )));
        }
        Ok(Self {
            ground,
            min_clearance,
            settle,
        })
    }
}

/// The game's default guard, or the engine's default when the resource is missing.
fn game_guard(world: &World) -> CameraGroundGuard {
    world
        .get_resource::<CameraGroundGuard>()
        .copied()
        .unwrap_or_default()
}

/// Fields a command adds to the placement result.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReportExtras {
    pub viewpoint: Option<String>,
    pub facets_applied: Option<Map<String, Value>>,
}

/// A camera's pose as the commands report it. `position`, `yaw` and `pitch` paste into
/// `camera.look_at` as they are.
#[derive(Serialize, JsonSchema, Debug, Clone, PartialEq)]
pub(crate) struct PoseReport {
    camera: CameraChoice,
    /// Absolute metres, rounded to 0.01.
    position: [f64; 3],
    /// Degrees, rounded to 0.01.
    yaw: f64,
    pitch: f64,
    /// Whether the camera renders in the shown view, so a screenshot of it would work.
    active: bool,
    /// The surface below the camera. `null` with no ground probe or no ground there.
    ground: Option<GroundReport>,
    /// Where the view centre meets a surface. `null` above the horizon or with no probe.
    center_hit: Option<CenterHit>,
    notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewpoint: Option<String>,
    /// The applied value of each facet, as JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    facets_applied: Option<Map<String, Value>>,
}

#[derive(Serialize, JsonSchema, Debug, Clone, PartialEq)]
struct GroundReport {
    /// The probes asked.
    mode: GroundGuardMode,
    /// Absolute metres.
    height: f64,
    /// The probe the ground came from: "colliders", "meshes", or "custom" for a game's own.
    from: Option<ProbeKind>,
    /// Metres from the ground up to the camera.
    clearance: f64,
    /// Metres the guard lifted the camera by.
    #[serde(skip_serializing_if = "Option::is_none")]
    lifted_by: Option<f64>,
    /// True when no settle ran, so finer ground may still stream in.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    provisional: bool,
}

#[derive(Serialize, JsonSchema, Debug, Clone, PartialEq)]
struct CenterHit {
    point: [f64; 3],
    distance: f64,
}

/// Placement generations per camera. The newest placement of a camera wins.
#[derive(Resource, Default)]
struct CameraPlacements(HashMap<Entity, u64>);

impl CameraPlacements {
    fn bump(&mut self, camera: Entity) -> u64 {
        let generation = self.0.entry(camera).or_default();
        *generation += 1;
        *generation
    }

    fn current(&self, camera: Entity) -> u64 {
        self.0.get(&camera).copied().unwrap_or_default()
    }
}

/// What a placement keeps facing after a lift.
#[derive(Clone, Copy, Debug)]
enum Target {
    Point(DVec3),
    Entity(Entity),
}

/// The newest placement generation of `camera`. A sequence that placed it compares this with
/// [`Placed::generation`] to see whether another call placed it since.
pub(crate) fn placement_generation(world: &World, camera: Entity) -> u64 {
    world
        .get_resource::<CameraPlacements>()
        .map_or(0, |placements| placements.current(camera))
}

impl Placed {
    pub(crate) fn camera(&self) -> Entity {
        self.camera
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// A ground probe had not loaded the ground below the camera at the last guard.
    pub(crate) fn ground_pending(&self) -> bool {
        self.ground_pending
    }

    /// Metres the guard has lifted the camera by so far.
    pub(crate) fn lifted_by(&self) -> f64 {
        self.lifted_by
    }
}

/// Runs the ground guard again on a placement, as `camera.look_at` does while the ground loads
/// and after it settles.
pub(crate) fn guard_placed(world: &mut World, placed: &mut Placed) -> Result<(), CommandError> {
    guard(world, placed)
}

/// A placement after its first write: what the job carries on with.
pub(crate) struct Placed {
    camera: Entity,
    choice: CameraChoice,
    generation: u64,
    target: Option<Target>,
    options: PlaceOptions,
    lifted_by: f64,
    ground_pending: bool,
    notes: Vec<String>,
    extras: ReportExtras,
}

#[derive(Debug)]
enum Phase {
    Ground { since: Instant },
    Settling,
    Settled,
    Frame { since: u32 },
}

/// A `camera.look_at` or `viewpoint.goto` job in progress, on its work entity.
#[derive(Component)]
struct Placement {
    job: JobHandle<PoseReport>,
    placed: Placed,
    phase: Phase,
    rounds: u32,
}

fn look_at(
    In(call): In<Call<LookAtParams>>,
    world: &mut World,
) -> Result<JobHandle<PoseReport>, CommandError> {
    let params = &call.params;
    let forms = [
        params.look_at.is_some(),
        params.look_at_entity.is_some(),
        params.yaw.is_some() || params.pitch.is_some(),
        params.rotation.is_some(),
    ];
    if forms.iter().filter(|given| **given).count() > 1 {
        return Err(invalid_pose(
            "give at most one of `look_at`, `look_at_entity`, `yaw`/`pitch` and `rotation`",
        ));
    }
    let position = params
        .position
        .map(|p| finite_vec("position", p))
        .transpose()?;
    let aim = if let Some(point) = params.look_at {
        Aim::Point(finite_vec("look_at", point)?)
    } else if let Some(target) = &params.look_at_entity {
        Aim::Entity(target.resolve(world)?)
    } else if let Some([x, y, z, w]) = params.rotation {
        let rotation = DQuat::from_xyzw(x, y, z, w);
        if !rotation.is_finite() || rotation.length() < 1e-6 {
            return Err(invalid_pose(
                "`rotation` must be a finite, non-zero quaternion",
            ));
        }
        Aim::Rotation(rotation.normalize())
    } else if params.yaw.is_some() || params.pitch.is_some() {
        Aim::Angles {
            yaw: params.yaw.map(|yaw| finite("yaw", yaw)).transpose()?,
            pitch: params
                .pitch
                .map(|pitch| finite("pitch", pitch))
                .transpose()?,
        }
    } else {
        Aim::Keep
    };
    let advance = finite("advance", params.advance.unwrap_or(0.0))?;
    let options = PlaceOptions::new(
        world,
        params.ground,
        params.min_clearance,
        params.settle.clone(),
    )?;
    let request = PoseRequest {
        position,
        aim,
        advance,
    };
    start_placement(
        world,
        &call,
        params.camera,
        &request,
        options,
        ReportExtras::default(),
    )
}

fn get_pose(
    In(call): In<Call<GetPoseParams>>,
    world: &mut World,
) -> Result<PoseReport, CommandError> {
    let choice = call
        .params
        .camera
        .unwrap_or_else(|| CameraChoice::shown(world));
    let camera = find_camera(world, choice)?;
    let mode = call.params.ground.unwrap_or(game_guard(world).ground);
    Ok(report(world, camera, choice, mode, None))
}

/// What a placement would refuse before it writes anything: the camera (`no_game_camera`,
/// `camera_driven`, no position) and the settle params. For a command that changes other state
/// first, such as `viewpoint.goto` applying facets.
pub(crate) fn check_placement(
    world: &mut World,
    choice: Option<CameraChoice>,
    options: &PlaceOptions,
) -> Result<(), CommandError> {
    let choice = choice.unwrap_or_else(|| CameraChoice::shown(world));
    let camera = resolve_camera(world, choice)?;
    read_pose(world, camera)
        .ok_or_else(|| CommandError::failed("not_on_grid", "the camera has no world position"))?;
    if let Some(settle) = &options.settle {
        SettleGate::new(settle, world)?;
    }
    Ok(())
}

/// Places the camera and starts the job that waits for ground, settles and finishes it.
pub(crate) fn start_placement<P>(
    world: &mut World,
    call: &Call<P>,
    choice: Option<CameraChoice>,
    request: &PoseRequest,
    options: PlaceOptions,
    extras: ReportExtras,
) -> Result<JobHandle<PoseReport>, CommandError> {
    let choice = choice.unwrap_or_else(|| CameraChoice::shown(world));
    // Checked before anything moves, so a bad probe name places nothing.
    if let Some(settle) = &options.settle {
        SettleGate::new(settle, world)?;
    }
    let mut placed = place_camera_pose(world, choice, request, options)?;
    placed.extras = extras;
    let work = world.spawn_empty().id();
    let job: JobHandle<PoseReport> = match call.start_job(world, work, OnCancel::Despawn) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    let phase = if placed.ground_pending {
        Phase::Ground {
            since: Instant::now(),
        }
    } else {
        next_after_ground(world, work, &placed)?
    };
    let (on_timeout, started) = (job.clone(), job.clone());
    world
        .entity_mut(work)
        .insert(Placement {
            job,
            placed,
            phase,
            rounds: 0,
        })
        .observe(|done: On<Settled>, mut placements: Query<&mut Placement>| {
            if let Ok(mut placement) = placements.get_mut(done.entity) {
                placement.phase = Phase::Settled;
            }
        })
        .observe(
            move |timeout: On<SettleTimedOut>, mut jobs: ResMut<Jobs>, mut commands: Commands| {
                jobs.finish(&on_timeout, Err(timeout.error()));
                commands.entity(timeout.entity).despawn();
            },
        );
    Ok(started)
}

/// Settling when asked for, else straight to the frame wait.
fn next_after_ground(
    world: &mut World,
    work: Entity,
    placed: &Placed,
) -> Result<Phase, CommandError> {
    match &placed.options.settle {
        Some(settle) => {
            let gate = SettleGate::new(settle, world)?;
            world.entity_mut(work).insert(gate);
            Ok(Phase::Settling)
        }
        None => Ok(Phase::Frame {
            since: frame(world),
        }),
    }
}

/// Writes the pose, clears the camera's motion and runs the ground guard once. The core of
/// `camera.look_at`, for any command that places a camera; the job carries on from what it
/// returns.
pub(crate) fn place_camera_pose(
    world: &mut World,
    choice: CameraChoice,
    request: &PoseRequest,
    options: PlaceOptions,
) -> Result<Placed, CommandError> {
    let camera = resolve_camera(world, choice)?;
    let (current_position, current_rotation) = read_pose(world, camera)
        .ok_or_else(|| CommandError::failed("not_on_grid", "the camera has no world position"))?;
    let mut notes = Vec::new();
    let position = request.position.unwrap_or(current_position);
    let (current_yaw, current_pitch) = yaw_pitch(current_rotation);
    let (target, (yaw, pitch)) = match request.aim {
        Aim::Keep => (None, (current_yaw, current_pitch)),
        Aim::Angles { yaw, pitch } => (
            None,
            (yaw.unwrap_or(current_yaw), pitch.unwrap_or(current_pitch)),
        ),
        Aim::Rotation(rotation) => {
            let roll = roll_degrees(rotation);
            if roll.abs() >= 0.01 {
                notes.push(format!(
                    "dropped a roll of {:.2}°: a pose has none",
                    round2(roll)
                ));
            }
            (None, yaw_pitch(rotation))
        }
        Aim::Point(point) => (Some(Target::Point(point)), towards(position, point)?),
        Aim::Entity(entity) => {
            let point = entity_position(world, entity)?;
            (Some(Target::Entity(entity)), towards(position, point)?)
        }
    };
    let pitch = clamp_pitch(choice, pitch, &mut notes);
    let rotation = rotation_from_yaw_pitch(yaw, pitch);
    let position = position + rotation * DVec3::NEG_Z * request.advance;
    if let Some(point) = target
        .map(|target| target_point(world, target))
        .transpose()?
        && point.distance(position) < 1e-6
    {
        return Err(invalid_pose(
            "`advance` moved the camera onto the point it looks at",
        ));
    }
    // Every caller, a viewpoint file included, gets the same check before anything is written.
    if !(position.is_finite() && rotation.is_finite()) {
        return Err(invalid_pose(format!(
            "the pose is not finite: position {position}, yaw {yaw}, pitch {pitch}"
        )));
    }
    write_placed(world, choice, camera, target, position, rotation)?;
    let generation = world.resource_mut::<CameraPlacements>().bump(camera);
    let mut placed = Placed {
        camera,
        choice,
        generation,
        target,
        options,
        lifted_by: 0.0,
        ground_pending: false,
        notes,
        extras: ReportExtras::default(),
    };
    guard(world, &mut placed)?;
    Ok(placed)
}

/// Writes the pose, then clears the camera's motion with the orbit pivot on the target, read
/// after the write.
fn write_placed(
    world: &mut World,
    choice: CameraChoice,
    camera: Entity,
    target: Option<Target>,
    position: DVec3,
    rotation: DQuat,
) -> Result<(), CommandError> {
    write_pose(world, camera, position, rotation)?;
    let focus = target
        .map(|target| target_point(world, target))
        .transpose()?;
    reset_motion(world, choice, camera, focus);
    Ok(())
}

/// Lifts the camera above the ground below it, never through a surface above it. Records
/// whether a probe has not loaded its ground yet.
fn guard(world: &mut World, placed: &mut Placed) -> Result<(), CommandError> {
    placed.ground_pending = false;
    let mode = placed.options.ground;
    if !has_ground_probes(world, mode) {
        return Ok(());
    }
    let Some((position, current_rotation)) = read_pose(world, placed.camera) else {
        return Ok(());
    };
    let plan = plan_lift(world, position, placed.options.min_clearance, mode);
    placed.ground_pending = plan.ground.not_loaded;
    if let (true, Some(ceiling)) = (plan.limited, plan.ceiling) {
        placed.notes.push(if plan.crowded {
            format!(
                "less than {CEILING_MARGIN} m is free under the surface at y = {}; the camera \
                 stays on the ground below it",
                round2(ceiling)
            )
        } else {
            format!(
                "a surface at y = {} limits the lift to {} m under it, short of the {} m \
                 clearance asked for",
                round2(ceiling),
                CEILING_MARGIN,
                round2(placed.options.min_clearance)
            )
        });
    }
    let Some(lifted_y) = plan.target else {
        return Ok(());
    };
    let lifted = DVec3::new(position.x, lifted_y, position.z);
    let towards_target = placed
        .target
        .map(|target| target_point(world, target))
        .transpose()?
        .and_then(|point| yaw_pitch_towards(lifted, point));
    let rotation = match towards_target {
        Some((yaw, pitch)) => {
            rotation_from_yaw_pitch(yaw, clamp_pitch(placed.choice, pitch, &mut Vec::new()))
        }
        None => current_rotation,
    };
    write_placed(
        world,
        placed.choice,
        placed.camera,
        placed.target,
        lifted,
        rotation,
    )?;
    placed.lifted_by += lifted_y - position.y;
    Ok(())
}

/// Moves every waiting placement on by one frame.
fn advance_placements(world: &mut World) {
    let works: Vec<Entity> = world
        .query_filtered::<Entity, With<Placement>>()
        .iter(world)
        .collect();
    for work in works {
        let Some(mut placement) = world.entity_mut(work).take::<Placement>() else {
            continue;
        };
        match step(world, work, &mut placement) {
            Ok(None) => {
                world.entity_mut(work).insert(placement);
            }
            Ok(Some(report)) => {
                world
                    .resource_mut::<Jobs>()
                    .finish(&placement.job, Ok(report));
                world.despawn(work);
            }
            Err(err) => {
                world
                    .resource_mut::<Jobs>()
                    .finish(&placement.job, Err(err));
                world.despawn(work);
            }
        }
    }
}

/// One frame of a placement: `Some` when it is done.
fn step(
    world: &mut World,
    work: Entity,
    placement: &mut Placement,
) -> Result<Option<PoseReport>, CommandError> {
    let camera = placement.placed.camera;
    if world.get_entity(camera).is_err() {
        return Err(CommandError::failed(
            "camera_gone",
            "the camera was despawned while the placement waited",
        ));
    }
    if world.resource::<CameraPlacements>().current(camera) != placement.placed.generation {
        return Err(CommandError::failed(
            "superseded",
            "the camera was placed again while this placement waited; the newer one wins",
        ));
    }
    match placement.phase {
        Phase::Ground { since } => {
            guard(world, &mut placement.placed)?;
            if placement.placed.ground_pending {
                if since.elapsed() < GROUND_WAIT {
                    return Ok(None);
                }
                placement.placed.notes.push(format!(
                    "the ground below the camera had not loaded after {} s",
                    GROUND_WAIT.as_secs()
                ));
            }
            placement.phase = next_after_ground(world, work, &placement.placed)?;
            Ok(None)
        }
        Phase::Settling => Ok(None),
        Phase::Settled => {
            placement.rounds += 1;
            let before = placement.placed.lifted_by;
            guard(world, &mut placement.placed)?;
            let lifted_again = placement.placed.lifted_by > before;
            if lifted_again && placement.rounds < MAX_SETTLE_ROUNDS {
                placement.phase = next_after_ground(world, work, &placement.placed)?;
            } else {
                if lifted_again {
                    placement.placed.notes.push(format!(
                        "the ground was still rising after {MAX_SETTLE_ROUNDS} settles; the \
                         camera may be lifted again by a later call"
                    ));
                }
                placement.phase = Phase::Frame {
                    since: frame(world),
                };
            }
            Ok(None)
        }
        Phase::Frame { since } => {
            if frame(world) <= since {
                return Ok(None);
            }
            let (choice, mode) = (placement.placed.choice, placement.placed.options.ground);
            Ok(Some(report(
                world,
                camera,
                choice,
                mode,
                Some(&mut placement.placed),
            )))
        }
    }
}

/// The camera's pose read back, with the ground and the view centre's hit, and what `placed`
/// adds when a placement finishes.
fn report(
    world: &mut World,
    camera: Entity,
    choice: CameraChoice,
    mode: GroundGuardMode,
    placed: Option<&mut Placed>,
) -> PoseReport {
    let (lifted_by, provisional, mut notes, extras) = match placed {
        Some(placed) => (
            Some(placed.lifted_by),
            placed.options.settle.is_none(),
            std::mem::take(&mut placed.notes),
            std::mem::take(&mut placed.extras),
        ),
        None => (None, false, Vec::new(), ReportExtras::default()),
    };
    let (position, rotation) = read_pose(world, camera).unwrap_or_default();
    let (yaw, pitch) = yaw_pitch(rotation);
    let shown = CameraChoice::shown(world);
    let active = shown == choice
        && world
            .get::<Camera>(camera)
            .is_some_and(|camera| camera.is_active);
    if !active {
        let view = match choice {
            CameraChoice::EditorCamera => "scene",
            CameraChoice::GameCamera => "game",
        };
        notes.push(format!(
            "this camera does not render in the shown view: call `editor.set_view` with \
             view={view} before capturing it"
        ));
    }
    let mut ground = None;
    let mut hit = None;
    if has_ground_probes(world, mode) {
        let sample = ground_below(world, position, mode);
        ground = sample.surface.map(|height| GroundReport {
            mode,
            height: round2(height),
            from: sample.from,
            clearance: round2(position.y - height),
            lifted_by: lifted_by.map(round2),
            provisional: provisional || sample.not_loaded,
        });
        if sample.surface.is_none() {
            notes.push(no_ground_note(world, position, sample.not_loaded, mode));
        }
        if let Ok(forward) = Dir3::new((rotation * DVec3::NEG_Z).as_vec3()) {
            hit = center_hit(world, position, forward, mode).map(|point| CenterHit {
                point: round_vec(point),
                distance: round2(point.distance(position)),
            });
        }
    }
    PoseReport {
        camera: choice,
        position: round_vec(position),
        yaw: round2(yaw),
        pitch: round2(pitch),
        active,
        ground,
        center_hit: hit,
        notes,
        viewpoint: extras.viewpoint,
        facets_applied: extras.facets_applied,
    }
}

fn no_ground_note(
    world: &mut World,
    position: DVec3,
    not_loaded: bool,
    mode: GroundGuardMode,
) -> String {
    if not_loaded {
        return "the ground below the camera has not loaded".into();
    }
    match cast_ground_ray(world, position, Dir3::Y, MAX_RAY_DISTANCE, mode) {
        Some(distance) => format!(
            "no ground below the camera, but a surface {} m above it: the camera may be under \
             the ground",
            round2(distance)
        ),
        None => "no ground below the camera".into(),
    }
}

/// The camera entity a choice names, to read. `no_game_camera` with the count.
pub(crate) fn find_camera(world: &mut World, choice: CameraChoice) -> Result<Entity, CommandError> {
    match choice {
        CameraChoice::EditorCamera => world
            .query_filtered::<Entity, With<EditorCamera>>()
            .single(world)
            .map_err(|err| {
                CommandError::failed(
                    "no_editor_camera",
                    format!("the editor camera is missing: {err}"),
                )
            }),
        CameraChoice::GameCamera => single_main_camera(world).map_err(|count| {
            CommandError::failed(
                "no_game_camera",
                format!("the game camera needs exactly one `MainCamera`; there are {count}"),
            )
        }),
    }
}

/// The camera entity a choice names, to place. As [`find_camera`], plus `camera_driven` when a
/// `CameraDriver` would overwrite the pose next frame.
pub(crate) fn resolve_camera(
    world: &mut World,
    choice: CameraChoice,
) -> Result<Entity, CommandError> {
    let camera = find_camera(world, choice)?;
    match choice {
        CameraChoice::EditorCamera => Ok(camera),
        CameraChoice::GameCamera => {
            if let Some(virtual_camera) = live_virtual_camera(world, camera) {
                let name = world
                    .get::<Name>(virtual_camera)
                    .map_or_else(|| virtual_camera.to_bits().to_string(), |n| n.to_string());
                return Err(CommandError::failed(
                    "camera_driven",
                    "the game camera follows a virtual camera through its `CameraDriver`, which \
                     would overwrite the pose next frame",
                )
                .with_hint(format!(
                    "Move the virtual camera `{name}` instead, or set `CameraDriver.enabled` to \
                     false."
                )));
            }
            Ok(camera)
        }
    }
}

/// The virtual camera an enabled `CameraDriver` on `camera` follows, as the driver picks it.
fn live_virtual_camera(world: &mut World, camera: Entity) -> Option<Entity> {
    let driver = world.get::<CameraDriver>(camera)?;
    if !driver.enabled {
        return None;
    }
    let mask = driver.channel_mask;
    if let Tracking::Target(target) = driver.tracking {
        return world.get::<VirtualCamera>(target).map(|_| target);
    }
    world
        .query_filtered::<(Entity, &VirtualCamera), With<GlobalTransform>>()
        .iter(world)
        .filter(|(_, vc)| vc.enabled && vc.channel_mask & mask != 0)
        .max_by_key(|(_, vc)| vc.priority)
        .map(|(entity, _)| entity)
}

fn reset_motion(world: &mut World, choice: CameraChoice, camera: Entity, focus: Option<DVec3>) {
    match choice {
        CameraChoice::EditorCamera => reset_editor_camera_motion(world, camera, focus),
        CameraChoice::GameCamera => {
            if let Some(mut controller) =
                world.get_mut::<ename_engine::bigspace::BigSpaceCameraController>(camera)
            {
                ename_engine::camera::pose::reset_controller_motion(&mut controller);
            }
        }
    }
}

/// Absolute pose, or the plain `Transform` of a camera with no grid (an editor before any stage
/// has loaded).
pub(crate) fn read_pose(world: &World, camera: Entity) -> Option<(DVec3, DQuat)> {
    match grid::absolute_pose(world, camera) {
        Ok(pose) => Some(pose),
        Err(PositionError::NotOnGrid(_)) if world.get::<ChildOf>(camera).is_none() => world
            .get::<Transform>(camera)
            .map(|t| (t.translation.as_dvec3(), t.rotation.as_dquat())),
        Err(_) => None,
    }
}

pub(crate) fn write_pose(
    world: &mut World,
    camera: Entity,
    position: DVec3,
    rotation: DQuat,
) -> Result<(), CommandError> {
    match grid::set_absolute_pose(world, camera, position, rotation) {
        Ok(_) => Ok(()),
        Err(PositionError::NotOnGrid(_)) if world.get::<ChildOf>(camera).is_none() => {
            if let Some(mut transform) = world.get_mut::<Transform>(camera) {
                transform.translation = position.as_vec3();
                transform.rotation = rotation.as_quat();
            }
            Ok(())
        }
        Err(err) => Err(CommandError::failed("not_on_grid", err.to_string())),
    }
}

fn entity_position(world: &World, entity: Entity) -> Result<DVec3, CommandError> {
    match grid::absolute_position(world, entity) {
        Ok(found) => Ok(found.position),
        Err(PositionError::NotOnGrid(_)) if world.get::<ChildOf>(entity).is_none() => world
            .get::<Transform>(entity)
            .map(|t| t.translation.as_dvec3())
            .ok_or_else(|| {
                CommandError::failed("not_on_grid", format!("entity {entity} has no position"))
            }),
        Err(err) => Err(CommandError::failed("entity_not_found", err.to_string())),
    }
}

fn target_point(world: &World, target: Target) -> Result<DVec3, CommandError> {
    match target {
        Target::Point(point) => Ok(point),
        Target::Entity(entity) => entity_position(world, entity),
    }
}

fn towards(from: DVec3, to: DVec3) -> Result<(f64, f64), CommandError> {
    yaw_pitch_towards(from, to)
        .ok_or_else(|| invalid_pose("the point to face is the camera's own position"))
}

fn clamp_pitch(choice: CameraChoice, pitch: f64, notes: &mut Vec<String>) -> f64 {
    if choice != CameraChoice::EditorCamera || pitch.abs() <= MAX_EDITOR_PITCH_DEGREES {
        return pitch;
    }
    let clamped = pitch.clamp(-MAX_EDITOR_PITCH_DEGREES, MAX_EDITOR_PITCH_DEGREES);
    notes.push(format!(
        "pitch {} is past the editor camera's limit; it holds {clamped}",
        round2(pitch)
    ));
    clamped
}

fn finite(field: &str, value: f64) -> Result<f64, CommandError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid_pose(format!("`{field}` must be a finite number")))
    }
}

fn finite_vec(field: &str, [x, y, z]: [f64; 3]) -> Result<DVec3, CommandError> {
    let vec = DVec3::new(x, y, z);
    if vec.is_finite() {
        Ok(vec)
    } else {
        Err(invalid_pose(format!("`{field}` must be finite numbers")))
    }
}

fn invalid_pose(message: impl Into<String>) -> CommandError {
    CommandError::invalid("invalid_pose", message)
}

fn frame(world: &World) -> u32 {
    world
        .get_resource::<FrameCount>()
        .map_or(0, |count| count.0)
}

pub(crate) fn round2(value: f64) -> f64 {
    let rounded = (value * 100.0).round() / 100.0;
    // No "-0.0" in results.
    if rounded == 0.0 { 0.0 } else { rounded }
}

pub(crate) fn round_vec(vec: DVec3) -> [f64; 3] {
    [round2(vec.x), round2(vec.y), round2(vec.z)]
}

#[cfg(test)]
pub(crate) mod tests;
