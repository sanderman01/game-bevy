//! Commands: named, typed, schema-described functionality any plugin registers in one place.
//!
//! A plugin registers a command with [`CommandAppExt::register_command`] (answers in the call) or
//! [`CommandAppExt::register_job_command`] (starts a [job](Jobs) that finishes later). Each has
//! a params struct and a result type deriving `serde` and `schemars::JsonSchema`, so callers get
//! a real JSON schema, and a [`CommandScope`] saying who may run it. Callers go through
//! [`CommandWorldExt::call_command`]: `ename_remote` for the agent tools, a console later.
//!
//! ```ignore
//! #[derive(Deserialize, JsonSchema)]
//! #[serde(deny_unknown_fields)]
//! struct Params { view: View }
//!
//! fn show(In(call): In<Call<Params>>, world: &mut World) -> Result<Shown, CommandError> { .. }
//!
//! app.register_command(CommandSpec::new("editor.set_view", "Show a view."), show);
//! ```
//!
//! This module has no network code and is in every build. Nothing runs a command unless
//! something calls it, and the caller states its own [`CommandScope`].

mod error;
mod jobs;
mod registry;
mod run_id;

use bevy::prelude::*;

pub use error::{CommandError, ErrorClass};
pub use jobs::{
    JobCancelRequested, JobHandle, JobId, JobStatus, JobWork, Jobs, OnCancel, RETENTION,
};
pub use registry::{
    Call, Caller, CommandAppExt, CommandInfo, CommandKind, CommandRegistry, CommandScope,
    CommandSpec, CommandWorldExt, Reply,
};
pub use run_id::RunId;
use schemars::JsonSchema;
use serde::Deserialize;

/// The params of a command that takes none. `{}` or `null`.
#[derive(Deserialize, JsonSchema, Debug, Default, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// The command registry, the job table and this run's [`RunId`].
///
/// Registers [`CommandRegistry`], [`Jobs`], [`RunId`] and the system that forgets finished jobs
/// after [`RETENTION`]. Reads the working directory once, for callers that send none.
pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        let run = RunId::for_this_process();
        app.init_resource::<CommandRegistry>()
            .insert_resource(Jobs::new(run.clone()))
            .insert_resource(run)
            .insert_resource(registry::StartupCwd(std::env::current_dir().ok()))
            .add_systems(Last, jobs::expire_jobs);
    }
}

#[cfg(test)]
mod tests;
