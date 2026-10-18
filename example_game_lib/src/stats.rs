//! Example content for the statistics panel (`F3`): a section of its own and a row in a
//! built-in section.

use bevy::{diagnostic::FrameTimeDiagnosticsPlugin, prelude::*};
use ename_engine::perf::{StatsCells, StatsRow, StatsSceneSection, StatsSection};

/// Adds an "Example" section with a mesh count and a frame count row in the Scene section.
pub struct StatsExamplePlugin;

impl Plugin for StatsExamplePlugin {
    fn build(&self, app: &mut App) {
        // The built-in sections exist from `PreStartup`.
        app.add_systems(Startup, add_stats);
    }
}

fn add_stats(mut commands: Commands, scene: Single<Entity, With<StatsSceneSection>>) {
    commands.spawn((
        StatsSection::titled("Example", 100).with_header(["", "value"]),
        children![StatsRow::system("Meshes", 0, mesh_count)],
    ));
    commands.spawn((
        StatsRow::diagnostic("Frames", 20, FrameTimeDiagnosticsPlugin::FRAME_COUNT),
        ChildOf(*scene),
    ));
}

fn mesh_count(_host: In<Entity>, meshes: Query<(), With<Mesh3d>>) -> Option<StatsCells> {
    Some(StatsCells::one(meshes.iter().count().to_string()))
}
