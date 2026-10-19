//! Negative control for the integration tests in `tests/physics_grid.rs`: without
//! `propagate_grid_bodies`, a frame with two fixed ticks starts its second tick from last frame's
//! pose, and the same kind of measurement catches it.

use std::time::Duration;

use avian3d::prelude::{Collider, Gravity, LinearVelocity, Position, RigidBody};
use bevy::{
    asset::AssetPlugin, ecs::schedule::ScheduleCleanupPolicy, mesh::MeshPlugin, prelude::*,
    time::TimeUpdateStrategy,
};
use big_space::prelude::{BigSpace, BigSpaceDefaultPlugins, CellCoord, FloatingOrigin, Grid};

use super::{PhysicsIntegrationPlugin, grid_propagate::propagate_grid_bodies};

const SPEED: f32 = 3.0;

/// The farthest avian's `Position` of a body moving at [`SPEED`] was from its grid pose after a
/// frame with two fixed ticks, over a second of such frames, in metres. Without propagation the
/// second tick restarts from last frame's pose, and `Position` ends a tick behind.
fn lag(with_propagation: bool) -> f32 {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::input::InputPlugin,
        BigSpaceDefaultPlugins,
        AssetPlugin::default(),
        MeshPlugin,
        PhysicsIntegrationPlugin,
    ))
    .insert_resource(Gravity(Vec3::ZERO))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        1.0 / 32.0,
    )));
    if !with_propagation {
        app.remove_systems_in_set(
            FixedPostUpdate,
            propagate_grid_bodies,
            ScheduleCleanupPolicy::RemoveSystemsOnly,
        )
        .expect("propagate_grid_bodies is scheduled");
    }
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let root = world
        .spawn((BigSpace::default(), Grid::new(100.0, 0.0)))
        .id();
    world.spawn((
        ChildOf(root),
        FloatingOrigin,
        CellCoord::new(3, 0, -2),
        Transform::default(),
    ));
    let body = world
        .spawn((
            ChildOf(root),
            CellCoord::new(3, 0, -2),
            Transform::from_xyz(5.0, 0.0, 0.0),
            RigidBody::Kinematic,
            Collider::sphere(0.5),
            LinearVelocity(Vec3::new(SPEED, 0.0, 0.0)),
        ))
        .id();
    for _ in 0..10 {
        app.update();
    }

    // Where `Transform` and avian's `Position` put the body, in metres from the grid origin. The
    // floating origin never moves here, so `Position` is relative to its fixed place.
    let origin = app
        .world()
        .get::<Grid>(root)
        .unwrap()
        .cell_to_float(&CellCoord::new(3, 0, -2))
        .as_vec3();
    let mut worst = 0.0_f32;
    for _ in 0..32 {
        app.update();
        let world = app.world();
        let cell = world.get::<CellCoord>(body).unwrap();
        let grid = world.get::<Grid>(root).unwrap();
        let drawn =
            grid.cell_to_float(cell).as_vec3() + world.get::<Transform>(body).unwrap().translation;
        let simulated = origin + world.get::<Position>(body).unwrap().0;
        worst = worst.max(drawn.distance(simulated));
    }
    worst
}

#[test]
fn the_harness_catches_a_body_restarting_from_last_frames_pose() {
    let tick = SPEED / 64.0;
    let with = lag(true);
    assert!(with < 1e-3, "lagged {with} m with propagation");
    let without = lag(false);
    assert!(
        without > tick * 0.9,
        "lagged only {without} m without propagation, so the harness would miss the bug"
    );
}
