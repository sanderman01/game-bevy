//! `wait.until`: a job that finishes once one condition holds.
//!
//! Conditions are evaluated every frame in [`WaitSystems::Resolve`](super::WaitSystems), after
//! the time controls counted the frame, so a condition met in the last stepped frame still
//! succeeds. `time: "run"` resumes the world when the job starts and pauses it in the frame the
//! job ends, by any path: [`UntilWait`]'s remove hook covers cancel and despawn.

use std::time::Instant;

use bevy::{
    diagnostic::FrameCount,
    ecs::{lifecycle::HookContext, world::DeferredWorld},
    prelude::*,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::predicate::{
    ComponentCondition, PositionCheck, PositionCondition, Predicate, ResourceCondition,
};
use super::rest::{AtRest, AtRestCondition};
use crate::{
    command::{Call, CommandAppExt, CommandError, CommandSpec, JobHandle, Jobs, OnCancel},
    log::{CaptureLevel, LogBuffer, LogFilter},
    time::TimeControl,
};

const DEFAULT_TIMEOUT_SECONDS: f64 = 30.0;
const MAX_TIMEOUT_SECONDS: f64 = 600.0;
const MAX_HOLD_FRAMES: u32 = 600;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UntilParams {
    /// What to wait for. Exactly one key: {"frames": 30} real frames; {"virtual_seconds": 2.0}
    /// of game time; {"log": {"message_contains": "baked"}} a new log line; {"component": ..},
    /// {"resource": ..} or {"position": ..} a predicate on reflected state.
    condition: Condition,
    /// Consecutive frames the condition must hold, 1 to 600. Defaults to 1. Not for `frames`,
    /// `virtual_seconds` or `log`.
    #[serde(default)]
    hold_frames: Option<u32>,
    /// Real seconds before the job fails `timed_out`, up to 600. Defaults to 30.
    #[serde(default)]
    timeout_seconds: Option<f64>,
    /// "leave" (default) observes. "run" resumes the world when the wait starts and pauses it in
    /// the frame the wait ends, however it ends.
    #[serde(default)]
    time: TimeMode,
}

#[derive(Deserialize, JsonSchema, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum TimeMode {
    #[default]
    Leave,
    Run,
}

/// One condition, its kind as the only key.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Condition {
    /// Real frames since the job started. Progresses while paused.
    Frames(u32),
    /// Seconds of `Time<Virtual>`. Needs the world running.
    VirtualSeconds(f64),
    /// A log entry newer than `after_sequence` (default: the newest when the job starts) that
    /// matches, with the same filters as `log_get_entries`.
    Log(LogCondition),
    /// A component of one entity, compared with `op`:
    /// {"name": "Crate", "component": "LinearVelocity", "path": ".0.y", "op": "gt", "value": -0.1}.
    Component(ComponentCondition),
    /// A resource, compared with `op`: {"resource": "...", "path": "...", "op": "eq", "value": ..}.
    Resource(ResourceCondition),
    /// An entity's absolute position along one axis, metres, Y up:
    /// {"name": "Crate", "axis": "y", "op": "lt", "value": 2.0}.
    Position(PositionCondition),
    /// A dynamic rigid body that stays still for `hold_seconds` of virtual time, or sleeps:
    /// {"name": "Crate", "speed_below": 0.05, "hold_seconds": 0.25}. Needs the world running.
    AtRest(AtRestCondition),
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct LogCondition {
    /// Case-insensitive substring of the message.
    #[serde(default)]
    message_contains: Option<String>,
    /// Case-insensitive substring of the emitting module path.
    #[serde(default)]
    target_contains: Option<String>,
    /// Least severe level: TRACE, DEBUG, INFO, WARN or ERROR.
    #[serde(default)]
    min_level: Option<String>,
    /// Only entries after this sequence number. Defaults to the newest entry when the job starts.
    #[serde(default)]
    after_sequence: Option<u64>,
}

/// A condition being evaluated, with what it needs between frames.
enum Evaluation {
    Frames(u32),
    VirtualSeconds { seconds: f64 },
    Log(LogFilter),
    Predicate(Predicate),
    Position(PositionCheck),
    AtRest(AtRest),
}

impl Evaluation {
    fn needs_virtual_time(&self) -> bool {
        matches!(self, Self::VirtualSeconds { .. } | Self::AtRest(_))
    }

    fn holds(&self) -> bool {
        !matches!(
            self,
            Self::Frames(_) | Self::VirtualSeconds { .. } | Self::Log(_)
        )
    }
}

/// What one frame's check saw.
pub(super) struct Check {
    /// The value to report when met.
    pub(super) met: Option<Value>,
    /// The last value seen, for progress and the timeout message.
    pub(super) last: Value,
}

/// A running `wait.until`.
#[derive(Component)]
#[component(on_remove = until_removed)]
pub(super) struct UntilWait {
    job: JobHandle<UntilResult>,
    evaluation: Option<Evaluation>,
    hold_frames: u32,
    held: u32,
    timeout_seconds: f64,
    started: Instant,
    frames: u32,
    virtual_start: f64,
    last: Value,
    runs_time: bool,
}

#[derive(Serialize, JsonSchema)]
struct UntilResult {
    /// What met the condition: the value, the log entry, or the counts.
    met: Value,
    /// Frames the wait took.
    frames: u32,
    /// Real seconds the wait took.
    seconds: f64,
    /// Virtual seconds that passed during the wait.
    virtual_seconds: f64,
    /// `FrameCount` when it ended. Bevy bumps it in `Last` with no set to order against, so it
    /// may read one lower than `time_get` would for the same frame.
    frame: u32,
    /// Whether the world is paused now.
    paused: bool,
}

pub(super) fn register(app: &mut App) {
    app.register_job_command(
        CommandSpec::new(
            "wait.until",
            "Wait until one condition holds: frames, virtual seconds, a log line, a component or \
             resource value, or a position. \
             time=run runs the world for the wait and pauses it in the frame it ends.",
        ),
        until,
    );
}

fn until(
    In(call): In<Call<UntilParams>>,
    world: &mut World,
) -> Result<JobHandle<UntilResult>, CommandError> {
    let params = &call.params;
    let timeout_seconds = params.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    if !(timeout_seconds > 0.0 && timeout_seconds <= MAX_TIMEOUT_SECONDS) {
        return Err(invalid(format!(
            "`timeout_seconds` must be above 0 and at most {MAX_TIMEOUT_SECONDS}, not \
             {timeout_seconds}"
        )));
    }
    let evaluation = evaluation(&params.condition, world)?;
    let hold_frames = match params.hold_frames {
        None => 1,
        Some(_) if !evaluation.holds() => {
            return Err(invalid(
                "`hold_frames` does not apply to `frames`, `virtual_seconds` or `log`",
            ));
        }
        Some(frames @ 1..=MAX_HOLD_FRAMES) => frames,
        Some(frames) => {
            return Err(invalid(format!(
                "`hold_frames` must be 1 to {MAX_HOLD_FRAMES}, not {frames}"
            )));
        }
    };
    let runs_time = params.time == TimeMode::Run;
    if runs_time && !has_time_control(world) {
        return Err(CommandError::invalid(
            "invalid_params",
            "`time: \"run\"` needs the time controls, which this app does not have",
        ));
    }
    if !runs_time && evaluation.needs_virtual_time() && frozen(world) {
        return Err(world_paused());
    }
    let virtual_start = virtual_elapsed(world);
    let work = world.spawn_empty().id();
    let job = match call.start_job(world, work, OnCancel::Despawn) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    // Only once the job exists, so a refused call leaves time as it was.
    if runs_time {
        set_paused(world, false);
    }
    world.entity_mut(work).insert(UntilWait {
        job: job.clone(),
        evaluation: Some(evaluation),
        hold_frames,
        held: 0,
        timeout_seconds,
        started: Instant::now(),
        frames: 0,
        virtual_start,
        last: Value::Null,
        runs_time,
    });
    Ok(job)
}

fn evaluation(condition: &Condition, world: &mut World) -> Result<Evaluation, CommandError> {
    Ok(match condition {
        Condition::Frames(frames) => Evaluation::Frames(*frames),
        Condition::Component(component) => {
            Evaluation::Predicate(Predicate::component(component, world)?)
        }
        Condition::Resource(resource) => {
            Evaluation::Predicate(Predicate::resource(resource, world)?)
        }
        Condition::Position(position) => Evaluation::Position(PositionCheck::new(position, world)?),
        Condition::AtRest(rest) => Evaluation::AtRest(AtRest::new(rest, world)?),
        Condition::VirtualSeconds(seconds) if seconds.is_finite() && *seconds >= 0.0 => {
            Evaluation::VirtualSeconds { seconds: *seconds }
        }
        Condition::VirtualSeconds(seconds) => {
            return Err(invalid(format!(
                "`virtual_seconds` must be a number of seconds, not {seconds}"
            )));
        }
        Condition::Log(log) => {
            let mut filter = LogFilter::new(
                log.min_level.as_deref(),
                log.target_contains.as_deref(),
                log.message_contains.as_deref(),
                log.after_sequence,
            )
            .map_err(|err| invalid(err.to_string()))?;
            if let (Some(min), Some(capture)) = (filter.min_level, world.get_resource::<CaptureLevel>())
                // More verbose compares greater.
                && min > capture.get()
            {
                return Err(CommandError::invalid(
                    "level_not_captured",
                    format!(
                        "the log buffer captures {} and above, so no {min} entry can arrive",
                        capture.get()
                    ),
                )
                .with_hint("Use a `min_level` the buffer captures, or raise it in the console."));
            }
            if filter.after_sequence.is_none() {
                filter.after_sequence = world
                    .get_resource::<LogBuffer>()
                    .and_then(|buffer| buffer.read(|view| view.sequence_range()))
                    .map(|(_, newest)| newest);
            }
            Evaluation::Log(filter)
        }
    })
}

/// Evaluates every `wait.until` against this frame.
pub(super) fn resolve_untils(world: &mut World) {
    let waits: Vec<Entity> = world
        .query_filtered::<Entity, With<UntilWait>>()
        .iter(world)
        .collect();
    for entity in waits {
        let Some(mut evaluation) = world
            .get_mut::<UntilWait>(entity)
            .and_then(|mut wait| wait.evaluation.take())
        else {
            continue;
        };
        let virtual_now = virtual_elapsed(world);
        let (frames, virtual_start) = {
            let Some(mut wait) = world.get_mut::<UntilWait>(entity) else {
                continue;
            };
            wait.frames += 1;
            (wait.frames, wait.virtual_start)
        };
        let check = check(&mut evaluation, world, frames, virtual_now, virtual_start);
        let frozen = frozen(world);
        let paused_now = world_is_paused(world);
        let Some(mut wait) = world.get_mut::<UntilWait>(entity) else {
            continue;
        };
        let outcome = match check {
            Err(err) => Some(Err(err)),
            Ok(check) => {
                wait.last = check.last;
                wait.held = if check.met.is_some() {
                    wait.held + 1
                } else {
                    0
                };
                match check.met {
                    Some(met) if wait.held >= wait.hold_frames => Some(Ok(met)),
                    _ if evaluation.needs_virtual_time() && frozen => Some(Err(world_paused())),
                    _ if wait.started.elapsed().as_secs_f64() >= wait.timeout_seconds => {
                        Some(Err(CommandError::failed(
                            "timed_out",
                            format!(
                                "the condition did not hold within {} s; last seen: {}",
                                wait.timeout_seconds, wait.last
                            ),
                        )))
                    }
                    _ => None,
                }
            }
        };
        let Some(outcome) = outcome else {
            wait.evaluation = Some(evaluation);
            let (job, progress) = (
                wait.job.clone(),
                json!({
                    "frames": wait.frames,
                    "virtual_seconds": round_ms(virtual_now - virtual_start),
                    "last": wait.last,
                    "paused": paused_now,
                }),
            );
            if let Some(mut jobs) = world.get_resource_mut::<Jobs>() {
                jobs.set_progress(job.id(), progress);
            }
            continue;
        };
        let job = wait.job.clone();
        let seconds = round_ms(wait.started.elapsed().as_secs_f64());
        let runs_time = wait.runs_time;
        if runs_time {
            set_paused(world, true);
        }
        let result = outcome.map(|met| UntilResult {
            met,
            frames,
            seconds,
            virtual_seconds: round_ms(virtual_elapsed(world) - virtual_start),
            frame: world
                .get_resource::<FrameCount>()
                .map_or(0, |frame| frame.0),
            paused: world_is_paused(world),
        });
        if let Some(mut jobs) = world.get_resource_mut::<Jobs>() {
            jobs.finish(&job, result);
        }
        world.despawn(entity);
    }
}

fn check(
    evaluation: &mut Evaluation,
    world: &mut World,
    frames: u32,
    virtual_now: f64,
    virtual_start: f64,
) -> Result<Check, CommandError> {
    let virtual_seconds = virtual_now - virtual_start;
    Ok(match evaluation {
        Evaluation::Frames(target) => Check {
            met: (frames >= *target).then(|| json!({ "frames": frames })),
            last: json!(frames),
        },
        Evaluation::VirtualSeconds { seconds } => Check {
            met: (virtual_seconds >= *seconds)
                .then(|| json!({ "virtual_seconds": round_ms(virtual_seconds) })),
            last: json!(round_ms(virtual_seconds)),
        },
        Evaluation::Predicate(predicate) => predicate.check(world)?,
        Evaluation::Position(position) => position.check(world)?,
        Evaluation::AtRest(rest) => rest.check(world, virtual_now)?,
        Evaluation::Log(filter) => {
            let Some(buffer) = world.get_resource::<LogBuffer>() else {
                return Ok(Check {
                    met: None,
                    last: Value::Null,
                });
            };
            let (found, newest) = buffer.read(|view| {
                let Some((oldest, newest)) = view.sequence_range() else {
                    return (None, None);
                };
                // Only entries not looked at yet: sequences are contiguous, so this is by index.
                let first = filter
                    .after_sequence
                    .map_or(oldest, |after| after.saturating_add(1))
                    .max(oldest);
                let found = (first..=newest)
                    .filter_map(|sequence| view.by_sequence(sequence))
                    .find(|entry| filter.matches(entry))
                    .map(|entry| {
                        json!({
                            "sequence": entry.sequence,
                            "level": entry.level.as_str(),
                            "target": entry.target_name,
                            "message": entry.message,
                        })
                    });
                (found, Some(newest))
            });
            // Next frame starts after the newest entry seen.
            if found.is_none() && newest.is_some() {
                filter.after_sequence = newest;
            }
            Check {
                met: found,
                last: json!({ "after_sequence": filter.after_sequence }),
            }
        }
    })
}

fn until_removed(mut world: DeferredWorld, context: HookContext) {
    if world
        .get::<UntilWait>(context.entity)
        .is_some_and(|wait| wait.runs_time)
    {
        world
            .commands()
            .queue(|world: &mut World| set_paused(world, true));
    }
}

/// Does nothing in an app without the time controls; `until` refuses `time: "run"` there.
fn set_paused(world: &mut World, paused: bool) {
    if !has_time_control(world) {
        return;
    }
    world.resource_scope(|world, mut control: Mut<TimeControl>| {
        let mut time = world.resource_mut::<Time<Virtual>>();
        if paused {
            control.pause(&mut time);
        } else {
            control.resume(&mut time);
        }
    });
}

fn has_time_control(world: &World) -> bool {
    world.contains_resource::<TimeControl>() && world.contains_resource::<Time<Virtual>>()
}

/// Paused with no step in flight: virtual time cannot advance until someone resumes it.
fn frozen(world: &World) -> bool {
    world_is_paused(world)
        && world
            .get_resource::<TimeControl>()
            .is_none_or(|control| control.steps_remaining().is_none())
}

fn world_is_paused(world: &World) -> bool {
    world
        .get_resource::<Time<Virtual>>()
        .is_some_and(Time::is_paused)
}

fn virtual_elapsed(world: &World) -> f64 {
    world
        .get_resource::<Time<Virtual>>()
        .map_or(0.0, Time::elapsed_secs_f64)
}

fn world_paused() -> CommandError {
    CommandError::failed(
        "world_paused",
        "the world is paused, and this condition needs virtual time to advance",
    )
    .with_hint("Call `time_set` with resume or step, or pass time=run.")
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::invalid("invalid_params", message)
}

fn round_ms(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::{log::Level, time::TimePlugin};

    use super::*;
    use crate::{
        command::{Caller, CommandPlugin, CommandScope, CommandWorldExt, JobId, JobStatus, Reply},
        time::TimeControlPlugin,
        wait::WaitPlugin,
    };

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((TimePlugin, TimeControlPlugin, CommandPlugin, WaitPlugin))
            .init_resource::<LogBuffer>()
            .init_resource::<FrameCount>();
        // Virtual time advances by a fixed 1/60 s per frame.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_secs_f64(1.0 / 60.0),
        ));
        app.update();
        app
    }

    fn call(app: &mut App, params: Value) -> Result<JobId, CommandError> {
        match app.world_mut().call_command(
            "wait.until",
            params,
            Caller {
                scope: CommandScope::Development,
                cwd: None,
            },
        )? {
            Reply::Job(job) => Ok(job),
            Reply::Done(value) => panic!("not a job: {value}"),
        }
    }

    fn status(app: &App, job: &JobId) -> JobStatus {
        app.world()
            .resource::<Jobs>()
            .status(job, CommandScope::Development)
            .unwrap()
    }

    fn pause(app: &mut App, paused: bool) {
        set_paused(app.world_mut(), paused);
    }

    fn paused(app: &App) -> bool {
        world_is_paused(app.world())
    }

    #[test]
    fn frames_while_paused_succeeds() {
        let mut app = app();
        pause(&mut app, true);
        let job = call(&mut app, json!({ "condition": { "frames": 3 } })).unwrap();
        app.update();
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        app.update();
        let status = status(&app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        assert_eq!(status.result.unwrap()["met"], json!({ "frames": 3 }));
    }

    #[test]
    fn virtual_seconds_while_paused_fails_at_once() {
        let mut app = app();
        pause(&mut app, true);
        let err = call(&mut app, json!({ "condition": { "virtual_seconds": 1.0 } })).unwrap_err();
        assert_eq!(err.kind, "world_paused");
    }

    #[test]
    fn a_step_that_covers_virtual_seconds_succeeds() {
        let mut app = app();
        let job = call(
            &mut app,
            json!({ "condition": { "virtual_seconds": 0.05 } }),
        )
        .unwrap();
        {
            let world = app.world_mut();
            world.resource_scope(|world, mut control: Mut<TimeControl>| {
                control.step(&mut world.resource_mut::<Time<Virtual>>(), 5);
            });
        }
        for _ in 0..5 {
            app.update();
        }
        let status = status(&app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        assert!(paused(&app));
    }

    #[test]
    fn a_step_too_short_fails_world_paused_once_it_runs_out() {
        let mut app = app();
        let job = call(
            &mut app,
            json!({ "condition": { "virtual_seconds": 10.0 } }),
        )
        .unwrap();
        app.world_mut()
            .resource_scope(|world, mut control: Mut<TimeControl>| {
                control.step(&mut world.resource_mut::<Time<Virtual>>(), 2);
            });
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        app.update();
        assert_eq!(status(&app, &job).error.unwrap().kind, "world_paused");
    }

    #[test]
    fn time_run_resumes_and_repauses_on_success() {
        let mut app = app();
        pause(&mut app, true);
        let job = call(
            &mut app,
            json!({ "condition": { "virtual_seconds": 0.03 }, "time": "run" }),
        )
        .unwrap();
        assert!(!paused(&app));
        for _ in 0..4 {
            app.update();
        }
        let status = status(&app, &job);
        assert_eq!(status.state, "succeeded", "{status:?}");
        assert_eq!(status.result.unwrap()["paused"], true);
        assert!(paused(&app));
    }

    #[test]
    fn time_run_repauses_on_timeout_and_on_cancel() {
        let mut app = app();
        let job = call(
            &mut app,
            json!({ "condition": { "frames": 100000 }, "time": "run", "timeout_seconds": 0.01 }),
        )
        .unwrap();
        app.update();
        std::thread::sleep(Duration::from_millis(20));
        app.update();
        assert_eq!(status(&app, &job).error.unwrap().kind, "timed_out");
        assert!(paused(&app));

        let job = call(
            &mut app,
            json!({ "condition": { "frames": 100000 }, "time": "run" }),
        )
        .unwrap();
        app.update();
        assert!(!paused(&app));
        let cancelled = app
            .world_mut()
            .cancel_job(&job, CommandScope::Development)
            .unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert!(paused(&app));
    }

    #[test]
    fn a_log_line_after_the_given_sequence_matches() {
        let mut app = app();
        let buffer = app.world().resource::<LogBuffer>().clone();
        buffer.push(0., Level::INFO, "landscape", "impostors baked".to_owned());
        // A line from before the wait does not count by default.
        let job = call(
            &mut app,
            json!({ "condition": { "log": { "message_contains": "BAKED" } } }),
        )
        .unwrap();
        app.update();
        assert_eq!(status(&app, &job).state, "running");
        buffer.push(1., Level::INFO, "landscape", "impostors baked".to_owned());
        app.update();
        let first = status(&app, &job);
        assert_eq!(first.state, "succeeded", "{first:?}");
        assert_eq!(first.result.unwrap()["met"]["sequence"], 1);

        // `after_sequence` picks the starting point, including entries already there.
        let job = call(
            &mut app,
            json!({ "condition": { "log": { "message_contains": "baked", "after_sequence": 0 } } }),
        )
        .unwrap();
        app.update();
        assert_eq!(status(&app, &job).result.unwrap()["met"]["sequence"], 1);
    }

    #[test]
    fn a_level_the_buffer_does_not_capture_is_refused() {
        let mut app = App::new();
        app.add_plugins((TimeControlPlugin, CommandPlugin, WaitPlugin));
        let layer = crate::log::capture_layer(&mut app);
        drop(layer);
        let err = call(
            &mut app,
            json!({ "condition": { "log": { "min_level": "trace" } } }),
        )
        .unwrap_err();
        assert_eq!(err.kind, "level_not_captured");
    }

    #[test]
    fn hold_frames_is_refused_for_counts() {
        let mut app = app();
        let err = call(
            &mut app,
            json!({ "condition": { "frames": 3 }, "hold_frames": 2 }),
        )
        .unwrap_err();
        assert_eq!(err.kind, "invalid_params");
    }
}
