//! Frame-time and entity-count diagnostics in every build, and the `diagnostics.log` debug switch
//! that decides which of them, if any, go to the log.

use std::time::Duration;

use bevy::{
    diagnostic::{
        EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin,
        LogDiagnosticsState,
    },
    platform::collections::HashSet,
    prelude::*,
};

use crate::debug_switch::{DebugSwitchAppExt, DebugSwitchKeys, DebugSwitchSpec};

/// How often [`LogDiagnosticsPlugin`] logs, when [`DiagnosticsLog`] lets it.
const LOG_INTERVAL: Duration = Duration::from_secs(5);

/// Which Bevy diagnostics are logged every 5 s. A binary that wants logging on at launch, say from
/// an environment variable, inserts its value before `EnginePlugins`' first frame.
#[derive(Resource, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Resource, Default)]
pub enum DiagnosticsLog {
    #[default]
    Off,
    /// Frames per second, frame time and frame count.
    FrameTimes,
    /// Every registered diagnostic, render pass timings included where
    /// `RenderDiagnosticsPlugin` is present (editor builds).
    All,
}

/// Adds [`FrameTimeDiagnosticsPlugin`], [`EntityCountDiagnosticsPlugin`] and
/// [`LogDiagnosticsPlugin`], each only if not already added, and registers [`DiagnosticsLog`] as
/// the `diagnostics.log` debug switch. "Already added" sees only plugins added before
/// `EnginePlugins`; a binary adding one after it panics on the duplicate. [`DiagnosticsLog`]
/// owns the log filter even for a `LogDiagnosticsPlugin` the binary added itself.
pub struct DiagnosticsLogPlugin;

impl Plugin for DiagnosticsLogPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<FrameTimeDiagnosticsPlugin>() {
            app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        }
        if !app.is_plugin_added::<EntityCountDiagnosticsPlugin>() {
            app.add_plugins(EntityCountDiagnosticsPlugin::default());
        }
        if !app.is_plugin_added::<LogDiagnosticsPlugin>() {
            app.add_plugins(LogDiagnosticsPlugin {
                wait_duration: LOG_INTERVAL,
                // An empty filter logs nothing, which is `DiagnosticsLog::Off`.
                filter: Some(HashSet::new()),
                ..default()
            });
        }
        app.init_resource::<DiagnosticsLog>()
            .register_type::<DiagnosticsLog>()
            .add_resource_debug_switch::<DiagnosticsLog>(DebugSwitchSpec::new(
                "diagnostics",
                "diagnostics.log",
                "Log Bevy diagnostics every 5 s: none, frame times, or all of them.",
            ))
            .add_systems(
                PreUpdate,
                apply_diagnostics_log
                    .after(DebugSwitchKeys)
                    .run_if(resource_changed::<DiagnosticsLog>),
            );
    }
}

fn apply_diagnostics_log(log: Res<DiagnosticsLog>, mut state: ResMut<LogDiagnosticsState>) {
    match *log {
        DiagnosticsLog::Off => state.enable_filtering(),
        DiagnosticsLog::FrameTimes => {
            state.enable_filtering();
            state.extend_filter([
                FrameTimeDiagnosticsPlugin::FPS,
                FrameTimeDiagnosticsPlugin::FRAME_TIME,
                FrameTimeDiagnosticsPlugin::FRAME_COUNT,
            ]);
        }
        DiagnosticsLog::All => state.disable_filtering(),
    }
}
