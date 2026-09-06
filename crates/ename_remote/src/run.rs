//! Run control: pause the world, step it a fixed number of frames, resume.
//!
//! This is what makes the rest of the catalogue trustworthy. Reading a component from a world
//! that advances between the read and the next read tells the agent very little; reading one
//! from a frozen world, stepping ten frames, and reading again is an experiment.

use bevy::{
    diagnostic::FrameCount,
    prelude::*,
    remote::{BrpResult, builtin_methods::parse_some},
};
use ename_game::GameState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const GET_METHOD: &str = "game.run_state.get";
pub const SET_METHOD: &str = "game.run_state.set";

/// Whether the world is advancing, and how much longer.
///
/// `Time<Virtual>` owns the paused flag itself; this resource exists for the step countdown,
/// which has nowhere else to live.
#[derive(Resource, Default)]
pub(crate) struct RunControl {
    /// Frames still to run before repausing. `None` means "run until told otherwise".
    steps_remaining: Option<u32>,
}

/// Owns [`RunControl`] and the countdown that ends a step request.
pub(crate) struct RunControlPlugin;

impl Plugin for RunControlPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RunControl>()
            .add_systems(Last, repause_when_steps_exhausted);
    }
}

/// Runs in `Last` so a stepped frame is a whole frame: everything scheduled for it has already
/// happened by the time the countdown reaches zero.
fn repause_when_steps_exhausted(mut control: ResMut<RunControl>, mut time: ResMut<Time<Virtual>>) {
    let Some(remaining) = control.steps_remaining else {
        return;
    };
    match remaining.saturating_sub(1) {
        0 => {
            control.steps_remaining = None;
            time.pause();
        }
        left => control.steps_remaining = Some(left),
    }
}

#[derive(Serialize)]
pub(crate) struct RunStateResponse {
    paused: bool,
    /// Frames still to run before the world repauses, when a step is in flight.
    steps_remaining: Option<u32>,
    /// Seconds of virtual time since startup. Does not advance while paused.
    elapsed_seconds: f64,
    frame: u32,
    game_state: String,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum SetParams {
    Pause,
    Resume,
    /// Runs `frames` frames and pauses again.
    Step {
        frames: u32,
    },
}

pub(crate) fn get(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    crate::position::to_value(read(world))
}

pub(crate) fn set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SetParams = parse_some(params)?;
    let mut time = world.resource_mut::<Time<Virtual>>();
    match params {
        SetParams::Pause => {
            time.pause();
            world.resource_mut::<RunControl>().steps_remaining = None;
        }
        SetParams::Resume => {
            time.unpause();
            world.resource_mut::<RunControl>().steps_remaining = None;
        }
        // Zero frames is a pause, not an error: it is what "step until here" means at the end.
        SetParams::Step { frames } => {
            if frames == 0 {
                time.pause();
                world.resource_mut::<RunControl>().steps_remaining = None;
            } else {
                time.unpause();
                world.resource_mut::<RunControl>().steps_remaining = Some(frames);
            }
        }
    }
    crate::position::to_value(read(world))
}

fn read(world: &World) -> RunStateResponse {
    let time = world.resource::<Time<Virtual>>();
    RunStateResponse {
        paused: time.is_paused(),
        steps_remaining: world.resource::<RunControl>().steps_remaining,
        elapsed_seconds: time.elapsed_secs_f64(),
        frame: world.resource::<FrameCount>().0,
        game_state: format!("{:?}", world.resource::<State<GameState>>().get()),
    }
}
