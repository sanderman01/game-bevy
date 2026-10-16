//! Waiting for the running game: settle probes crates fill, and the commands that wait on them.
//!
//! A crate that owns work which finishes later (asset loads, pipeline compiles, streaming)
//! registers a probe with [`SettleAppExt::register_settle_probe`]. While something waits, every
//! probe is sampled once a frame in [`WaitSystems::Sample`], and the waits are decided in
//! [`WaitSystems::Resolve`]. Nothing runs while nobody waits.
//!
//! No network code: `ename_remote` exposes the commands like any other.

mod assets;
mod pipelines;
mod predicate;
mod probe;
mod settled;
mod until;

use bevy::prelude::*;

pub use probe::{Activity, SettleAppExt, SettleSample, SettleWaiter};
pub use settled::{SettleGate, SettleParams, SettleProgress, SettleTimedOut, Settled};

use crate::time::TimeControlSystems;

/// The order waits are evaluated in, in `Last`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum WaitSystems {
    /// Runs every probe and stores [`SettleSample`].
    Sample,
    /// Decides each wait from this frame's sample.
    Resolve,
}

/// The settle probe registry, its sampling, and the wait commands.
///
/// Registers [`SettleSample`], the `assets` and `pipelines` probes, `wait.settled` and
/// `wait.activity` and `wait.until`, and orders [`WaitSystems`] after
/// [`TimeControlSystems::Countdown`], so a stepped frame is judged with its step already counted.
pub struct WaitPlugin;

impl Plugin for WaitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<probe::SettleProbes>()
            .init_resource::<SettleSample>()
            .configure_sets(
                Last,
                (WaitSystems::Sample, WaitSystems::Resolve)
                    .chain()
                    .after(TimeControlSystems::Countdown),
            )
            .add_systems(
                Last,
                probe::sample_probes
                    .in_set(WaitSystems::Sample)
                    .run_if(any_with_component::<SettleWaiter>),
            );
        app.register_settle_probe("assets", assets::assets_probe);
        pipelines::register(app);
        settled::register(app);
        until::register(app);
        app.add_systems(
            Last,
            settled::resolve_gates
                .in_set(WaitSystems::Resolve)
                .run_if(any_with_component::<SettleGate>),
        );
        app.add_systems(
            Last,
            until::resolve_untils
                .in_set(WaitSystems::Resolve)
                .run_if(any_with_component::<until::UntilWait>),
        );
    }
}
