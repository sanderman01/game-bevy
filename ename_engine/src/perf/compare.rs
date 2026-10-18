//! `perf.compare`: two settings sampled interleaved, `A B B A` per round, so linear drift cancels.
//!
//! Each step runs its arm's commands through `call_command` with the caller's scope, waits for
//! the ones that start jobs, then samples with a child [`Sampler`], which settles again first.
//! The `restore` commands run at the end however it ends: done, failed or cancelled. A cancel
//! while an arm's job runs cancels that job and waits for it to end first, since its own cleanup
//! could undo `restore`. Restore steps that start jobs are started, not awaited.

use std::collections::BTreeMap;

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    PerfDemandHolder,
    report::{Aggregate, SampleResult, Sampled, SeriesStat, Warning, Warnings, report, round2},
    sample::{
        PerfJob, Progress, SampleParams, SamplePlan, SampleSystems, Sampler, check_not_busy,
        spawn_sampler,
    },
    stats::{Metric, Verdict, verdict},
};
use crate::command::{
    Call, Caller, CommandAppExt, CommandError, CommandRegistry, CommandSpec, CommandWorldExt,
    JobCancelRequested, JobHandle, JobId, Jobs, OnCancel, Reply,
};

const DEFAULT_ROUNDS: u32 = 2;
const MAX_ROUNDS: u32 = 10;
/// Repeats per step unless `sample.repeats` says otherwise: the steps are the runs.
const STEP_REPEATS: u32 = 1;
/// Passes compared, by arm A's GPU time.
const TOP_PASSES: usize = 10;

/// One command call of an arm.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct Step {
    /// A dotted command name, such as "debug.set".
    command: String,
    /// Its params object.
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompareParams {
    /// Commands that set up arm A, run before each A sample. Jobs among them are awaited.
    a: Vec<Step>,
    /// Commands that set up arm B, run before each B sample.
    b: Vec<Step>,
    /// Commands run once at the end, also on failure and cancel, such as "debug.reset".
    #[serde(default)]
    restore: Vec<Step>,
    /// Rounds of A B B A, 1 to 10. Defaults to 2.
    #[serde(default)]
    rounds: Option<u32>,
    /// How each step samples, as `perf.sample`'s params; `repeats` defaults to 1 here.
    #[serde(default)]
    sample: SampleParams,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    A,
    B,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Arm::A => "a",
            Arm::B => "b",
        }
    }
}

/// One metric of B against A.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq)]
struct DiffOut {
    a: f64,
    b: f64,
    /// b - a; negative is faster.
    diff: f64,
    /// The larger of the two arms' noise.
    noise: Option<f64>,
    verdict: Verdict,
}

#[derive(Serialize, JsonSchema, Clone, Debug, Default)]
struct Diffs {
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_ms: Option<DiffOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_main_ms: Option<DiffOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    render_wall_ms: Option<DiffOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpu_ms: Option<DiffOut>,
    /// By label: GPU time, or CPU time without timestamps.
    views: BTreeMap<String, DiffOut>,
    /// GPU time of arm A's top passes.
    passes: BTreeMap<String, DiffOut>,
}

#[derive(Serialize, JsonSchema)]
struct Compared {
    a: SampleResult,
    b: SampleResult,
    diff: Diffs,
    /// The arms in the order they were sampled.
    order: Vec<&'static str>,
    /// Both arms' warnings, and the restore steps that failed.
    warnings: Vec<Warning>,
}

#[derive(Serialize)]
struct CompareProgress {
    /// 1-based.
    step: usize,
    of: usize,
    arm: &'static str,
    /// "commands", "sampling" or "cancelling" (waiting for an arm's job to end).
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample: Option<Progress>,
}

enum State {
    /// Running the arm's commands from `index`; `waiting` is a job one of them started.
    Commands {
        index: usize,
        waiting: Option<JobId>,
    },
    Sampling(Entity),
    /// Cancelled while an arm's job ran: `restore` waits until that job has ended.
    Draining(JobId),
}

#[derive(Component)]
#[require(PerfDemandHolder)]
struct CompareJob {
    job: JobHandle<Compared>,
    caller: Caller,
    a: Vec<Step>,
    b: Vec<Step>,
    restore: Vec<Step>,
    order: Vec<Arm>,
    plan: SamplePlan,
    step: usize,
    state: State,
    sampled: [Sampled; 2],
    cancel: bool,
}

pub(super) fn register(app: &mut App) {
    app.add_observer(cancel_compare)
        .add_systems(
            Last,
            drive_compares
                .after(SampleSystems)
                .run_if(any_with_component::<CompareJob>),
        )
        .register_job_command(
            CommandSpec::new(
                "perf.compare",
                "Sample two settings interleaved (A B B A per round), each set by commands such as \
                 debug.set, and report per-metric differences against the noise.",
            ),
            compare,
        );
}

fn compare(
    In(call): In<Call<CompareParams>>,
    world: &mut World,
) -> Result<JobHandle<Compared>, CommandError> {
    check_not_busy(world)?;
    let params = &call.params;
    let rounds = params.rounds.unwrap_or(DEFAULT_ROUNDS);
    if !(1..=MAX_ROUNDS).contains(&rounds) {
        return Err(CommandError::invalid(
            "invalid_params",
            format!("`rounds` must be 1 to {MAX_ROUNDS}, not {rounds}"),
        ));
    }
    let registry = world.resource::<CommandRegistry>();
    for step in params.a.iter().chain(&params.b).chain(&params.restore) {
        if registry.get(&step.command).is_none() {
            return Err(CommandError::invalid(
                "unknown_command",
                format!("no command `{}` in this build", step.command),
            ));
        }
    }
    let plan = SamplePlan::new(&params.sample, STEP_REPEATS, world)?;
    let order = (0..rounds)
        .flat_map(|_| [Arm::A, Arm::B, Arm::B, Arm::A])
        .collect();
    let work = world.spawn(PerfJob).id();
    let job = match call.start_job::<Compared>(world, work, OnCancel::Notify) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    let Call { params, caller, .. } = call;
    world.entity_mut(work).insert(CompareJob {
        job: job.clone(),
        caller,
        a: params.a,
        b: params.b,
        restore: params.restore,
        order,
        plan,
        step: 0,
        state: State::Commands {
            index: 0,
            waiting: None,
        },
        sampled: default(),
        cancel: false,
    });
    Ok(job)
}

fn cancel_compare(cancel: On<JobCancelRequested>, mut compares: Query<&mut CompareJob>) {
    if let Ok(mut compare) = compares.get_mut(cancel.entity) {
        compare.cancel = true;
    }
}

/// How a compare's frame ended.
enum Outcome {
    Running,
    Done,
    Failed(CommandError),
    Cancelled,
}

fn drive_compares(world: &mut World) {
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, With<CompareJob>>()
        .iter(world)
        .collect();
    for entity in entities {
        // Taken out, so its commands can run against the world.
        let Some(mut compare) = world.entity_mut(entity).take::<CompareJob>() else {
            continue;
        };
        let outcome = advance(world, &mut compare);
        if !matches!(outcome, Outcome::Running) {
            end(world, entity, compare, outcome);
            continue;
        }
        let arm = compare.order[compare.step];
        let (phase, sample) = match compare.state {
            State::Sampling(child) => (
                "sampling",
                world.get::<Sampler>(child).map(|s| s.progress().clone()),
            ),
            State::Commands { .. } => ("commands", None),
            State::Draining(_) => ("cancelling", None),
        };
        let progress = CompareProgress {
            step: compare.step + 1,
            of: compare.order.len(),
            arm: arm.name(),
            phase,
            sample,
        };
        world
            .resource_mut::<Jobs>()
            .set_progress(compare.job.id(), progress);
        world.entity_mut(entity).insert(compare);
    }
}

fn advance(world: &mut World, compare: &mut CompareJob) -> Outcome {
    let scope = compare.caller.scope;
    let running = |world: &World, job: &JobId| {
        world
            .resource::<Jobs>()
            .status(job, scope)
            .is_some_and(|status| status.state == "running")
    };
    if let State::Draining(job) = &compare.state {
        return if running(world, job) {
            Outcome::Running
        } else {
            Outcome::Cancelled
        };
    }
    if compare.cancel {
        if let State::Commands {
            waiting: Some(job), ..
        } = &compare.state
        {
            let job = job.clone();
            let _ = world.cancel_job(&job, scope);
            if running(world, &job) {
                compare.state = State::Draining(job);
                return Outcome::Running;
            }
        }
        return Outcome::Cancelled;
    }
    let arm = compare.order[compare.step];
    let number = compare.step + 1;
    let failed = |err: CommandError, what: &str| {
        Outcome::Failed(CommandError {
            message: format!(
                "step {} (arm {}) {what}: {}",
                number,
                arm.name(),
                err.message
            ),
            ..err
        })
    };
    match &compare.state {
        State::Commands { index, waiting } => {
            let mut index = *index;
            if let Some(job) = waiting {
                let status = world.resource::<Jobs>().status(job, compare.caller.scope);
                match status.as_ref().map(|s| s.state) {
                    Some("running") => return Outcome::Running,
                    Some("succeeded") => index += 1,
                    _ => {
                        let error = status.and_then(|s| s.error).unwrap_or_else(|| {
                            CommandError::failed("cancelled", "the job was cancelled")
                        });
                        let command = &steps(compare, arm)[index].command;
                        return failed(error, &format!("`{command}`"));
                    }
                }
            }
            // Instant commands run in this frame, one after another.
            while let Some(step) = steps(compare, arm).get(index) {
                match world.call_command(&step.command, step.params.clone(), compare.caller.clone())
                {
                    Ok(Reply::Done(_)) => index += 1,
                    Ok(Reply::Job(job)) => {
                        let waiting = Some(job);
                        let state = State::Commands { index, waiting };
                        compare.state = state;
                        return Outcome::Running;
                    }
                    Err(err) => return failed(err, &format!("`{}`", step.command)),
                }
            }
            match spawn_sampler(world, compare.plan.clone()) {
                Ok(child) => {
                    compare.state = State::Sampling(child);
                    Outcome::Running
                }
                Err(err) => failed(err, "sampling"),
            }
        }
        State::Sampling(child) => {
            let child = *child;
            let Some(outcome) = world
                .get_mut::<Sampler>(child)
                .and_then(|mut sampler| sampler.take_outcome())
            else {
                return Outcome::Running;
            };
            world.despawn(child);
            let sampled = match outcome {
                Ok(sampled) => sampled,
                Err(err) => return failed(err, "sampling"),
            };
            compare.sampled[arm as usize].merge(sampled);
            compare.step += 1;
            if compare.step == compare.order.len() {
                return Outcome::Done;
            }
            compare.state = State::Commands {
                index: 0,
                waiting: None,
            };
            Outcome::Running
        }
        State::Draining(_) => unreachable!("a draining compare returns above"),
    }
}

fn steps(compare: &CompareJob, arm: Arm) -> &[Step] {
    match arm {
        Arm::A => &compare.a,
        Arm::B => &compare.b,
    }
}

/// Stops what still runs, runs `restore`, reports, and despawns the work entity.
fn end(world: &mut World, entity: Entity, mut compare: CompareJob, outcome: Outcome) {
    match compare.state {
        State::Commands {
            waiting: Some(ref job),
            ..
        } => {
            let _ = world.cancel_job(job, compare.caller.scope);
        }
        State::Sampling(child) => {
            // Already gone when its sample finished the last step.
            if let Ok(child) = world.get_entity_mut(child) {
                child.despawn();
            }
        }
        State::Commands { .. } | State::Draining(_) => {}
    }
    let mut warnings = Warnings::default();
    for step in std::mem::take(&mut compare.restore) {
        if let Err(err) = world.call_command(&step.command, step.params, compare.caller.clone()) {
            warn!(
                "perf.compare: restore step `{}` failed: {err}",
                step.command
            );
            warnings.add(
                "restore_failed",
                format!("restore step `{}` failed: {}", step.command, err.message),
            );
        }
    }
    let result = match outcome {
        Outcome::Running => unreachable!("only a finished compare ends"),
        Outcome::Cancelled => {
            world.resource_mut::<Jobs>().cancelled(compare.job.id());
            world.despawn(entity);
            return;
        }
        Outcome::Failed(err) => Err(err),
        Outcome::Done => Ok(compared(&compare, warnings)),
    };
    world.resource_mut::<Jobs>().finish(&compare.job, result);
    world.despawn(entity);
}

fn compared(compare: &CompareJob, mut warnings: Warnings) -> Compared {
    let [a, b] = &compare.sampled;
    // Arms may change cameras on purpose, but not the window.
    let window = |sampled: &Sampled| sampled.context.layout.as_ref().map(|l| l.window);
    if let (Some(a_window), Some(b_window)) = (window(a), window(b))
        && a_window != b_window
    {
        warnings.add(
            "unstable_view",
            "arms a and b ran at different window sizes; see each arm's `window`",
        );
    }
    warnings.extend(&a.context.warnings);
    warnings.extend(&b.context.warnings);
    let detail = compare.plan.detail;
    let (a_result, a_aggregate) = report(a, detail);
    let (b_result, b_aggregate) = report(b, detail);
    for result in [&a_result, &b_result] {
        for warning in &result.warnings {
            warnings.add(warning.kind, warning.message.clone());
        }
    }
    Compared {
        diff: diffs(&a_aggregate, &b_aggregate),
        a: a_result,
        b: b_result,
        order: compare.order.iter().map(|arm| arm.name()).collect(),
        warnings: warnings.list(),
    }
}

fn diffs(a: &Aggregate, b: &Aggregate) -> Diffs {
    let series = |a: Option<SeriesStat>, b: Option<SeriesStat>| Some(diff(a?.metric, b?.metric));
    let mut views = BTreeMap::new();
    for view_a in &a.views {
        let Some(view_b) = b.views.iter().find(|view| view.label == view_a.label) else {
            continue;
        };
        let pair = match (view_a.gpu, view_b.gpu) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => view_a.cpu.zip(view_b.cpu),
        };
        if let Some((a, b)) = pair {
            views.insert(view_a.label.to_string(), diff(a, b));
        }
    }
    let mut passes = BTreeMap::new();
    let b_passes = b.passes.as_deref().unwrap_or_default();
    for pass_a in a.passes.iter().flatten().take(TOP_PASSES) {
        let gpu_b = b_passes
            .iter()
            .find(|pass| pass.path == pass_a.path)
            .and_then(|pass| pass.gpu);
        if let Some((a, b)) = pass_a.gpu.zip(gpu_b) {
            passes.insert(pass_a.path.clone(), diff(a, b));
        }
    }
    Diffs {
        frame_ms: series(a.frame_ms, b.frame_ms),
        cpu_main_ms: series(a.cpu_main_ms, b.cpu_main_ms),
        render_wall_ms: series(a.render_wall_ms, b.render_wall_ms),
        gpu_ms: series(a.gpu_ms, b.gpu_ms),
        views,
        passes,
    }
}

/// Judged on the rounded numbers it shows, so a 0.00 difference is never called faster.
fn diff(a: Metric, b: Metric) -> DiffOut {
    let rounded = |metric: Metric| Metric {
        median: round2(metric.median),
        noise: metric.noise.map(round2),
    };
    let (a, b) = (rounded(a), rounded(b));
    let noise = match (a.noise, b.noise) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0).max(b.unwrap_or(0.0))),
    };
    DiffOut {
        a: a.median,
        b: b.median,
        diff: round2(b.median - a.median),
        noise,
        verdict: verdict(a, b),
    }
}
