//! Jobs: work a command started that finishes in a later frame.
//!
//! A job command spawns (or picks) a work entity and calls [`Call::start_job`](super::Call) on
//! it. [`Jobs`] records the job as running and the feature finishes it, usually from an observer
//! on the work entity. A `JobWork` component ties the two: removing the work entity while the job
//! still runs marks it cancelled, so no job waits on work that is gone. Finished jobs are kept for
//! [`RETENTION`] and then forgotten.

use std::{
    collections::HashMap,
    marker::PhantomData,
    time::{Duration, Instant},
};

use bevy::{
    ecs::{lifecycle::HookContext, world::DeferredWorld},
    prelude::*,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CommandError, CommandScope, RunId};

/// How long a finished job stays readable.
pub const RETENTION: Duration = Duration::from_secs(5 * 60);

/// How many finished jobs are kept at most. The oldest go first.
const MAX_FINISHED: usize = 256;

/// `"<run>-<n>"`, such as `"kqvzmbta-3"`. An id from an earlier run starts with that run's id,
/// so it is recognisably not from this one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct JobId(String);

impl JobId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A running job, typed by the result its command declared, so [`Jobs::finish`] takes that type.
pub struct JobHandle<R> {
    id: JobId,
    _result: PhantomData<fn() -> R>,
}

impl<R> JobHandle<R> {
    pub fn id(&self) -> &JobId {
        &self.id
    }
}

impl<R> Clone for JobHandle<R> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            _result: PhantomData,
        }
    }
}

/// What cancelling a job does to its work entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnCancel {
    /// Despawn it. For work whose despawn already means "abort". The remove hook then marks the
    /// job cancelled.
    Despawn,
    /// Trigger [`JobCancelRequested`] on it and let the feature decide, then finish the job
    /// itself. The job stays running, with `cancel_requested` set, until it does.
    Notify,
}

/// Asks a job's work entity to end early. Only sent for jobs started with [`OnCancel::Notify`].
#[derive(EntityEvent, Clone, Copy, Debug)]
pub struct JobCancelRequested {
    pub entity: Entity,
}

/// Ties a work entity to its job. Removing it, by despawn or otherwise, cancels a job that has
/// not finished. Inserted by [`Call::start_job`](super::Call::start_job) only.
#[derive(Component, Debug)]
#[component(on_remove = work_removed)]
pub struct JobWork(pub(crate) JobId);

impl JobWork {
    pub fn job(&self) -> &JobId {
        &self.0
    }
}

fn work_removed(mut world: DeferredWorld, context: HookContext) {
    let Some(id) = world
        .get::<JobWork>(context.entity)
        .map(|work| work.0.clone())
    else {
        return;
    };
    if let Some(mut jobs) = world.get_resource_mut::<Jobs>() {
        jobs.cancelled(&id);
    }
}

#[derive(Debug)]
enum JobState {
    Running,
    Succeeded(Value),
    Failed(CommandError),
    Cancelled,
}

#[derive(Debug)]
struct JobRecord {
    /// Start order. Breaks ties between equal timestamps.
    seq: u64,
    command: &'static str,
    scope: CommandScope,
    work: Entity,
    on_cancel: OnCancel,
    state: JobState,
    progress: Option<Value>,
    cancel_requested: bool,
    started: Instant,
    /// When the record left [`JobState::Running`].
    ended: Option<Instant>,
}

/// A job as every caller sees it.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct JobStatus {
    pub id: JobId,
    pub command: String,
    /// "running", "succeeded", "failed" or "cancelled".
    pub state: &'static str,
    pub elapsed_seconds: f64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cancel_requested: bool,
    /// What the feature reports while it runs, such as frames recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CommandError>,
}

/// Every running job, and the finished ones still retained.
#[derive(Resource, Debug)]
pub struct Jobs {
    run: RunId,
    next: u64,
    records: HashMap<JobId, JobRecord>,
}

impl Jobs {
    pub(crate) fn new(run: RunId) -> Self {
        Self {
            run,
            next: 1,
            records: HashMap::new(),
        }
    }

    pub(crate) fn start(
        &mut self,
        command: &'static str,
        scope: CommandScope,
        work: Entity,
        on_cancel: OnCancel,
    ) -> JobId {
        let seq = self.next;
        let id = JobId(format!("{}-{seq}", self.run));
        self.next += 1;
        self.records.insert(
            id.clone(),
            JobRecord {
                seq,
                command,
                scope,
                work,
                on_cancel,
                state: JobState::Running,
                progress: None,
                cancel_requested: false,
                started: Instant::now(),
                ended: None,
            },
        );
        id
    }

    /// Records the outcome. Does nothing unless the job is still running.
    pub fn finish<R: Serialize>(&mut self, job: &JobHandle<R>, result: Result<R, CommandError>) {
        let state = match result {
            Ok(value) => match serde_json::to_value(value) {
                Ok(value) => JobState::Succeeded(value),
                Err(err) => JobState::Failed(CommandError::failed(
                    "handler_failed",
                    format!("the job's result does not serialize: {err}"),
                )),
            },
            Err(err) => JobState::Failed(err),
        };
        self.end(&job.id, state);
    }

    /// Marks a running job cancelled, for a feature whose cancel left nothing to report.
    pub fn cancelled(&mut self, job: &JobId) {
        self.end(job, JobState::Cancelled);
    }

    pub fn set_progress(&mut self, job: &JobId, progress: impl Serialize) {
        if let Some(record) = self.records.get_mut(job)
            && matches!(record.state, JobState::Running)
        {
            record.progress = serde_json::to_value(progress).ok();
        }
    }

    pub fn is_cancel_requested(&self, job: &JobId) -> bool {
        self.records
            .get(job)
            .is_some_and(|record| record.cancel_requested)
    }

    /// `None` for an unknown id and for a job above `scope`: a caller cannot tell the two apart.
    pub fn status(&self, job: &JobId, scope: CommandScope) -> Option<JobStatus> {
        self.visible(job, scope)
            .map(|record| Self::status_of(job, record, Instant::now()))
    }

    /// Running and retained jobs a caller with `scope` may see, oldest first.
    pub fn list(&self, scope: CommandScope) -> Vec<JobStatus> {
        let now = Instant::now();
        let mut jobs: Vec<_> = self
            .records
            .iter()
            .filter(|(_, record)| record.scope <= scope)
            .collect();
        jobs.sort_by_key(|(_, record)| record.seq);
        jobs.into_iter()
            .map(|(id, record)| Self::status_of(id, record, now))
            .collect()
    }

    /// The error for a job id that [`Self::status`] does not know.
    pub fn unknown(&self, job: &JobId) -> CommandError {
        let earlier_run = !job
            .as_str()
            .strip_prefix(self.run.as_str())
            .is_some_and(|rest| rest.starts_with('-'));
        let message = if earlier_run {
            format!(
                "job `{job}` is not from this run of the game (run `{}`); jobs do not survive a \
                 restart",
                self.run
            )
        } else {
            format!(
                "no job `{job}`: finished jobs are kept for {} minutes",
                RETENTION.as_secs() / 60
            )
        };
        CommandError::invalid("unknown_job", message)
    }

    /// Forgets finished jobs older than [`RETENTION`] at `now`, then the oldest finished ones
    /// past [`MAX_FINISHED`].
    pub(crate) fn expire(&mut self, now: Instant) {
        self.records.retain(|_, record| {
            record
                .ended
                .is_none_or(|ended| now.saturating_duration_since(ended) < RETENTION)
        });
        let mut finished: Vec<(Instant, u64, JobId)> = self
            .records
            .iter()
            .filter_map(|(id, record)| record.ended.map(|ended| (ended, record.seq, id.clone())))
            .collect();
        if finished.len() > MAX_FINISHED {
            finished.sort_by_key(|(ended, seq, _)| (*ended, *seq));
            for (_, _, id) in &finished[..finished.len() - MAX_FINISHED] {
                self.records.remove(id);
            }
        }
    }

    /// The work entity and cancel rule of a visible job, marking a running one as asked to
    /// cancel. `None` for the work when the job has already finished.
    fn request_cancel(
        &mut self,
        job: &JobId,
        scope: CommandScope,
    ) -> Option<Option<(Entity, OnCancel)>> {
        let record = self.records.get_mut(job).filter(|r| r.scope <= scope)?;
        if !matches!(record.state, JobState::Running) {
            return Some(None);
        }
        record.cancel_requested = true;
        Some(Some((record.work, record.on_cancel)))
    }

    fn end(&mut self, job: &JobId, state: JobState) {
        if let Some(record) = self.records.get_mut(job)
            && matches!(record.state, JobState::Running)
        {
            record.state = state;
            record.ended = Some(Instant::now());
            record.progress = None;
        }
    }

    fn visible(&self, job: &JobId, scope: CommandScope) -> Option<&JobRecord> {
        self.records.get(job).filter(|record| record.scope <= scope)
    }

    fn status_of(id: &JobId, record: &JobRecord, now: Instant) -> JobStatus {
        let elapsed = record.ended.unwrap_or(now).duration_since(record.started);
        let (state, result, error) = match &record.state {
            JobState::Running => ("running", None, None),
            JobState::Succeeded(value) => ("succeeded", Some(value.clone()), None),
            JobState::Failed(err) => ("failed", None, Some(err.clone())),
            JobState::Cancelled => ("cancelled", None, None),
        };
        JobStatus {
            id: id.clone(),
            command: record.command.to_owned(),
            state,
            elapsed_seconds: (elapsed.as_secs_f64() * 1000.0).round() / 1000.0,
            cancel_requested: record.cancel_requested,
            progress: record.progress.clone(),
            result,
            error,
        }
    }
}

/// Cancels a job as its [`OnCancel`] says, and returns its status afterwards.
///
/// The `Jobs` borrow is released before the work entity is despawned or notified, so the remove
/// hook and any observer may use `Jobs` themselves.
pub(crate) fn cancel_job(
    world: &mut World,
    job: &JobId,
    scope: CommandScope,
) -> Result<JobStatus, CommandError> {
    let mut jobs = world.resource_mut::<Jobs>();
    let Some(action) = jobs.request_cancel(job, scope) else {
        return Err(jobs.unknown(job));
    };
    match action {
        None => {}
        Some((work, OnCancel::Despawn)) => {
            let despawned = world
                .get_entity_mut(work)
                .map(EntityWorldMut::despawn)
                .is_ok();
            if !despawned {
                // Gone without the hook noticing, which means it was never really tied to it.
                world.resource_mut::<Jobs>().cancelled(job);
            }
        }
        Some((work, OnCancel::Notify)) => {
            world.trigger(JobCancelRequested { entity: work });
        }
    }
    world.flush();
    let jobs = world.resource::<Jobs>();
    jobs.status(job, scope).ok_or_else(|| jobs.unknown(job))
}

/// Forgets finished jobs past their retention.
pub(crate) fn expire_jobs(mut jobs: ResMut<Jobs>) {
    jobs.expire(Instant::now());
}

impl<R> JobHandle<R> {
    pub(crate) fn new(id: JobId) -> Self {
        Self {
            id,
            _result: PhantomData,
        }
    }
}
