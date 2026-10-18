//! `capture.fly_through`: a camera moved along a path in N steps, captured as stills or a video.
//!
//! Each step's pose is a function of its index, not of time, so a video shows the same motion at
//! any frame rate. Positions are absolute f64 metres; yaw turns the short way.

use std::path::PathBuf;

use bevy::{math::DVec3, prelude::*};
use ename_engine::{
    camera::{
        ground::GroundGuardMode,
        pose::{CameraPose, rotation_from_yaw_pitch},
        viewpoint::{self, Viewpoint},
    },
    command::{Call, CommandAppExt, CommandError, CommandSpec, JobHandle},
    video::{check_frame_rate, check_video_path},
    wait::SettleParams,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    CommonParams, Output, SequencePlan, SequenceResult, SequenceSource, Step, start_sequence,
};
use crate::viewpoints::viewpoints_file;

pub(super) fn register(app: &mut App) {
    app.register_job_command(
        CommandSpec::new(
            "capture.fly_through",
            "Move a camera along a path in `steps` even steps, between viewpoints, along a \
             direction or through poses, and capture each step as a still or one video frame. \
             For bugs that only show in motion.",
        ),
        fly_through,
    );
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FlyParams {
    /// The camera to move and capture: "editor_camera" (Scene View) or "game_camera" (Game
    /// View). The view is shown for the run and switched back afterwards.
    source: SequenceSource,
    /// Where the camera goes.
    path: FlyPath,
    /// Poses along the path, the first and last included. 1 to 10000.
    steps: u32,
    /// Stills (`{"stills": {"dir": ...}}`, one PNG per step) or a video (`{"video": {"path":
    /// ..., "frame_rate": 30}}`, one frame per step).
    record: Record,
    /// Stills: settle before each capture, as `wait.settled` does; `{}` uses its defaults; a step
    /// that does not settle in time is listed under `failed`. Video: settle once at the first
    /// pose before recording. Left out, nothing waits, so streaming shows as a moving camera
    /// would see it.
    #[serde(default)]
    settle: Option<SettleParams>,
    /// Pause virtual time for the run. Defaults to true.
    #[serde(default)]
    freeze_time: Option<bool>,
    /// Put the camera back where it was afterwards. Defaults to true.
    #[serde(default)]
    restore: Option<bool>,
    /// Keep the editor's debug overlays, statistics panel, transform gizmo and pick marker in the
    /// captures.
    /// Defaults to false.
    #[serde(default)]
    overlays: Option<bool>,
    /// Apply the facets of a named `from` viewpoint before the first step. Defaults to true.
    #[serde(default)]
    facets: Option<bool>,
    /// Which ground keeps the camera clear of it, as on `camera.look_at`. Defaults to "none", so
    /// the camera follows the path exactly.
    #[serde(default)]
    ground: Option<GroundGuardMode>,
    /// Stills: reuse `dir`, deleting the files its old manifest.json lists first.
    #[serde(default)]
    replace: bool,
}

/// One of three path forms.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(untagged)]
enum FlyPath {
    Between(Between),
    Along(Along),
    Poses(Poses),
}

/// From one pose to another: position in a straight line, yaw the short way, pitch linearly.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Between {
    from: PoseRef,
    to: PoseRef,
}

/// From a pose along a direction, orientation fixed.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Along {
    from: PoseRef,
    /// "forward" and "right" are the camera's, "up" is the world's +Y; or an absolute `[x, y,
    /// z]` direction.
    along: Direction,
    /// Metres from the first pose to the last.
    distance: f64,
}

/// Through inline poses, steps spaced evenly by distance.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Poses {
    poses: Vec<InlinePose>,
}

/// A viewpoint name, or an inline pose.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(untagged)]
enum PoseRef {
    Viewpoint(String),
    Pose(InlinePose),
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
struct InlinePose {
    /// Absolute metres, Y up.
    position: [f64; 3],
    /// Degrees: 0 looks along -Z, 90 along +X.
    yaw: f64,
    /// Degrees, positive up.
    pitch: f64,
}

impl From<InlinePose> for CameraPose {
    fn from(pose: InlinePose) -> Self {
        Self {
            position: DVec3::from_array(pose.position),
            yaw: pose.yaw,
            pitch: pose.pitch,
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug)]
#[serde(untagged)]
enum Direction {
    Named(Axis),
    Vector([f64; 3]),
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Axis {
    Forward,
    Right,
    Up,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Stills {
        /// The output directory, relative to the caller's working directory. Must be absent or
        /// empty, unless `replace`. Files are `0.png`, `1.png`, ..., zero-padded to the last
        /// index's width.
        dir: PathBuf,
    },
    Video {
        /// An `.mp4` or `.mkv` file, relative to the caller's working directory; replaced if it
        /// exists. The manifest goes beside it as `<path>.manifest.json`.
        path: PathBuf,
        /// Output frames per second, 1 to 240. Defaults to 30.
        #[serde(default)]
        frame_rate: Option<u32>,
    },
}

fn fly_through(
    In(call): In<Call<FlyParams>>,
    world: &mut World,
) -> Result<JobHandle<SequenceResult>, CommandError> {
    let params = &call.params;
    let steps = params.steps as usize;
    if !(1..=super::MAX_STEPS).contains(&steps) {
        return Err(invalid(format!(
            "`steps` must be 1 to {}, not {steps}",
            super::MAX_STEPS
        )));
    }
    let mut resolver = Resolver::new(world);
    let (poses, first) = match &params.path {
        FlyPath::Between(between) => {
            let (from, first) = resolver.resolve(&between.from)?;
            let (to, _) = resolver.resolve(&between.to)?;
            (between_poses(from, to, steps), first)
        }
        FlyPath::Along(along) => {
            let (from, first) = resolver.resolve(&along.from)?;
            (
                along_poses(from, along.along, along.distance, steps)?,
                first,
            )
        }
        FlyPath::Poses(list) => (polyline_poses(&list.poses, steps)?, None),
    };
    for pose in &poses {
        if !(pose.position.is_finite() && pose.yaw.is_finite() && pose.pitch.is_finite()) {
            return Err(invalid("the path's poses must be finite numbers"));
        }
    }
    let output = match &params.record {
        Record::Stills { dir } => Output::Stills {
            dir: call.resolve_path(dir)?,
        },
        Record::Video { path, frame_rate } => {
            let path = call.resolve_path(path)?;
            check_video_path(&path).map_err(|err| invalid_video(&err))?;
            let frame_rate =
                check_frame_rate(frame_rate.unwrap_or(30)).map_err(|err| invalid_video(&err))?;
            Output::Video { path, frame_rate }
        }
    };
    let width = (steps - 1).to_string().len();
    let stills = matches!(output, Output::Stills { .. });
    let mut steps: Vec<Step> = poses
        .into_iter()
        .enumerate()
        .map(|(index, pose)| Step {
            pose,
            viewpoint: None,
            file: stills.then(|| format!("{index:0width$}.png")),
            facets: None,
        })
        .collect();
    if let Some(viewpoint) = first {
        steps[0].viewpoint = Some(viewpoint.name.clone());
        if params.facets.unwrap_or(true) {
            steps[0].facets = Some(viewpoint);
        }
    }
    let plan = SequencePlan {
        common: CommonParams {
            source: params.source,
            settle: params.settle.clone(),
            freeze_time: params.freeze_time.unwrap_or(true),
            restore: params.restore.unwrap_or(true),
            overlays: params.overlays.unwrap_or(false),
        },
        steps,
        output,
        ground: params.ground.unwrap_or(GroundGuardMode::None),
        replace: params.replace,
    };
    start_sequence(world, &call, plan)
}

/// Looks viewpoint names up, reading the file at most once.
struct Resolver<'w> {
    world: &'w World,
    file: Option<viewpoint::ViewpointFile>,
}

impl<'w> Resolver<'w> {
    fn new(world: &'w World) -> Self {
        Self { world, file: None }
    }

    /// The pose, and the viewpoint when it is a named one.
    fn resolve(
        &mut self,
        reference: &PoseRef,
    ) -> Result<(CameraPose, Option<Viewpoint>), CommandError> {
        let name = match reference {
            PoseRef::Pose(pose) => return Ok(((*pose).into(), None)),
            PoseRef::Viewpoint(name) => name.trim(),
        };
        let path = viewpoints_file(self.world)?;
        if self.file.is_none() {
            self.file = Some(viewpoint::load(&path)?.unwrap_or_default());
        }
        let file = self.file.as_ref().expect("loaded above");
        let found = file
            .viewpoints
            .iter()
            .find(|v| v.name == name)
            .ok_or_else(|| {
                let known: Vec<&str> = file.viewpoints.iter().map(|v| v.name.as_str()).collect();
                CommandError::failed(
                    "unknown_viewpoint",
                    format!(
                        "{} has no viewpoint `{name}`; it has: {}",
                        path.display(),
                        if known.is_empty() {
                            "none".to_owned()
                        } else {
                            known.join(", ")
                        }
                    ),
                )
            })?;
        Ok((found.pose(), Some(found.clone())))
    }
}

/// `t` of step `index` of `steps`: 0 for the first, 1 for the last.
fn fraction(index: usize, steps: usize) -> f64 {
    if steps <= 1 {
        0.0
    } else {
        index as f64 / (steps - 1) as f64
    }
}

/// Position in a straight line, yaw the short way round, pitch linearly.
fn lerp_pose(from: CameraPose, to: CameraPose, t: f64) -> CameraPose {
    let mut turn = (to.yaw - from.yaw).rem_euclid(360.0);
    if turn > 180.0 {
        turn -= 360.0;
    }
    let mut yaw = (from.yaw + turn * t).rem_euclid(360.0);
    if yaw > 180.0 {
        yaw -= 360.0;
    }
    CameraPose {
        position: from.position.lerp(to.position, t),
        yaw,
        pitch: from.pitch + (to.pitch - from.pitch) * t,
    }
}

fn between_poses(from: CameraPose, to: CameraPose, steps: usize) -> Vec<CameraPose> {
    (0..steps)
        .map(|index| lerp_pose(from, to, fraction(index, steps)))
        .collect()
}

fn along_poses(
    from: CameraPose,
    along: Direction,
    distance: f64,
    steps: usize,
) -> Result<Vec<CameraPose>, CommandError> {
    if !distance.is_finite() {
        return Err(invalid("`distance` must be a finite number of metres"));
    }
    let rotation = rotation_from_yaw_pitch(from.yaw, from.pitch);
    let direction = match along {
        Direction::Named(Axis::Forward) => rotation * DVec3::NEG_Z,
        Direction::Named(Axis::Right) => rotation * DVec3::X,
        Direction::Named(Axis::Up) => DVec3::Y,
        Direction::Vector(vector) => DVec3::from_array(vector)
            .try_normalize()
            .ok_or_else(|| invalid("`along` must be a finite, non-zero direction"))?,
    };
    Ok((0..steps)
        .map(|index| CameraPose {
            position: from.position + direction * distance * fraction(index, steps),
            ..from
        })
        .collect())
}

/// Steps spaced evenly by distance along the polyline; by index when every pose is at one point.
fn polyline_poses(poses: &[InlinePose], steps: usize) -> Result<Vec<CameraPose>, CommandError> {
    if poses.len() < 2 {
        return Err(invalid("`poses` needs at least two poses"));
    }
    let poses: Vec<CameraPose> = poses.iter().map(|&pose| pose.into()).collect();
    let lengths: Vec<f64> = poses
        .windows(2)
        .map(|pair| pair[0].position.distance(pair[1].position))
        .collect();
    let total: f64 = lengths.iter().sum();
    if !total.is_finite() {
        return Err(invalid("the path's poses must be finite numbers"));
    }
    let segments = lengths.len();
    Ok((0..steps)
        .map(|index| {
            // The ends are the given poses, whatever zero-length segments sit next to them.
            if index == 0 {
                return poses[0];
            }
            if index == steps - 1 {
                return poses[poses.len() - 1];
            }
            let t = fraction(index, steps);
            if total <= 0.0 {
                let at = t * segments as f64;
                let segment = (at.floor() as usize).min(segments - 1);
                return lerp_pose(poses[segment], poses[segment + 1], at - segment as f64);
            }
            let mut left = t * total;
            for (segment, &length) in lengths.iter().enumerate() {
                if left <= length || segment == segments - 1 {
                    let u = if length > 0.0 {
                        (left / length).min(1.0)
                    } else {
                        1.0
                    };
                    return lerp_pose(poses[segment], poses[segment + 1], u);
                }
                left -= length;
            }
            unreachable!("the last segment always returns")
        })
        .collect())
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::invalid("invalid_params", message)
}

fn invalid_video(error: &ename_engine::video::VideoError) -> CommandError {
    use ename_engine::capture::CaptureError as _;
    CommandError::invalid(error.kind(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(position: [f64; 3], yaw: f64, pitch: f64) -> CameraPose {
        CameraPose {
            position: DVec3::from_array(position),
            yaw,
            pitch,
        }
    }

    #[test]
    fn yaw_turns_the_short_way() {
        let poses = between_poses(pose([0.0; 3], 170.0, 0.0), pose([0.0; 3], -170.0, 10.0), 3);
        assert_eq!(poses[1].yaw.abs(), 180.0);
        assert!((poses[2].yaw + 170.0).abs() < 1e-9, "{}", poses[2].yaw);
        assert!((poses[1].pitch - 5.0).abs() < 1e-9);
        let one = between_poses(pose([1.0; 3], 0.0, 0.0), pose([9.0; 3], 90.0, 0.0), 1);
        assert_eq!(one[0].position, DVec3::ONE);
    }

    #[test]
    fn along_forward_keeps_the_orientation() {
        let poses = along_poses(
            pose([0.0, 5.0, 0.0], 90.0, 0.0),
            Direction::Named(Axis::Forward),
            100.0,
            5,
        )
        .unwrap();
        assert_eq!(poses.len(), 5);
        // Yaw 90 looks along +X.
        assert!((poses[4].position - DVec3::new(100.0, 5.0, 0.0)).length() < 1e-6);
        assert!((poses[1].position.x - 25.0).abs() < 1e-6);
        assert!(poses.iter().all(|p| p.yaw == 90.0));
        assert!(
            along_poses(
                pose([0.0; 3], 0.0, 0.0),
                Direction::Vector([0.0; 3]),
                1.0,
                2
            )
            .is_err()
        );
    }

    #[test]
    fn a_polyline_is_spaced_by_distance() {
        let corner = |position, yaw| InlinePose {
            position,
            yaw,
            pitch: 0.0,
        };
        // 10 m then 30 m: the step at a quarter of the way is the first corner.
        let poses = polyline_poses(
            &[
                corner([0.0; 3], 0.0),
                corner([10.0, 0.0, 0.0], 0.0),
                corner([10.0, 0.0, 30.0], 90.0),
            ],
            5,
        )
        .unwrap();
        assert!((poses[1].position - DVec3::new(10.0, 0.0, 0.0)).length() < 1e-9);
        assert!((poses[2].position - DVec3::new(10.0, 0.0, 10.0)).length() < 1e-9);
        assert!((poses[4].position - DVec3::new(10.0, 0.0, 30.0)).length() < 1e-9);
        assert!((poses[4].yaw - 90.0).abs() < 1e-9);
        assert!(polyline_poses(&[corner([0.0; 3], 0.0)], 3).is_err());
        let still = polyline_poses(&[corner([1.0; 3], 0.0), corner([1.0; 3], 90.0)], 3).unwrap();
        assert!((still[1].yaw - 45.0).abs() < 1e-9);
        // A turn on the spot at the end keeps its final orientation.
        let turn = polyline_poses(
            &[
                corner([0.0; 3], 0.0),
                corner([10.0, 0.0, 0.0], 0.0),
                corner([10.0, 0.0, 0.0], 90.0),
            ],
            3,
        )
        .unwrap();
        assert_eq!(turn[2].yaw, 90.0);
    }
}
