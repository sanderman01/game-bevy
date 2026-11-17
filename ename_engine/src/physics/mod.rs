//! Physics integration: avian3d configured to work inside big_space grids.

mod collider_grid_transform;
mod gltf_colliders;
mod grid_propagate;
mod grid_writeback;

use avian3d::{
    PhysicsPlugins,
    physics_transform::{PhysicsTransformConfig, PhysicsTransformSystems},
    prelude::{Collider, PhysicsSystems, RigidBody},
};
use bevy::prelude::*;

pub use gltf_colliders::{COLLIDER_NODE_PREFIX, GltfColliderShape};

/// Adds avian3d, and replaces its transform propagation before the step and its
/// `position_to_transform` writeback after it with grid-aware versions: bodies keep every tick of
/// motion on frames with several fixed ticks, and survive being recentered into another cell.
/// Also turns glTF `COL_` nodes into colliders as their scenes spawn.
///
/// Needs no rendering or gizmos, so headless tests can use it as is. avian's
/// `PhysicsDebugPlugin` is a separate member of [`EnginePlugins`](crate::EnginePlugins).
pub struct PhysicsIntegrationPlugin;

impl Plugin for PhysicsIntegrationPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugins::default())
            .insert_resource(PhysicsTransformConfig {
                // Replaced by grid_propagate::propagate_grid_bodies.
                propagate_before_physics: false,
                transform_to_position: true,
                // Replaced by grid_writeback::physics_position_to_transform.
                position_to_transform: false,
                ..default()
            })
            .add_systems(
                FixedPostUpdate,
                (
                    // After avian's own collider propagation, so this one's result stands; before
                    // `TransformToPosition`, ahead of collider scale and mass properties.
                    (
                        grid_propagate::propagate_grid_bodies,
                        collider_grid_transform::propagate_collider_transforms,
                    )
                        .in_set(PhysicsSystems::Prepare)
                        .after(PhysicsTransformSystems::Propagate)
                        .before(PhysicsTransformSystems::TransformToPosition),
                    grid_writeback::physics_position_to_transform.after(PhysicsSystems::Writeback),
                ),
            )
            .add_observer(grid_propagate::reseed_nested_global_transform::<Collider>)
            .add_observer(grid_propagate::reseed_nested_global_transform::<RigidBody>)
            .add_observer(collider_grid_transform::mark_ancestors_of_new_collider)
            .add_observer(collider_grid_transform::mark_ancestors_of_reparented)
            .add_observer(collider_grid_transform::restart_detached)
            .register_type::<GltfColliderShape>()
            .add_observer(gltf_colliders::build_gltf_colliders);
    }
}

#[cfg(test)]
mod tests;
