use std::time::Duration;

use avian3d::{physics_transform::PhysicsTransformConfig, prelude::PhysicsPlugins};
use bevy::{asset::AssetPlugin, mesh::MeshPlugin, scene::ScenePlugin, time::TimeUpdateStrategy};
use big_space::prelude::{BigSpace, BigSpaceDefaultPlugins, CellCoord, FloatingOrigin, Grid};

use super::*;
use crate::camera::ground::{self, GroundGuardMode, LiftPlan};

// The colliders probe alone, as the `colliders` mode asks it.
const MODE: GroundGuardMode = GroundGuardMode::Colliders;

fn plan_lift(world: &mut World, point: DVec3, min_clearance: f64) -> LiftPlan {
    ground::plan_lift(world, point, min_clearance, MODE)
}

use avian3d::prelude::{Collider, RigidBody, Sensor};

const EDGE: f32 = 100.0;
const CLEARANCE: f64 = 2.0;

/// Floating-origin placements the scenes run under: cell and in-cell offset.
const ORIGINS: [([i64; 3], [f32; 3]); 3] = [
    ([0, 0, 0], [0.0, 0.0, 0.0]),
    ([3, 0, -2], [12.0, 4.0, -7.0]),
    ([-6, 2, 9], [-31.0, 8.0, 44.0]),
];

struct Scene {
    app: App,
    root: Entity,
}

impl Scene {
    fn new(origin: ([i64; 3], [f32; 3])) -> Self {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::input::InputPlugin,
            BigSpaceDefaultPlugins,
            AssetPlugin::default(),
            MeshPlugin,
            ScenePlugin,
            PhysicsPlugins::default(),
        ))
        .insert_resource(PhysicsTransformConfig {
            propagate_before_physics: false,
            transform_to_position: true,
            position_to_transform: false,
            ..default()
        })
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / 30.0,
        )));
        app.add_plugins(crate::camera::ground::CameraGroundPlugin);
        app.finish();
        app.cleanup();
        let world = app.world_mut();
        let root = world
            .spawn((
                BigSpace::default(),
                Grid::new(EDGE, 0.0),
                Transform::default(),
            ))
            .id();
        let ([x, y, z], offset) = origin;
        world.spawn((
            ChildOf(root),
            FloatingOrigin,
            CellCoord::new(x, y, z),
            Transform::from_translation(Vec3::from(offset)),
        ));
        Self { app, root }
    }

    /// A static cuboid from `bottom` to `top` at absolute `(x, z)`, `size` wide.
    fn cuboid(&mut self, x: f64, z: f64, bottom: f64, top: f64, size: f32) -> Entity {
        let centre = DVec3::new(x, (bottom + top) / 2.0, z);
        let (cell, translation) = self.grid().translation_to_grid(centre);
        self.app
            .world_mut()
            .spawn((
                ChildOf(self.root),
                cell,
                Transform::from_translation(translation),
                RigidBody::Static,
                Collider::cuboid(size, (top - bottom) as f32, size),
            ))
            .id()
    }

    /// A static triangle mesh at absolute `centre`.
    fn mesh(&mut self, centre: DVec3, vertices: Vec<Vec3>, indices: Vec<[u32; 3]>) -> Entity {
        let (cell, translation) = self.grid().translation_to_grid(centre);
        let root = self.root;
        self.app
            .world_mut()
            .spawn((
                ChildOf(root),
                cell,
                Transform::from_translation(translation),
                RigidBody::Static,
                Collider::trimesh(vertices, indices),
            ))
            .id()
    }

    /// A closed box mesh from `bottom` to `top`, faces outward, or inward for a cave.
    fn mesh_box(&mut self, x: f64, z: f64, bottom: f64, top: f64, size: f32, inward: bool) {
        let (vertices, indices) = box_mesh(size, (top - bottom) as f32, inward);
        self.mesh(DVec3::new(x, (bottom + top) / 2.0, z), vertices, indices);
    }

    /// An upward-facing square at height `y`.
    fn mesh_sheet(&mut self, x: f64, z: f64, y: f64, size: f32) -> Entity {
        let h = size / 2.0;
        let corners = vec![
            Vec3::new(-h, 0.0, -h),
            Vec3::new(-h, 0.0, h),
            Vec3::new(h, 0.0, h),
            Vec3::new(h, 0.0, -h),
        ];
        self.mesh(DVec3::new(x, y, z), corners, vec![[0, 1, 2], [0, 2, 3]])
    }

    fn grid(&self) -> Grid {
        self.app.world().get::<Grid>(self.root).unwrap().clone()
    }

    /// Lets physics sync the colliders.
    fn settle(&mut self) {
        for _ in 0..5 {
            self.app.update();
        }
    }

    fn lift(&mut self, x: f64, y: f64, z: f64) -> LiftPlan {
        plan_lift(self.app.world_mut(), DVec3::new(x, y, z), CLEARANCE)
    }
}

fn near(value: Option<f64>, expected: f64) -> bool {
    value.is_some_and(|value| (value - expected).abs() < 2e-3)
}

/// Runs `check` under every floating-origin placement.
fn each_origin(build: impl Fn(&mut Scene), check: impl Fn(&mut Scene)) {
    for origin in ORIGINS {
        let mut scene = Scene::new(origin);
        build(&mut scene);
        scene.settle();
        check(&mut scene);
    }
}
/// Box vertices and triangles, wound so the faces point out (or in).
fn box_mesh(size: f32, height: f32, inward: bool) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let (h, v) = (size / 2.0, height / 2.0);
    let vertices = vec![
        Vec3::new(-h, -v, -h),
        Vec3::new(h, -v, -h),
        Vec3::new(h, v, -h),
        Vec3::new(-h, v, -h),
        Vec3::new(-h, -v, h),
        Vec3::new(h, -v, h),
        Vec3::new(h, v, h),
        Vec3::new(-h, v, h),
    ];
    let mut indices = vec![
        [0, 1, 5],
        [0, 5, 4],
        [3, 7, 6],
        [3, 6, 2],
        [0, 3, 2],
        [0, 2, 1],
        [4, 5, 6],
        [4, 6, 7],
        [0, 4, 7],
        [0, 7, 3],
        [1, 2, 6],
        [1, 6, 5],
    ];
    if inward {
        for triangle in &mut indices {
            triangle.swap(1, 2);
        }
    }
    (vertices, indices)
}

#[test]
fn a_room_ceiling_caps_the_lift() {
    each_origin(
        |scene| {
            scene.cuboid(180.0, -200.0, -1.0, 0.0, 20.0);
            scene.cuboid(180.0, -200.0, 2.5, 2.8, 20.0);
        },
        |scene| {
            let world = scene.app.world_mut();
            let plan = plan_lift(world, DVec3::new(180.0, 1.0, -200.0), 3.0);
            assert!(near(plan.ground.surface, 0.0), "{plan:?}");
            assert!(near(plan.ceiling, 2.5), "{plan:?}");
            assert!(near(plan.target, 2.4) && plan.limited, "{plan:?}");
            let plan = scene.lift(180.0, 1.0, -200.0);
            assert!(near(plan.target, 2.0) && !plan.limited, "{plan:?}");
            // 5 mm under the ceiling: already above the cap, so it stays.
            let plan = scene.lift(180.0, 2.495, -200.0);
            assert!(near(plan.ceiling, 2.5), "{plan:?}");
            assert_eq!(plan.target, None, "{plan:?}");
        },
    );
}

#[test]
fn in_a_separate_cave_under_marked_terrain_the_camera_stays_in_the_cave() {
    each_origin(
        |scene| {
            let terrain = scene.mesh_sheet(-100.0, 100.0, 100.0, 200.0);
            scene
                .app
                .world_mut()
                .entity_mut(terrain)
                .insert(GroundSurface);
            scene.mesh_box(-100.0, 100.0, 40.0, 60.0, 10.0, true);
        },
        |scene| {
            let plan = scene.lift(-100.0, 41.0, 100.0);
            assert!(near(plan.ground.surface, 40.0), "{plan:?}");
            assert!(near(plan.ceiling, 60.0), "{plan:?}");
            assert!(near(plan.target, 42.0), "{plan:?}");
            // Beside the cave, under the terrain: onto it.
            let plan = scene.lift(-50.0, 30.0, 100.0);
            assert!(near(plan.ground.surface, 100.0), "{plan:?}");
            assert!(near(plan.target, 102.0), "{plan:?}");
        },
    );
}

#[test]
fn a_sensor_is_neither_ground_nor_ceiling() {
    each_origin(
        |scene| {
            scene.cuboid(240.0, 40.0, -1.0, 0.0, 10.0);
            let trigger = scene.cuboid(240.0, 40.0, -1.0, 100.0, 4.0);
            scene.app.world_mut().entity_mut(trigger).insert(Sensor);
        },
        |scene| {
            let plan = scene.lift(240.0, 1.0, 40.0);
            assert!(near(plan.ground.surface, 0.0), "{plan:?}");
            assert_eq!(plan.ceiling, None, "{plan:?}");
            assert!(near(plan.target, 2.0), "{plan:?}");
        },
    );
}

#[test]
fn flush_stacked_solids_lift_out_of_the_whole_stack() {
    each_origin(
        |scene| {
            scene.cuboid(240.0, -260.0, 0.0, 2.5, 4.0);
            scene.cuboid(240.0, -260.0, 2.5, 6.0, 4.0);
        },
        |scene| {
            let plan = scene.lift(240.0, 1.0, -260.0);
            assert!(near(plan.ground.surface, 6.0), "{plan:?}");
            assert!(near(plan.target, 8.0), "{plan:?}");
        },
    );
}

#[test]
fn on_a_compound_floor_the_ground_is_that_part_not_the_roof() {
    each_origin(
        |scene| {
            // A building: a floor slab and a roof, one compound collider.
            let building = Collider::compound(vec![
                (
                    Vec3::new(0.0, -0.5, 0.0),
                    Quat::IDENTITY,
                    Collider::cuboid(20.0, 1.0, 20.0),
                ),
                (
                    Vec3::new(0.0, 6.0, 0.0),
                    Quat::IDENTITY,
                    Collider::cuboid(20.0, 0.5, 20.0),
                ),
            ]);
            let (cell, translation) = scene
                .grid()
                .translation_to_grid(DVec3::new(0.0, 0.0, 200.0));
            let root = scene.root;
            scene.app.world_mut().spawn((
                ChildOf(root),
                cell,
                Transform::from_translation(translation),
                RigidBody::Static,
                building,
            ));
        },
        |scene| {
            // Exactly on the floor part, and inside it.
            for y in [0.0, -0.5] {
                let plan = scene.lift(1.0, y, 201.0);
                assert!(near(plan.ground.surface, 0.0), "{y}: {plan:?}");
                assert!(near(plan.ceiling, 5.75), "{y}: {plan:?}");
                assert!(near(plan.target, 2.0), "{y}: {plan:?}");
            }
        },
    );
}

#[test]
fn under_a_half_space_ground_the_camera_goes_onto_it() {
    each_origin(
        |scene| {
            let (cell, translation) = scene.grid().translation_to_grid(DVec3::new(0.0, 10.0, 0.0));
            let root = scene.root;
            scene.app.world_mut().spawn((
                ChildOf(root),
                cell,
                Transform::from_translation(translation),
                RigidBody::Static,
                Collider::half_space(Vec3::Y),
            ));
        },
        |scene| {
            let plan = scene.lift(3.0, 9.0, -4.0);
            assert!(near(plan.ground.surface, 10.0), "{plan:?}");
            assert!(near(plan.target, 12.0), "{plan:?}");
            let plan = scene.lift(3.0, 11.0, -4.0);
            assert!(near(plan.ground.surface, 10.0), "{plan:?}");
        },
    );
}

#[test]
fn far_along_x_a_close_ceiling_and_an_exact_floor_hold() {
    // Render-space X near 5 km, Y small: f32 resolution on X must not move vertical rays.
    let mut scene = Scene::new(([0, 0, 0], [0.0; 3]));
    scene.cuboid(5000.0, 0.0, -1.0, 0.0, 20.0);
    scene.cuboid(5000.0, 0.0, 2.5, 2.8, 20.0);
    scene.cuboid(5000.0, 100.0, 8.7, 9.7, 20.0);
    scene.settle();
    // 1 mm under the ceiling: below it, no lift.
    let plan = scene.lift(5000.0, 2.499, 0.0);
    assert!(near(plan.ground.surface, 0.0), "{plan:?}");
    assert!(near(plan.ceiling, 2.5), "{plan:?}");
    assert_eq!(plan.target, None, "{plan:?}");
    // Exactly on a floor: the full clearance.
    let plan = scene.lift(5001.0, 9.7, 101.0);
    assert!(near(plan.ground.surface, 9.7), "{plan:?}");
    assert!(near(plan.target, 11.7), "{plan:?}");
}

#[test]
fn marked_terrain_from_below_and_exactly_on_it_far_from_the_render_origin() {
    // About 5 km of render-space offset on every axis, at an odd height.
    let mut scene = Scene::new(([50, -50, 50], [10.0, -20.0, 30.0]));
    let terrain = scene.mesh_sheet(360.0, -140.0, 12.3456, 40.0);
    scene
        .app
        .world_mut()
        .entity_mut(terrain)
        .insert(GroundSurface);
    scene.settle();
    for below in [0.0, 1e-4, 1e-3, 5.0] {
        let plan = scene.lift(362.0, 12.3456 - below, -141.0);
        assert!(near(plan.ground.surface, 12.3456), "{below}: {plan:?}");
        assert!(near(plan.target, 14.3456), "{below}: {plan:?}");
    }
}

#[test]
fn a_solid_resting_on_marked_terrain_is_risen_through_from_below() {
    each_origin(
        |scene| {
            let terrain = scene.cuboid(0.0, -300.0, -1.0, 0.0, 30.0);
            scene
                .app
                .world_mut()
                .entity_mut(terrain)
                .insert(GroundSurface);
            scene.cuboid(0.0, -300.0, 0.0, 3.0, 4.0);
        },
        |scene| {
            let plan = scene.lift(0.0, -2.0, -300.0);
            assert!(near(plan.ground.surface, 3.0), "{plan:?}");
            assert!(near(plan.target, 5.0), "{plan:?}");
        },
    );
}

#[test]
fn answers_correctly_after_a_paused_jump_across_cells() {
    // Physics positions stay in the old render frame while paused; the probe must use that frame.
    let mut scene = Scene::new(ORIGINS[1]);
    scene.cuboid(305.0, -195.0, -1.0, 1.0, 20.0);
    scene.settle();
    scene
        .app
        .world_mut()
        .resource_mut::<Time<Virtual>>()
        .pause();
    let world = scene.app.world_mut();
    let origin = world
        .query_filtered::<Entity, With<FloatingOrigin>>()
        .single(world)
        .unwrap();
    world.entity_mut(origin).insert((
        CellCoord::new(-6, 2, 9),
        Transform::from_xyz(-31.0, 8.0, 44.0),
    ));
    for _ in 0..3 {
        scene.app.update();
    }
    assert!(near(scene.lift(301.0, 21.0, -189.0).ground.surface, 1.0));
    scene
        .app
        .world_mut()
        .resource_mut::<Time<Virtual>>()
        .unpause();
    scene.settle();
    assert!(near(scene.lift(301.0, 21.0, -189.0).ground.surface, 1.0));
}
