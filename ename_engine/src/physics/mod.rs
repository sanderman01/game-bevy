//! Physics integration: avian3d configured to work inside big_space grids.

mod collider_grid_transform;
mod grid_writeback;

use avian3d::{
    PhysicsPlugins, debug_render::PhysicsDebugPlugin, physics_transform::PhysicsTransformConfig,
    prelude::PhysicsSystems,
};
use bevy::prelude::*;

use crate::bigspace::GridSystems;

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
            )
            .add_systems(
                PostUpdate,
                collider_grid_transform::collider_transform_from_global
                    .in_set(GridSystems::Recentered),
            );
    }
}
