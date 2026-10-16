//! `wait.settled`, `wait.activity` and [`SettleGate`], the settle step other job commands reuse.
//!
//! "Settled" means every considered probe reported idle for `stable_frames` consecutive frames:
//! work often starts a frame after its cause (a camera move seen by LOD selection next `Update`,
//! a pipeline queued a frame behind in the render world), so one idle sample is not enough.
//! Frames are real frames, which keep running while the world is paused.

use std::{collections::BTreeMap, time::Instant};

use bevy::{diagnostic::FrameCount, prelude::*};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    Activity, SettleWaiter,
    probe::{SettleSample, probe_names, run_probes},
};
use crate::command::{
    Call, CommandAppExt, CommandError, CommandScope, CommandSpec, JobHandle, JobWork, Jobs,
    NoParams, OnCancel,
};

const DEFAULT_STABLE_FRAMES: u32 = 5;
const MAX_STABLE_FRAMES: u32 = 600;
const DEFAULT_TIMEOUT_SECONDS: f64 = 30.0;
const MAX_TIMEOUT_SECONDS: f64 = 600.0;

/// How to settle: the params of `wait.settled`, and the `settle` field of commands that settle
/// before they work. `{}` means the defaults.
#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct SettleParams {
    /// Consecutive frames with every considered probe idle, 1 to 600. Defaults to 5.
    #[serde(default)]
    pub stable_frames: Option<u32>,
    /// Real seconds before giving up, up to 600. Defaults to 30.
    #[serde(default)]
    pub timeout_seconds: Option<f64>,
    /// The probes to consider, by name; all of them when left out. `[]` considers none, which
    /// makes the wait a plain count of `stable_frames` frames.
    #[serde(default)]
    pub probes: Option<Vec<String>>,
}

/// Waits on the work entity it is inserted on, then triggers [`Settled`] or [`SettleTimedOut`]
/// there and removes itself. It never touches a job: the feature that inserted it decides what
/// a timeout means.
#[derive(Component, Debug)]
#[require(SettleWaiter)]
pub struct SettleGate {
    stable_frames: u32,
    timeout_seconds: f64,
    /// `None` considers every probe.
    probes: Option<Vec<&'static str>>,
    started: Instant,
    frames: u32,
    stable_for: u32,
    busy: BTreeMap<&'static str, Activity>,
    paused_seen: bool,
}

impl SettleGate {
    /// Checks `params` against the probes this app registered. `unknown_probe` lists them.
    pub fn new(params: &SettleParams, world: &World) -> Result<Self, CommandError> {
        let stable_frames = params.stable_frames.unwrap_or(DEFAULT_STABLE_FRAMES);
        if !(1..=MAX_STABLE_FRAMES).contains(&stable_frames) {
            return Err(CommandError::invalid(
                "invalid_params",
                format!("`stable_frames` must be 1 to {MAX_STABLE_FRAMES}, not {stable_frames}"),
            ));
        }
        let timeout_seconds = params.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if !(timeout_seconds > 0.0 && timeout_seconds <= MAX_TIMEOUT_SECONDS) {
            return Err(CommandError::invalid(
                "invalid_params",
                format!(
                    "`timeout_seconds` must be above 0 and at most {MAX_TIMEOUT_SECONDS}, not \
                     {timeout_seconds}"
                ),
            ));
        }
        let probes = params
            .probes
            .as_ref()
            .map(|wanted| {
                let known = probe_names(world);
                wanted
                    .iter()
                    .map(|name| {
                        known
                            .iter()
                            .find(|known| **known == name.as_str())
                            .copied()
                            .ok_or_else(|| {
                                CommandError::invalid(
                                    "unknown_probe",
                                    format!(
                                        "no settle probe `{name}`; this build has: {}",
                                        known.join(", ")
                                    ),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        Ok(Self {
            stable_frames,
            timeout_seconds,
            probes,
            started: Instant::now(),
            frames: 0,
            stable_for: 0,
            busy: BTreeMap::new(),
            paused_seen: false,
        })
    }

    /// What a job reports while this gate waits.
    pub fn progress(&self) -> SettleProgress {
        SettleProgress {
            frames: self.frames,
            stable_for: self.stable_for,
            busy: self.busy.clone(),
        }
    }

    fn seconds(&self) -> f64 {
        round_ms(self.started.elapsed().as_secs_f64())
    }
}

/// A settle in progress.
#[derive(Serialize, Clone, Debug)]
pub struct SettleProgress {
    /// Frames sampled so far.
    pub frames: u32,
    /// Consecutive idle frames so far.
    pub stable_for: u32,
    /// The considered probes still busy, with their counters.
    pub busy: BTreeMap<&'static str, Activity>,
}

/// The gate's probes stayed idle for its `stable_frames`.
#[derive(EntityEvent, Clone, Debug)]
pub struct Settled {
    pub entity: Entity,
    pub frames: u32,
    /// Real seconds the settle took.
    pub seconds: f64,
}

/// The gate's timeout passed before it settled.
#[derive(EntityEvent, Clone, Debug)]
pub struct SettleTimedOut {
    pub entity: Entity,
    pub frames: u32,
    pub seconds: f64,
    /// What was still busy in the last frame. Empty when it was idle, but not for long enough.
    pub busy: BTreeMap<&'static str, Activity>,
    pub stable_for: u32,
    pub stable_frames: u32,
    /// The world was paused at some point during the wait.
    pub paused: bool,
}

impl SettleTimedOut {
    /// The `not_settled` failure of a command that settles before it works.
    pub fn error(&self) -> CommandError {
        let message = if self.busy.is_empty() {
            format!(
                "idle for only {} of {} frames after {} s",
                self.stable_for, self.stable_frames, self.seconds
            )
        } else {
            let busy: Vec<String> = self
                .busy
                .iter()
                .map(|(probe, activity)| format!("{probe} ({activity})"))
                .collect();
            format!("still busy after {} s: {}", self.seconds, busy.join(", "))
        };
        let error = CommandError::failed("not_settled", message);
        if self.paused {
            error.with_hint(
                "The world is paused; a probe whose work runs on virtual time cannot finish until \
                 `time_set` resumes it.",
            )
        } else {
            error
        }
    }
}

/// `wait.settled`'s work entity.
#[derive(Component)]
pub(super) struct SettleWait;

#[derive(Serialize, JsonSchema)]
struct SettledResult {
    /// Frames the settle took.
    frames: u32,
    /// Real seconds the settle took.
    seconds: f64,
    /// `FrameCount` when it settled; like `wait.until`'s, it may read one lower than `time_get`
    /// would for the same frame.
    frame: u32,
    /// Whether the world was paused when it settled.
    paused: bool,
}

#[derive(Serialize, JsonSchema)]
struct ActivityResult {
    frame: u32,
    paused: bool,
    /// Busy probes with their counters. Empty when everything is idle.
    #[schemars(with = "BTreeMap<String, BTreeMap<String, u32>>")]
    busy: BTreeMap<&'static str, Activity>,
    /// Every registered probe.
    probes: Vec<&'static str>,
}

pub(super) fn register(app: &mut App) {
    app.register_job_command(
        CommandSpec::new(
            "wait.settled",
            "Wait until loading, pipeline compiles and streaming have finished and stayed \
             finished for a few frames. Use before a capture or measurement instead of a sleep.",
        )
        .scope(CommandScope::Safe)
        .read_only(),
        settled,
    )
    .register_command(
        CommandSpec::new(
            "wait.activity",
            "What the settle probes report as still pending, right now.",
        )
        .scope(CommandScope::Safe)
        .read_only(),
        activity,
    );
}

fn settled(
    In(call): In<Call<SettleParams>>,
    world: &mut World,
) -> Result<JobHandle<SettledResult>, CommandError> {
    let gate = SettleGate::new(&call.params, world)?;
    let work = world.spawn((SettleWait, gate)).id();
    let job = match call.start_job(world, work, OnCancel::Despawn) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    let (on_settled, on_timeout) = (job.clone(), job.clone());
    world
        .entity_mut(work)
        .observe(
            move |done: On<Settled>,
                  mut jobs: ResMut<Jobs>,
                  frame: Option<Res<FrameCount>>,
                  time: Option<Res<Time<Virtual>>>,
                  mut commands: Commands| {
                jobs.finish(
                    &on_settled,
                    Ok(SettledResult {
                        frames: done.frames,
                        seconds: done.seconds,
                        frame: frame.map_or(0, |frame| frame.0),
                        paused: time.is_some_and(|time| time.is_paused()),
                    }),
                );
                commands.entity(done.entity).despawn();
            },
        )
        .observe(
            move |timeout: On<SettleTimedOut>, mut jobs: ResMut<Jobs>, mut commands: Commands| {
                jobs.finish(&on_timeout, Err(timeout.error()));
                commands.entity(timeout.entity).despawn();
            },
        );
    Ok(job)
}

fn activity(In(_): In<Call<NoParams>>, world: &mut World) -> Result<ActivityResult, CommandError> {
    let busy = run_probes(world);
    Ok(ActivityResult {
        frame: world
            .get_resource::<FrameCount>()
            .map_or(0, |frame| frame.0),
        paused: world
            .get_resource::<Time<Virtual>>()
            .is_some_and(Time::is_paused),
        busy,
        probes: probe_names(world),
    })
}

/// Advances every gate by this frame's sample, then opens or times out the ones that are done.
pub(super) fn resolve_gates(
    sample: Res<SettleSample>,
    time: Option<Res<Time<Virtual>>>,
    mut jobs: Option<ResMut<Jobs>>,
    mut gates: Query<(Entity, &mut SettleGate, Option<&JobWork>, Has<SettleWait>)>,
    mut commands: Commands,
) {
    let paused = time.is_some_and(|time| time.is_paused());
    for (entity, mut gate, work, is_wait) in &mut gates {
        gate.frames += 1;
        gate.paused_seen |= paused;
        let busy: BTreeMap<_, _> = sample
            .busy
            .iter()
            .filter(|(name, _)| gate.probes.as_ref().is_none_or(|p| p.contains(name)))
            .map(|(&name, activity)| (name, activity.clone()))
            .collect();
        gate.stable_for = if busy.is_empty() {
            gate.stable_for + 1
        } else {
            0
        };
        gate.busy = busy;
        let seconds = gate.seconds();
        if gate.stable_for >= gate.stable_frames {
            commands
                .entity(entity)
                .remove::<(SettleGate, SettleWaiter)>();
            commands.trigger(Settled {
                entity,
                frames: gate.frames,
                seconds,
            });
        } else if seconds >= gate.timeout_seconds {
            commands
                .entity(entity)
                .remove::<(SettleGate, SettleWaiter)>();
            commands.trigger(SettleTimedOut {
                entity,
                frames: gate.frames,
                seconds,
                busy: gate.busy.clone(),
                stable_for: gate.stable_for,
                stable_frames: gate.stable_frames,
                paused: gate.paused_seen,
            });
        } else if let (Some(work), Some(jobs)) = (work, jobs.as_mut()) {
            let progress = gate.progress();
            if is_wait {
                jobs.set_progress(work.job(), progress);
            } else {
                jobs.set_progress(work.job(), Phased::settling(progress));
            }
        }
    }
}

/// The progress of a command still in its settle step.
#[derive(Serialize)]
struct Phased {
    phase: &'static str,
    #[serde(flatten)]
    progress: SettleProgress,
}

impl Phased {
    fn settling(progress: SettleProgress) -> Self {
        Self {
            phase: "settling",
            progress,
        }
    }
}

fn round_ms(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        command::{Caller, CommandPlugin, CommandWorldExt, JobId, JobStatus, Reply},
        wait::{SettleAppExt, WaitPlugin},
    };

    #[derive(Resource, Default)]
    struct Pending(usize);

    fn pending_probe(pending: Res<Pending>) -> Activity {
        Activity::idle().pending("things", pending.0)
    }

    fn other_probe() -> Activity {
        Activity::idle().pending("forever", 1)
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((CommandPlugin, WaitPlugin))
            .init_resource::<Pending>()
            .register_settle_probe("test", pending_probe)
            .register_settle_probe("other", other_probe);
        app
    }

    fn call(app: &mut App, name: &str, params: Value) -> Result<Reply, CommandError> {
        app.world_mut().call_command(
            name,
            params,
            Caller {
                scope: CommandScope::Safe,
                cwd: None,
            },
        )
    }

    fn start(app: &mut App, params: Value) -> JobId {
        match call(app, "wait.settled", params).unwrap() {
            Reply::Job(job) => job,
            Reply::Done(value) => panic!("not a job: {value}"),
        }
    }

    fn status(app: &App, job: &JobId) -> JobStatus {
        app.world()
            .resource::<Jobs>()
            .status(job, CommandScope::Safe)
            .unwrap()
    }

    #[test]
    fn settles_after_stable_frames_idle_frames() {
        let mut app = app();
        let job = start(&mut app, json!({ "stable_frames": 3, "probes": ["test"] }));
        app.update();
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        assert_eq!(
            status(&app, &job).progress,
            Some(json!({ "frames": 2, "stable_for": 2, "busy": {} }))
        );
        app.update();
        let status = status(&app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        assert_eq!(status.result.unwrap()["frames"], 3);
        assert_eq!(
            app.world_mut()
                .query::<&SettleWaiter>()
                .iter(app.world())
                .count(),
            0,
            "the work entity is gone and sampling stops"
        );
    }

    #[test]
    fn a_probe_going_busy_again_resets_the_count() {
        let mut app = app();
        let job = start(&mut app, json!({ "stable_frames": 2, "probes": ["test"] }));
        app.update();
        app.world_mut().resource_mut::<Pending>().0 = 4;
        app.update();
        assert_eq!(
            status(&app, &job).progress,
            Some(json!({ "frames": 2, "stable_for": 0, "busy": { "test": { "things": 4 } } }))
        );
        app.world_mut().resource_mut::<Pending>().0 = 0;
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        app.update();
        assert_eq!(status(&app, &job).state, "succeeded");
    }

    #[test]
    fn a_timeout_fails_with_the_busy_counters() {
        let mut app = app();
        let job = start(&mut app, json!({ "timeout_seconds": 0.01 }));
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.update();
        let error = status(&app, &job).error.unwrap();
        assert_eq!(error.kind, "not_settled");
        assert!(
            error.message.contains("other (forever 1)"),
            "{}",
            error.message
        );
    }

    #[test]
    fn an_empty_probe_list_is_a_frame_count() {
        let mut app = app();
        app.world_mut().resource_mut::<Pending>().0 = 9;
        let job = start(&mut app, json!({ "probes": [], "stable_frames": 1 }));
        app.update();
        assert_eq!(status(&app, &job).state, "succeeded");
    }

    #[test]
    fn an_unknown_probe_lists_the_registered_ones() {
        let mut app = app();
        let err = call(&mut app, "wait.settled", json!({ "probes": ["moon"] })).unwrap_err();
        assert_eq!(err.kind, "unknown_probe");
        assert!(
            err.message.contains("assets, other, pipelines, test"),
            "{err}"
        );
        let err = call(&mut app, "wait.settled", json!({ "stable_frames": 0 })).unwrap_err();
        assert_eq!(err.kind, "invalid_params");
    }

    #[test]
    fn cancelling_despawns_the_wait() {
        let mut app = app();
        let job = start(&mut app, json!({}));
        let status = app
            .world_mut()
            .cancel_job(&job, CommandScope::Safe)
            .unwrap();
        assert_eq!(status.state, "cancelled");
        assert_eq!(
            app.world_mut()
                .query::<&SettleGate>()
                .iter(app.world())
                .count(),
            0
        );
    }

    #[test]
    fn a_gate_times_out_without_touching_a_job() {
        #[derive(Resource, Default)]
        struct TimedOut(Option<String>);

        let mut app = app();
        app.init_resource::<TimedOut>();
        let gate = SettleGate::new(
            &SettleParams {
                timeout_seconds: Some(0.01),
                ..default()
            },
            app.world(),
        )
        .unwrap();
        let work = app.world_mut().spawn(gate).id();
        app.world_mut().entity_mut(work).observe(
            |timeout: On<SettleTimedOut>, mut seen: ResMut<TimedOut>| {
                seen.0 = Some(timeout.error().message);
            },
        );
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.update();
        let message = app.world().resource::<TimedOut>().0.clone().unwrap();
        assert!(message.contains("forever 1"), "{message}");
        assert!(app.world().get_entity(work).is_ok(), "the consumer decides");
        assert!(app.world().get::<SettleGate>(work).is_none());
        assert!(
            app.world()
                .resource::<Jobs>()
                .list(CommandScope::Safe)
                .is_empty()
        );
    }

    #[test]
    fn activity_reports_busy_probes_now() {
        let mut app = app();
        app.world_mut().resource_mut::<Pending>().0 = 2;
        let Reply::Done(value) = call(&mut app, "wait.activity", json!({})).unwrap() else {
            panic!("not instant");
        };
        assert_eq!(
            value["busy"],
            json!({ "other": { "forever": 1 }, "test": { "things": 2 } })
        );
        assert_eq!(
            value["probes"],
            json!(["assets", "other", "pipelines", "test"])
        );
    }
}
