//! `PhysicsIntegrationPlugin` inside one big_space grid: bodies keep every tick of motion on frames
//! with several fixed ticks, follow origin shifts and `Transform` edits on the next tick, and new
//! bodies and colliders start at their cell rather than cells away.

use std::time::Duration;

use avian3d::prelude::{
    AngularVelocity, Collider, ColliderAabb, ColliderTransform, Gravity, LinearVelocity,
    PhysicsSystems, Position, RigidBody, Rotation, Sleeping,
};
use bevy::{
    asset::AssetPlugin,
    ecs::change_detection::Tick,
    math::{DQuat, DVec3},
    mesh::MeshPlugin,
    prelude::*,
    time::TimeUpdateStrategy,
};
use big_space::prelude::{BigSpace, BigSpaceDefaultPlugins, CellCoord, FloatingOrigin, Grid};
use ename_engine::physics::PhysicsIntegrationPlugin;

const EDGE: f32 = 100.0;

/// Frame steps at the default 64 Hz fixed rate: 2 or 3, 1, and 0 or 1 ticks per frame.
const FRAME_STEPS: [f64; 3] = [1.0 / 30.0, 1.0 / 64.0, 1.0 / 144.0];

/// Floating-origin placements: cell and in-cell offset.
const ORIGINS: [([i64; 3], [f32; 3]); 3] = [
    ([0, 0, 0], [0.0, 0.0, 0.0]),
    ([3, 0, -2], [12.0, 4.0, -7.0]),
    ([-6, 2, 9], [-31.0, 8.0, 44.0]),
];

const TOLERANCE: f64 = 1e-4;
/// Fixed time each tracking test runs for: long enough for the crossing body to change cell.
const RUN_SECONDS: f64 = 1.2;

/// The absolute position of the floating origin at the last fixed tick: the frame `Position` is
/// in.
#[derive(Resource, Default)]
struct TickOrigin(DVec3);

fn record_tick_origin(grid: Single<&Grid, With<BigSpace>>, mut tick_origin: ResMut<TickOrigin>) {
    tick_origin.0 = origin_of(&grid);
}

fn origin_of(grid: &Grid) -> DVec3 {
    let origin = grid.local_floating_origin();
    grid.cell_to_float(&origin.cell()) + origin.translation().as_dvec3()
}

struct Scene {
    app: App,
    root: Entity,
    origin: Entity,
}

impl Scene {
    /// The engine's physics integration on a headless app with zero gravity, stepping
    /// `frame_step` seconds per update.
    fn new(frame_step: f64, origin: ([i64; 3], [f32; 3])) -> Self {
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
            frame_step,
        )))
        .init_resource::<TickOrigin>()
        .add_systems(FixedLast, record_tick_origin);
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
        let origin = world
            .spawn((
                ChildOf(root),
                FloatingOrigin,
                CellCoord::new(x, y, z),
                Transform::from_translation(Vec3::from(offset)),
            ))
            .id();
        Self { app, root, origin }
    }

    fn grid(&self) -> &Grid {
        self.app.world().get::<Grid>(self.root).unwrap()
    }

    /// Absolute position from `CellCoord` and `Transform`.
    fn absolute(&self, entity: Entity) -> DVec3 {
        let world = self.app.world();
        let cell = world.get::<CellCoord>(entity).unwrap();
        let transform = world.get::<Transform>(entity).unwrap();
        self.grid().cell_to_float(cell) + transform.translation.as_dvec3()
    }

    /// Absolute position from avian's `Position`, in the frame of the last fixed tick.
    fn physics_absolute(&self, entity: Entity) -> DVec3 {
        let world = self.app.world();
        world.resource::<TickOrigin>().0 + world.get::<Position>(entity).unwrap().0.as_dvec3()
    }

    fn origin_cell(&self) -> CellCoord {
        *self.app.world().get::<CellCoord>(self.origin).unwrap()
    }

    /// Moves the floating origin by `cells`; big_space applies it in the next `PostUpdate`.
    fn shift_origin(&mut self, cells: CellCoord) {
        let origin = self.origin;
        *self.app.world_mut().get_mut::<CellCoord>(origin).unwrap() += cells;
    }

    fn fixed_elapsed(&self) -> f64 {
        self.app
            .world()
            .resource::<Time<Fixed>>()
            .elapsed_secs_f64()
    }

    /// A body with a small sphere collider in `cell` at `translation`, moving at `velocity`.
    fn body(
        &mut self,
        rigid_body: RigidBody,
        cell: CellCoord,
        translation: Vec3,
        velocity: Vec3,
    ) -> Entity {
        let root = self.root;
        self.app
            .world_mut()
            .spawn((
                ChildOf(root),
                cell,
                Transform::from_translation(translation),
                rigid_body,
                Collider::sphere(0.5),
                LinearVelocity(velocity),
            ))
            .id()
    }

    /// A body at `offset` from the floating origin, moving at `velocity`.
    fn body_near_origin(&mut self, rigid_body: RigidBody, offset: Vec3, velocity: Vec3) -> Entity {
        let origin = self.absolute(self.origin);
        let (cell, translation) = self.grid().translation_to_grid(origin + offset.as_dvec3());
        self.body(rigid_body, cell, translation, velocity)
    }

    /// Runs at least 5 frames and 5 fixed ticks, so new entities are initialised and have a
    /// tick history.
    fn settle(&mut self) {
        for _ in 0..5 {
            self.app.update();
        }
        while self.fixed_elapsed() < 5.0 / 64.0 {
            self.app.update();
        }
    }

    /// Updates until at least one fixed tick has run.
    fn tick(&mut self) {
        let before = self.fixed_elapsed();
        for _ in 0..10 {
            self.app.update();
            if self.fixed_elapsed() > before {
                return;
            }
        }
        panic!("no fixed tick in 10 frames");
    }
}

/// Runs `check` on a fresh scene for every frame step and floating-origin placement.
fn each_setup(check: impl Fn(&mut Scene, &str)) {
    for frame_step in FRAME_STEPS {
        for origin in ORIGINS {
            let mut scene = Scene::new(frame_step, origin);
            check(
                &mut scene,
                &format!("frame step {frame_step}, origin {origin:?}"),
            );
        }
    }
}

/// Distance between two rotations, about half the angle between them in radians.
fn rotation_error(a: Quat, b: Quat) -> f64 {
    f64::from((a - b).length().min((a + b).length()))
}

/// A body expected to move at constant linear and angular velocity from its pose at fixed time
/// `since`.
struct Track {
    entity: Entity,
    start: DVec3,
    start_rotation: DQuat,
    velocity: DVec3,
    angular_velocity: DVec3,
    since: f64,
}

impl Track {
    fn new(scene: &Scene, entity: Entity) -> Self {
        let world = scene.app.world();
        Self {
            entity,
            start: scene.absolute(entity),
            start_rotation: world.get::<Transform>(entity).unwrap().rotation.as_dquat(),
            velocity: world.get::<LinearVelocity>(entity).unwrap().0.as_dvec3(),
            angular_velocity: world.get::<AngularVelocity>(entity).unwrap().0.as_dvec3(),
            since: scene.fixed_elapsed(),
        }
    }

    /// Distances of the body's `Transform` and avian pose from the track: translations in metres,
    /// rotations as [`rotation_error`].
    fn errors(&self, scene: &Scene) -> [f64; 4] {
        let world = scene.app.world();
        let elapsed = scene.fixed_elapsed() - self.since;
        let expected = self.start + self.velocity * elapsed;
        let expected_rotation = (DQuat::from_scaled_axis(self.angular_velocity * elapsed)
            * self.start_rotation)
            .as_quat();
        [
            scene.absolute(self.entity).distance(expected),
            scene.physics_absolute(self.entity).distance(expected),
            rotation_error(
                world.get::<Transform>(self.entity).unwrap().rotation,
                expected_rotation,
            ),
            rotation_error(
                world.get::<Rotation>(self.entity).unwrap().0,
                expected_rotation,
            ),
        ]
    }

    fn assert_followed(&self, scene: &Scene, context: &str) {
        let errors = self.errors(scene);
        assert!(
            errors.iter().all(|&error| error < TOLERANCE),
            "{context}: off by [transform, position, transform rotation, rotation] {errors:?}",
        );
    }
}

/// Moving and spinning dynamic and kinematic bodies, and a kinematic body that crosses a cell
/// boundary (returned last).
fn moving_bodies(scene: &mut Scene) -> Vec<Entity> {
    let origin_cell = scene.origin_cell();
    let bodies = vec![
        scene.body_near_origin(
            RigidBody::Dynamic,
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(3.0, 0.5, -1.0),
        ),
        scene.body_near_origin(
            RigidBody::Kinematic,
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(-2.0, 0.0, 4.0),
        ),
        // Recentered into the next cell after about 0.7 s.
        scene.body(
            RigidBody::Kinematic,
            origin_cell,
            Vec3::new(48.0, 2.0, 10.0),
            Vec3::new(3.0, 0.0, 0.0),
        ),
    ];
    let world = scene.app.world_mut();
    world
        .entity_mut(bodies[0])
        .insert(AngularVelocity(Vec3::new(0.0, 1.0, 0.3)));
    world
        .entity_mut(bodies[1])
        .insert(AngularVelocity(Vec3::new(-0.8, 0.0, 0.6)));
    bodies
}

/// Runs frames for [`RUN_SECONDS`] of fixed time, checking `tracks` after each, and calls
/// `midway` once halfway.
fn follow(scene: &mut Scene, tracks: &[Track], context: &str, mut midway: impl FnMut(&mut Scene)) {
    let start = scene.fixed_elapsed();
    let mut midway_done = false;
    let mut frame = 0;
    while scene.fixed_elapsed() - start < RUN_SECONDS {
        scene.app.update();
        frame += 1;
        for track in tracks {
            track.assert_followed(scene, &format!("{context}, frame {frame}"));
        }
        if !midway_done && scene.fixed_elapsed() - start > RUN_SECONDS / 2.0 {
            midway_done = true;
            midway(scene);
        }
    }
}

fn assert_crossed(scene: &Scene, body: Entity, cell: CellCoord, context: &str) {
    let now = *scene.app.world().get::<CellCoord>(body).unwrap();
    assert_ne!(now, cell, "{context}: the crossing body stayed in its cell");
}

#[test]
fn moving_bodies_keep_every_tick_of_motion() {
    each_setup(|scene, context| {
        let bodies = moving_bodies(scene);
        let crossing = *bodies.last().unwrap();
        scene.settle();
        let cell = *scene.app.world().get::<CellCoord>(crossing).unwrap();
        let tracks: Vec<_> = bodies.iter().map(|&body| Track::new(scene, body)).collect();
        follow(scene, &tracks, context, |_| {});
        assert_crossed(scene, crossing, cell, context);
    });
}

#[test]
fn an_origin_shift_moves_every_position_by_the_shift() {
    each_setup(|scene, context| {
        let mut bodies = moving_bodies(scene);
        let crossing = *bodies.last().unwrap();
        let resting =
            scene.body_near_origin(RigidBody::Dynamic, Vec3::new(5.0, 0.0, 0.0), Vec3::ZERO);
        bodies.push(resting);
        scene.settle();
        let cell = *scene.app.world().get::<CellCoord>(crossing).unwrap();
        let tracks: Vec<_> = bodies.iter().map(|&body| Track::new(scene, body)).collect();
        follow(scene, &tracks, context, |scene| {
            let before = scene.app.world().get::<Position>(resting).unwrap().0;
            scene.shift_origin(CellCoord::new(1, -1, 0));
            // This frame's ticks still run in the old frame; the shift lands in PostUpdate.
            scene.app.update();
            scene.tick();
            let after = scene.app.world().get::<Position>(resting).unwrap().0;
            let shift = Vec3::new(-EDGE, EDGE, 0.0);
            assert!(
                (after - before - shift).length() < TOLERANCE as f32,
                "{context}: resting body moved by {} instead of {shift}",
                after - before,
            );
        });
        assert_crossed(scene, crossing, cell, context);
    });
}

#[test]
fn a_transform_teleport_reaches_the_physics_pose_on_the_next_tick() {
    each_setup(|scene, context| {
        let bodies = [
            scene.body_near_origin(RigidBody::Dynamic, Vec3::new(3.0, 0.0, 0.0), Vec3::ZERO),
            scene.body_near_origin(RigidBody::Kinematic, Vec3::new(-3.0, 0.0, 0.0), Vec3::ZERO),
            scene.body_near_origin(RigidBody::Static, Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO),
        ];
        scene.settle();
        let origin_cell = scene.origin_cell();
        let targets = [
            (
                origin_cell + CellCoord::new(0, 0, 1),
                Transform::from_xyz(-20.0, 1.0, -45.0).with_rotation(Quat::from_rotation_y(2.0)),
            ),
            (
                origin_cell,
                Transform::from_xyz(30.0, -2.0, 7.0).with_rotation(Quat::from_euler(
                    EulerRot::YXZ,
                    -1.0,
                    0.4,
                    0.2,
                )),
            ),
            (
                origin_cell + CellCoord::new(-1, 0, 0),
                Transform::from_xyz(10.0, 5.0, -3.0).with_rotation(Quat::from_rotation_x(0.7)),
            ),
        ];
        for (&body, (cell, transform)) in bodies.iter().zip(targets) {
            let world = scene.app.world_mut();
            *world.get_mut::<CellCoord>(body).unwrap() = cell;
            *world.get_mut::<Transform>(body).unwrap() = transform;
        }
        scene.tick();
        for (&body, (cell, transform)) in bodies.iter().zip(targets) {
            let target = scene.grid().cell_to_float(&cell) + transform.translation.as_dvec3();
            let error = scene.physics_absolute(body).distance(target);
            assert!(error < TOLERANCE, "{context}: position off by {error} m");
            let rotation = scene.app.world().get::<Rotation>(body).unwrap().0;
            let error = rotation_error(rotation, transform.rotation);
            assert!(error < TOLERANCE, "{context}: rotation off by {error}");
        }
    });
}

/// The probed entity's `Transform` and `GlobalTransform` change ticks, as seen at the end of the
/// last fixed tick.
#[derive(Resource)]
struct ChangeProbe {
    entity: Entity,
    changed: [Tick; 2],
}

fn record_probe(
    mut probe: ResMut<ChangeProbe>,
    transforms: Query<(Ref<Transform>, Ref<GlobalTransform>)>,
) {
    let (transform, global_transform) = transforms.get(probe.entity).unwrap();
    probe.changed = [transform.last_changed(), global_transform.last_changed()];
}

#[test]
fn a_resting_static_body_is_not_written() {
    let mut scene = Scene::new(1.0 / 30.0, ORIGINS[1]);
    let body = scene.body_near_origin(RigidBody::Static, Vec3::ZERO, Vec3::ZERO);
    scene.settle();
    let entity = scene.app.world().entity(body);
    let changed = [
        entity.get_ref::<Transform>().unwrap().last_changed(),
        entity.get_ref::<GlobalTransform>().unwrap().last_changed(),
    ];
    scene
        .app
        .insert_resource(ChangeProbe {
            entity: body,
            changed,
        })
        .add_systems(FixedLast, record_probe);

    for _ in 0..5 {
        scene.tick();
    }
    let probed = scene.app.world().resource::<ChangeProbe>().changed;
    assert_eq!(
        probed, changed,
        "the static body's [Transform, GlobalTransform] was written in a tick"
    );
}

/// Spawns a collider in the cell next to the floating origin's, in the fixed schedule, once; with
/// a static body if the second field is set.
#[derive(Resource)]
struct SpawnCollider {
    spawned: Option<Entity>,
    with_static_body: bool,
}

fn spawn_collider_mid_frame(
    mut commands: Commands,
    mut spawn: ResMut<SpawnCollider>,
    origins: Query<&CellCoord, With<FloatingOrigin>>,
    roots: Query<Entity, With<Grid>>,
) {
    if spawn.spawned.is_some() {
        return;
    }
    let cell = *origins.single().unwrap() + CellCoord::new(1, 0, 0);
    let root = roots.iter().next().unwrap();
    let mut entity = commands.spawn((
        ChildOf(root),
        cell,
        Transform::from_xyz(3.0, 1.0, -2.0),
        Collider::sphere(0.5),
    ));
    if spawn.with_static_body {
        entity.insert(RigidBody::Static);
    }
    spawn.spawned = Some(entity.id());
}

/// The farthest the spawned collider's `Position` was from its grid pose, checked after each
/// `Prepare`.
#[derive(Resource, Default)]
struct PlacementError {
    checks: usize,
    worst: f32,
}

fn check_placement(
    spawn: Res<SpawnCollider>,
    mut error: ResMut<PlacementError>,
    grids: Query<&Grid>,
    colliders: Query<(&Position, &ChildOf, &CellCoord, &Transform)>,
) {
    let Some(collider) = spawn.spawned else {
        return;
    };
    let Ok((position, child_of, cell, transform)) = colliders.get(collider) else {
        return;
    };
    let grid = grids.get(child_of.parent()).unwrap();
    let expected = grid.global_transform(cell, transform).translation();
    error.checks += 1;
    error.worst = error.worst.max(position.0.distance(expected));
}

/// A scene with [`spawn_collider_mid_frame`] and [`check_placement`] installed. Two ticks a frame:
/// the second runs before any propagation has seen the new collider.
fn placement_scene(with_static_body: bool) -> Scene {
    let mut scene = Scene::new(1.0 / 30.0, ORIGINS[1]);
    scene.settle();
    scene
        .app
        .insert_resource(SpawnCollider {
            spawned: None,
            with_static_body,
        })
        .init_resource::<PlacementError>()
        .add_systems(FixedUpdate, spawn_collider_mid_frame)
        .add_systems(
            FixedPostUpdate,
            check_placement
                .after(PhysicsSystems::Prepare)
                .before(PhysicsSystems::StepSimulation),
        );
    scene
}

fn assert_placed(scene: &Scene, what: &str) {
    let error = scene.app.world().resource::<PlacementError>();
    assert!(error.checks > 2);
    assert!(
        error.worst < 1e-3,
        "{what} was {} m from its grid pose",
        error.worst
    );
}

#[test]
fn a_static_body_spawned_mid_frame_is_placed_at_its_cell_on_its_first_tick() {
    let mut scene = placement_scene(true);
    for _ in 0..4 {
        scene.app.update();
    }
    assert_placed(&scene, "the new body");
}

#[test]
fn a_collider_without_a_body_spawned_mid_frame_is_placed_at_its_cell() {
    let mut scene = placement_scene(false);
    for _ in 0..4 {
        scene.app.update();
    }
    assert_placed(&scene, "the collider");
}

/// Gives the spawned collider a body, in the fixed schedule, after it has been through a few
/// physics steps.
#[derive(Resource)]
struct AddBody {
    body: RigidBody,
    added: bool,
    ticks: u32,
}

fn add_body_mid_frame(
    mut commands: Commands,
    spawn: Res<SpawnCollider>,
    mut add: ResMut<AddBody>,
    colliders: Query<(), With<Position>>,
) {
    let Some(collider) = spawn
        .spawned
        .filter(|&collider| colliders.contains(collider))
    else {
        return;
    };
    if add.added {
        return;
    }
    add.ticks += 1;
    if add.ticks < 3 {
        return;
    }
    add.added = true;
    commands.entity(collider).insert(add.body);
}

#[test]
fn a_body_added_to_a_placed_collider_stays_at_its_cell() {
    for body in [RigidBody::Static, RigidBody::Dynamic, RigidBody::Kinematic] {
        let mut scene = placement_scene(false);
        scene
            .app
            .insert_resource(AddBody {
                body,
                added: false,
                ticks: 0,
            })
            .add_systems(
                FixedUpdate,
                add_body_mid_frame.after(spawn_collider_mid_frame),
            );
        for _ in 0..6 {
            scene.app.update();
        }
        assert!(
            scene.app.world().resource::<AddBody>().added,
            "{body:?}: never added"
        );
        assert_placed(&scene, &format!("{body:?}: the body"));
    }
}

type WorldEdit = Box<dyn FnOnce(&mut World) + Send + Sync>;

/// A world edit run once, in `FixedUpdate`, on the `tick`th fixed tick after it was scheduled.
#[derive(Resource)]
struct AtTick {
    tick: u32,
    seen: u32,
    action: Option<WorldEdit>,
}

fn run_at_tick(world: &mut World) {
    let mut at = world.resource_mut::<AtTick>();
    at.seen += 1;
    if at.seen < at.tick {
        return;
    }
    if let Some(action) = at.action.take() {
        action(world);
    }
}

impl Scene {
    /// Runs `action` in `FixedUpdate` on the `tick`th fixed tick from now, mid-frame when the
    /// frame step holds two ticks.
    fn at_tick(&mut self, tick: u32, action: impl FnOnce(&mut World) + Send + Sync + 'static) {
        self.app
            .insert_resource(AtTick {
                tick,
                seen: 0,
                action: Some(Box::new(action)),
            })
            .add_systems(FixedUpdate, run_at_tick);
    }

    fn ran_at_tick(&self) -> bool {
        self.app.world().resource::<AtTick>().action.is_none()
    }

    /// Updates until the scheduled action has run, then for `frames` more frames.
    fn run_past_action(&mut self, frames: usize) {
        for _ in 0..20 {
            if self.ran_at_tick() {
                break;
            }
            self.app.update();
        }
        assert!(self.ran_at_tick(), "the scheduled action never ran");
        for _ in 0..frames {
            self.app.update();
        }
    }

    fn assert_pose_agrees(&self, entity: Entity, context: &str) {
        let error = self
            .physics_absolute(entity)
            .distance(self.absolute(entity));
        assert!(
            error < TOLERANCE,
            "{context}: Position and CellCoord/Transform disagree by {error} m"
        );
    }
}

#[test]
fn a_scaled_collider_spawned_mid_frame_keeps_its_scale() {
    let mut scene = Scene::new(1.0 / 30.0, ORIGINS[1]);
    scene.settle();
    let root = scene.root;
    let cell = scene.origin_cell() + CellCoord::new(1, 0, -1);
    scene.at_tick(2, move |world| {
        world.spawn((
            Name::new("scaled static"),
            ChildOf(root),
            cell,
            Transform::from_xyz(3.0, 1.0, -2.0).with_scale(Vec3::splat(2.0)),
            RigidBody::Static,
            Collider::sphere(0.5),
        ));
        let body = world
            .spawn((
                Name::new("scaled body"),
                ChildOf(root),
                cell,
                Transform::from_xyz(-3.0, 1.0, 2.0)
                    .with_scale(Vec3::splat(2.0))
                    .with_rotation(Quat::from_rotation_y(1.0)),
                RigidBody::Kinematic,
            ))
            .id();
        world.spawn((
            Name::new("child collider"),
            ChildOf(body),
            Transform::from_xyz(1.0, 0.0, 0.0),
            Collider::sphere(0.5),
        ));
    });
    scene.run_past_action(4);

    let world = scene.app.world_mut();
    let mut colliders = world.query::<(&Name, &ColliderTransform, &ColliderAabb, &Position)>();
    let mut checked = 0;
    for (name, collider_transform, aabb, position) in colliders.iter(world) {
        let half_extent = (aabb.max - aabb.min) / 2.0;
        assert!(
            (collider_transform.scale - Vec3::splat(2.0)).length() < 1e-4
                && (half_extent - Vec3::splat(1.0)).length() < 0.05
                && ((aabb.max + aabb.min) / 2.0 - position.0).length() < 1e-3,
            "{name}: {collider_transform:?}, AABB half extent {half_extent}"
        );
        if name.as_str() == "child collider" {
            assert!(
                (collider_transform.translation - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4,
                "{name}: offset from its body {}",
                collider_transform.translation
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 2);
}

#[test]
fn a_position_set_when_a_body_joins_a_collider_is_kept() {
    let mut scene = Scene::new(1.0 / 30.0, ORIGINS[1]);
    let collider = scene.body_near_origin(RigidBody::Static, Vec3::new(4.0, 0.0, 0.0), Vec3::ZERO);
    scene
        .app
        .world_mut()
        .entity_mut(collider)
        .remove::<RigidBody>();
    scene.settle();
    let target = scene.app.world().get::<Position>(collider).unwrap().0 + Vec3::new(10.0, 0.0, 0.0);
    scene.at_tick(2, move |world| {
        world
            .entity_mut(collider)
            .insert((RigidBody::Kinematic, Position(target)));
    });
    scene.run_past_action(0);
    let position = scene.app.world().get::<Position>(collider).unwrap().0;
    assert!(
        position.distance(target) < 1e-3,
        "the explicit Position was replaced by {position}"
    );
    scene.app.update();
    scene.assert_pose_agrees(collider, "a frame later");
}

#[test]
fn a_position_written_directly_moves_the_body() {
    each_setup(|scene, context| {
        let body = scene.body_near_origin(RigidBody::Dynamic, Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO);
        scene.settle();
        let start = scene.absolute(body);
        scene.at_tick(2, move |world| {
            world.get_mut::<Position>(body).unwrap().0 += Vec3::new(60.0, 0.0, 0.0);
        });
        scene.run_past_action(2);
        let moved = scene.absolute(body) - start;
        assert!(
            (moved - DVec3::new(60.0, 0.0, 0.0)).length() < TOLERANCE,
            "{context}: the body moved by {moved}"
        );
        scene.assert_pose_agrees(body, context);
    });
}

#[test]
fn a_sleeping_body_follows_an_origin_shift_and_a_teleport() {
    let mut scene = Scene::new(1.0 / 30.0, ORIGINS[1]);
    let body = scene.body_near_origin(RigidBody::Dynamic, Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO);
    for _ in 0..200 {
        if scene.app.world().entity(body).contains::<Sleeping>() {
            break;
        }
        scene.app.update();
    }
    assert!(scene.app.world().entity(body).contains::<Sleeping>());

    scene.shift_origin(CellCoord::new(-1, 0, 2));
    scene.app.update();
    scene.tick();
    scene.assert_pose_agrees(body, "after the origin shift");

    *scene.app.world_mut().get_mut::<Transform>(body).unwrap() =
        Transform::from_xyz(7.0, -3.0, 9.0);
    scene.tick();
    scene.assert_pose_agrees(body, "after the teleport");
}

#[test]
fn a_static_body_reparented_to_another_grid_moves_with_it() {
    let mut scene = Scene::new(1.0 / 30.0, ORIGINS[1]);
    let root = scene.root;
    let sub_cell = scene.origin_cell() + CellCoord::new(1, 0, 0);
    let sub_offset = Vec3::new(30.0, 5.0, -10.0);
    let sub_grid = scene
        .app
        .world_mut()
        .spawn((
            Grid::new(EDGE, 0.0),
            sub_cell,
            Transform::from_translation(sub_offset),
            ChildOf(root),
        ))
        .id();
    let body = scene.body_near_origin(RigidBody::Static, Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO);
    scene.settle();
    let cell = *scene.app.world().get::<CellCoord>(body).unwrap();
    let translation = scene
        .app
        .world()
        .get::<Transform>(body)
        .unwrap()
        .translation;

    scene.at_tick(1, move |world| {
        world.entity_mut(body).insert(ChildOf(sub_grid));
    });
    scene.run_past_action(0);

    let expected = scene.grid().cell_to_float(&sub_cell)
        + sub_offset.as_dvec3()
        + scene.grid().cell_to_float(&cell)
        + translation.as_dvec3();
    let error = scene.physics_absolute(body).distance(expected);
    assert!(error < TOLERANCE, "the body's Position is {error} m off");
}

#[test]
fn a_body_switched_to_static_mid_frame_stops_where_it_is() {
    each_setup(|scene, context| {
        let body = scene.body_near_origin(
            RigidBody::Dynamic,
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(3.0, 0.0, 1.0),
        );
        scene.settle();
        scene.at_tick(2, move |world| {
            world.entity_mut(body).insert(RigidBody::Static);
        });
        scene.run_past_action(1);
        let stopped = scene.absolute(body);
        scene.assert_pose_agrees(body, context);
        for _ in 0..10 {
            scene.app.update();
        }
        let drift = scene.absolute(body).distance(stopped);
        assert!(
            drift < TOLERANCE,
            "{context}: the static body drifted {drift} m"
        );
        scene.assert_pose_agrees(body, context);
    });
}
