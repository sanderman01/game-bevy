//! The virtual clock: freeze the world, step it a fixed number of frames, resume.
//!
//! This is what makes the rest of the catalogue trustworthy. Reading a component from a world
//! that advances between the read and the next read tells the agent very little; reading one
//! from a frozen world, stepping ten frames, and reading again is an experiment.
//!
//! The control itself is `ename_engine::time::TimeControl`. This module is only the BRP surface
//! over it: pausing the world is an engine capability, not an agent-tooling one.

use bevy::{
    diagnostic::FrameCount,
    prelude::*,
    remote::{BrpResult, builtin_methods::parse_some},
};
use ename_engine::time::TimeControl;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const GET_METHOD: &str = "game.time.get";
pub const SET_METHOD: &str = "game.time.set";

#[derive(Serialize)]
struct TimeResponse {
    paused: bool,
    /// Frames still to run before the world repauses, when a step is in flight.
    steps_remaining: Option<u32>,
    /// Seconds of virtual time since startup. Does not advance while paused.
    elapsed_seconds: f64,
    /// Real frames rendered since startup. Keeps climbing while paused.
    frame: u32,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum SetParams {
    Pause,
    Resume,
    /// Runs `frames` frames and pauses again.
    Step {
        frames: u32,
    },
}

pub(crate) fn get(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    crate::to_value(read(world))
}

pub(crate) fn set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params: SetParams = parse_some(params)?;
    // `resource_scope` because the countdown and the clock it drives are two resources and both
    // are needed mutably at once.
    world.resource_scope(|world, mut control: Mut<TimeControl>| {
        let mut time = world.resource_mut::<Time<Virtual>>();
        match params {
            SetParams::Pause => control.pause(&mut time),
            SetParams::Resume => control.resume(&mut time),
            SetParams::Step { frames } => control.step(&mut time, frames),
        }
    });
    crate::to_value(read(world))
}

fn read(world: &World) -> TimeResponse {
    let time = world.resource::<Time<Virtual>>();
    TimeResponse {
        paused: time.is_paused(),
        steps_remaining: world.resource::<TimeControl>().steps_remaining(),
        elapsed_seconds: time.elapsed_secs_f64(),
        frame: world.resource::<FrameCount>().0,
    }
}
