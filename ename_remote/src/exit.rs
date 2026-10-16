//! `game.exit`: a graceful stop that works on every platform.
//!
//! Ctrl+C reaches a game only from its own terminal, and a detached Windows process has none.
//! This writes `AppExit::Success` the way closing the window does, so recordings and other
//! shutdown work finish normally and the process exits with code 0.

use std::time::{Duration, Instant};

use bevy::{prelude::*, remote::BrpResult};
use serde_json::Value;

pub const METHOD: &str = "game.exit";

/// Room for the answer to leave the HTTP task before the app stops polling it.
const ANSWER_GRACE: Duration = Duration::from_millis(100);

/// When the requested exit is due.
#[derive(Resource)]
struct ExitRequested(Instant);

/// Exits the app shortly after a `game.exit` request.
pub(crate) struct ExitPlugin;

impl Plugin for ExitPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Last, exit_when_due.run_if(resource_exists::<ExitRequested>));
    }
}

pub(crate) fn exit(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    world.insert_resource(ExitRequested(Instant::now() + ANSWER_GRACE));
    Ok(Value::Null)
}

fn exit_when_due(requested: Res<ExitRequested>, mut exits: MessageWriter<AppExit>) {
    if Instant::now() >= requested.0 {
        exits.write(AppExit::Success);
    }
}
