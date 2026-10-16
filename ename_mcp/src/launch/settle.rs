//! `settle: true` on a launch: once the game is ready, run its `wait.settled` job within what is
//! left of the launch's `wait`. The outcome is a note on a running game, never a launch failure.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::port;
use crate::brp::BrpError;

const POLL: Duration = Duration::from_millis(250);
/// `wait.settled`'s own bounds on `timeout_seconds`.
const MIN_TIMEOUT: f64 = 1.0;
const MAX_TIMEOUT: f64 = 600.0;

#[derive(Deserialize)]
struct JobAnswer {
    job: Job,
}

#[derive(Deserialize)]
struct Job {
    id: String,
    state: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JobError>,
    #[serde(default)]
    progress: Option<Value>,
}

#[derive(Deserialize)]
struct JobError {
    #[serde(default)]
    message: String,
}

/// Settles the game on `port` until `deadline` and says how it went.
pub(super) async fn settle(port: u16, deadline: Instant) -> String {
    let client = port::client(port);
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .as_secs_f64()
        .clamp(MIN_TIMEOUT, MAX_TIMEOUT);
    let body = json!({
        "command": "wait.settled",
        "params": { "timeout_seconds": (timeout * 1000.0).round() / 1000.0 },
    });
    let mut job = match client.call::<JobAnswer>("game.command.call", body).await {
        Ok(answer) => answer.job,
        Err(err) => {
            let unknown = err.downcast_ref::<BrpError>().is_some_and(|err| {
                err.data
                    .as_ref()
                    .is_some_and(|data| data["kind"] == "unknown_command")
            });
            return if unknown {
                "`settle` is not available: this game has no `wait.settled`; returned at ready."
                    .into()
            } else {
                format!("could not settle: {err:#}")
            };
        }
    };
    loop {
        match job.state.as_str() {
            "succeeded" => {
                let result = job.result.unwrap_or_default();
                return format!(
                    "settled after ready in {} s ({} frames).",
                    result["seconds"], result["frames"]
                );
            }
            "failed" => {
                let message = job.error.map(|error| error.message).unwrap_or_default();
                return format!("did not settle ({message}); the game runs.");
            }
            "cancelled" => return "the settle was cancelled; the game runs.".into(),
            _ if Instant::now() >= deadline => {
                let progress = job
                    .progress
                    .map(|progress| format!(" (last: {progress})"))
                    .unwrap_or_default();
                return format!(
                    "still settling when the wait ran out{progress}; job `{}` keeps going and \
                     job_wait follows it.",
                    job.id
                );
            }
            _ => {}
        }
        tokio::time::sleep(POLL).await;
        job = match client
            .call::<JobAnswer>("game.job.status", json!({ "job": job.id }))
            .await
        {
            Ok(answer) => answer.job,
            Err(err) => return format!("lost the settle job: {err:#}"),
        };
    }
}
