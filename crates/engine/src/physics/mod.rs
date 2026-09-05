//! Physics integration: avian3d configured to work inside big_space grids.

mod grid_writeback;

use avian3d::{
    PhysicsPlugins, debug_render::PhysicsDebugPlugin, physics_transform::PhysicsTransformConfig,
    prelude::PhysicsSystems,
};
use bevy::prelude::*;

/// Adds avian3d, and replaces its `position_to_transform` writeback with one that survives
/// an entity being recentered into another grid cell.
pub struct PhysicsIntegrationPlugin;

impl Plugin for PhysicsIntegrationPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugins::default())
            .add_plugins(PhysicsDebugPlugin)
            .insert_resource(PhysicsTransformConfig {
                propagate_before_physics: false,
                transform_to_position: true,
                // Replaced by grid_writeback::physics_position_to_transform.
                position_to_transform: false,
                ..default()
            })
            .add_systems(
                FixedPostUpdate,
                grid_writeback::physics_position_to_transform.after(PhysicsSystems::Writeback),
            );
    }
}
