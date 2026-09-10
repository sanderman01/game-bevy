//! Time control: whether the world advances, and stepping it a fixed number of frames.
//!
//! `Time<Virtual>` owns the paused flag itself. [`TimeControl`] adds the step countdown, which has
//! nowhere else to live, and puts pause, resume and step behind one type so every caller agrees
//! on what a step in flight means. A pause menu and the agent-facing side channel both drive it.

use bevy::prelude::*;

/// The step countdown that sits alongside `Time<Virtual>`'s paused flag.
///
/// Every method takes the virtual clock rather than reaching for it, so this is usable from a
/// system holding both as resources and from an exclusive system holding a `&mut World`.
#[derive(Resource, Default)]
pub struct TimeControl {
    steps_remaining: Option<u32>,
}

impl TimeControl {
    /// Frames still to run before the world repauses. `None` means "run until told otherwise".
    pub fn steps_remaining(&self) -> Option<u32> {
        self.steps_remaining
    }

    /// Freezes the world, cancelling a step in flight.
    pub fn pause(&mut self, time: &mut Time<Virtual>) {
        self.steps_remaining = None;
        time.pause();
    }

    /// Unfreezes the world, cancelling a step in flight.
    pub fn resume(&mut self, time: &mut Time<Virtual>) {
        self.steps_remaining = None;
        time.unpause();
    }

    /// Runs `frames` frames and pauses again.
    ///
    /// Zero frames is a pause, not an error: it is what "step until here" means at the end.
    pub fn step(&mut self, time: &mut Time<Virtual>, frames: u32) {
        if frames == 0 {
            self.pause(time);
        } else {
            self.steps_remaining = Some(frames);
            time.unpause();
        }
    }
}

/// Owns [`TimeControl`] and the countdown that ends a step request.
pub struct TimeControlPlugin;

impl Plugin for TimeControlPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimeControl>()
            .add_systems(Last, repause_when_steps_exhausted);
    }
}

/// Runs in `Last` so a stepped frame is a whole frame: everything scheduled for it has already
/// happened by the time the countdown reaches zero.
fn repause_when_steps_exhausted(mut control: ResMut<TimeControl>, mut time: ResMut<Time<Virtual>>) {
    let Some(remaining) = control.steps_remaining else {
        return;
    };
    match remaining.saturating_sub(1) {
        0 => control.pause(&mut time),
        left => control.steps_remaining = Some(left),
    }
}
