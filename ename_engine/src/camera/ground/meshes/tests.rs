use std::time::Duration;

use avian3d::{
    physics_transform::PhysicsTransformConfig,
    prelude::{Collider, PhysicsPlugins, RigidBody},
};
use bevy::{asset::AssetPlugin, mesh::MeshPlugin, scene::ScenePlugin, time::TimeUpdateStrategy};
use big_space::prelude::{BigSpace, BigSpaceDefaultPlugins, CellCoord, FloatingOrigin, Grid};

use bevy::camera::primitives::MeshAabb;

use super::*;
use crate::camera::ground::{self, GroundGuardMode, GroundSample, LiftPlan, ProbeKind};

// The mesh probe alone, as the `meshes` mode asks it; the test with both probes says `Both`.
const MODE: GroundGuardMode = GroundGuardMode::Meshes;

fn ground_below(world: &mut World, point: DVec3) -> GroundSample {
    ground::ground_below(world, point, MODE)
}

fn plan_lift(world: &mut World, point: DVec3, min_clearance: f64) -> LiftPlan {
    ground::plan_lift(world, point, min_clearance, MODE)
}

const EDGE: f32 = 100.0;

struct Scene {
    app: App,
    root: Entity,
}

impl Scene {
    fn new(origin: ([i64; 3], [f32; 3]), physics: bool) -> Self {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::input::InputPlugin,
            BigSpaceDefaultPlugins,
            AssetPlugin::default(),
            MeshPlugin,
            ScenePlugin,
        ))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / 30.0,
        )));
        app.add_plugins(crate::camera::ground::CameraGroundPlugin);
        if physics {
            app.add_plugins(PhysicsPlugins::default())
                .insert_resource(PhysicsTransformConfig {
                    propagate_before_physics: false,
                    transform_to_position: true,
                    position_to_transform: false,
                    ..default()
                });
        }
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

    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        self.app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(mesh)
    }

    /// A horizontal square mesh entity at absolute `centre`, under `parent` (the root grid when
    /// `None`), with its `Aabb` when the mesh is known.
    fn plane(&mut self, centre: DVec3, size: f32, marked: bool) -> Entity {
        let mesh = Plane3d::default().mesh().size(size, size).build();
        let aabb = mesh.compute_aabb();
        let handle = self.add_mesh(mesh);
        self.mesh_entity(centre, handle, aabb, marked)
    }

    fn mesh_entity(
        &mut self,
        centre: DVec3,
        handle: Handle<Mesh>,
        aabb: Option<Aabb>,
        marked: bool,
    ) -> Entity {
        let grid = self.app.world().get::<Grid>(self.root).unwrap().clone();
        let (cell, translation) = grid.translation_to_grid(centre);
        let root = self.root;
        let mut entity = self.app.world_mut().spawn((
            ChildOf(root),
            cell,
            Transform::from_translation(translation),
            Mesh3d(handle),
        ));
        if let Some(aabb) = aabb {
            entity.insert(aabb);
        }
        if marked {
            entity.insert(GroundSurface);
        }
        entity.id()
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.app.update();
        }
    }

    fn ground(&mut self, point: DVec3) -> GroundSample {
        ground_below(self.app.world_mut(), point)
    }
}

const ORIGINS: [([i64; 3], [f32; 3]); 3] = [
    ([0, 0, 0], [0.0, 0.0, 0.0]),
    ([3, 0, -2], [12.0, 4.0, -7.0]),
    ([50, -50, 50], [10.0, -20.0, 30.0]),
];

fn near(value: Option<f64>, expected: f64) -> bool {
    value.is_some_and(|value| (value - expected).abs() < 2e-3)
}

#[test]
fn a_marked_plane_is_the_ground_and_an_unmarked_one_is_ignored() {
    for origin in ORIGINS {
        let mut scene = Scene::new(origin, false);
        scene.plane(DVec3::new(10.0, 3.0, 10.0), 20.0, true);
        scene.plane(DVec3::new(10.0, 6.0, 10.0), 20.0, false);
        scene.settle();
        let sample = scene.ground(DVec3::new(11.0, 8.0, 9.0));
        assert!(near(sample.surface, 3.0), "{origin:?}: {sample:?}");
        // Nothing marked here.
        assert_eq!(
            scene.ground(DVec3::new(-50.0, 8.0, 9.0)),
            GroundSample::default()
        );
    }
}

#[test]
fn a_mesh_under_a_marked_scene_root_counts() {
    let mut scene = Scene::new(ORIGINS[0], false);
    let root = scene.root;
    let scene_root = scene
        .app
        .world_mut()
        .spawn((
            ChildOf(root),
            CellCoord::default(),
            Transform::default(),
            GroundSurface,
        ))
        .id();
    let child = scene.plane(DVec3::new(0.0, 2.0, 0.0), 20.0, false);
    scene
        .app
        .world_mut()
        .entity_mut(child)
        .insert(ChildOf(scene_root))
        .remove::<CellCoord>();
    scene.settle();
    assert!(near(scene.ground(DVec3::new(1.0, 5.0, 1.0)).surface, 2.0));
}

#[test]
fn a_tunnel_under_marked_terrain_with_a_collider_stays_a_tunnel() {
    let mut scene = Scene::new(ORIGINS[1], true);
    let grid = scene.app.world().get::<Grid>(scene.root).unwrap().clone();
    let root = scene.root;
    let cuboid = |scene: &mut Scene, y: f64, height: f32| {
        let (cell, translation) = grid.translation_to_grid(DVec3::new(0.0, y, 0.0));
        scene
            .app
            .world_mut()
            .spawn((
                ChildOf(root),
                cell,
                Transform::from_translation(translation),
                RigidBody::Static,
                Collider::cuboid(40.0, height, 40.0),
            ))
            .id()
    };
    // Marked terrain at 10 that is both a render mesh and a collider.
    let terrain = scene.plane(DVec3::new(0.0, 10.0, 0.0), 40.0, true);
    scene
        .app
        .world_mut()
        .entity_mut(terrain)
        .insert((RigidBody::Static, Collider::cuboid(40.0, 0.01, 40.0)));
    // An unmarked tunnel under it: floor top at 0, ceiling from 3.
    cuboid(&mut scene, -0.5, 1.0);
    cuboid(&mut scene, 3.15, 0.3);
    for _ in 0..5 {
        scene.app.update();
    }
    let plan = ground::plan_lift(
        scene.app.world_mut(),
        DVec3::new(1.0, 1.0, 1.0),
        2.0,
        GroundGuardMode::Both,
    );
    assert!(near(plan.ground.surface, 0.0), "{plan:?}");
    assert!(near(plan.ceiling, 3.0), "{plan:?}");
    assert!(near(plan.target, 2.0), "{plan:?}");
}

#[test]
fn under_marked_terrain_a_floating_island_above_does_not_win() {
    for origin in ORIGINS {
        let mut scene = Scene::new(origin, false);
        scene.plane(DVec3::new(0.0, 0.0, 0.0), 40.0, true);
        scene.plane(DVec3::new(0.0, 20.0, 0.0), 10.0, true);
        scene.settle();
        let plan = plan_lift(scene.app.world_mut(), DVec3::new(1.0, -1.0, 1.0), 2.0);
        assert!(near(plan.ground.surface, 0.0), "{origin:?}: {plan:?}");
        assert!(near(plan.target, 2.0), "{origin:?}: {plan:?}");
    }
}

#[test]
fn a_collider_backed_marked_mesh_counts_for_meshes_alone() {
    let mut scene = Scene::new(ORIGINS[1], true);
    // Marked terrain at 4 that is both a render mesh and a collider, as in the example game.
    let terrain = scene.plane(DVec3::new(0.0, 4.0, 0.0), 40.0, true);
    scene
        .app
        .world_mut()
        .entity_mut(terrain)
        .insert((RigidBody::Static, Collider::cuboid(40.0, 0.01, 40.0)));
    for _ in 0..5 {
        scene.app.update();
    }
    let point = DVec3::new(1.0, 2.0, 1.0);
    let world = scene.app.world_mut();
    // The mesh probe alone guards it.
    let plan = ground::plan_lift(world, point, 2.0, GroundGuardMode::Meshes);
    assert!(near(plan.ground.surface, 4.0), "{plan:?}");
    assert_eq!(plan.ground.from, Some(ProbeKind::Meshes));
    assert!(near(plan.target, 6.0), "{plan:?}");
    // With both, the colliders probe answers for it (its 1 cm slab tops out at 4.005).
    let plan = ground::plan_lift(world, point, 2.0, GroundGuardMode::Both);
    assert!(near(plan.ground.surface, 4.005), "{plan:?}");
    assert_eq!(plan.ground.from, Some(ProbeKind::Colliders));
}

/// What a query in `Last` saw, the way a pending placement asks.
#[derive(Resource, Default)]
struct SeenInLast(Option<f64>);

fn ask_in_last(world: &mut World) {
    let point = DVec3::new(20.0, 5.0, 0.0);
    let ground = ground::ground_below(world, point, GroundGuardMode::Meshes).surface;
    world.resource_mut::<SeenInLast>().0 = ground;
}

#[test]
fn a_mesh_modified_this_frame_is_seen_by_a_query_in_last() {
    let mut scene = Scene::new(ORIGINS[0], false);
    scene
        .app
        .init_resource::<SeenInLast>()
        .add_systems(Last, ask_in_last);
    let mesh = Plane3d::default().mesh().size(10.0, 10.0).build();
    let handle = scene.add_mesh(mesh);
    scene.mesh_entity(DVec3::ZERO, handle.clone(), None, true);
    scene.settle();
    assert_eq!(scene.app.world().resource::<SeenInLast>().0, None);
    // Moved out of the old bounds, in the same frame the query in `Last` runs.
    let moved = Plane3d::default()
        .mesh()
        .size(10.0, 10.0)
        .build()
        .translated_by(Vec3::new(20.0, 2.0, 0.0));
    scene
        .app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .insert(&handle, moved)
        .unwrap();
    scene.app.update();
    let seen = scene.app.world().resource::<SeenInLast>().0;
    assert!(near(seen, 2.0), "{seen:?}");
}
