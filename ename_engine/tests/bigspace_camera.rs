use bevy::prelude::*;
use big_space::plugin::BigSpaceDefaultPlugins;
use big_space::prelude::{BigSpace, BigSpaceCameraController, CellCoord, FloatingOrigin, Grid};
use ename_engine::bigspace::{
    BigSpacePlugin, FloatingOriginCandidate, FrozenOrigin, GridFollowCamera, root_frame_pose,
    set_origin_frozen,
};

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(bevy::input::InputPlugin)
        .add_plugins(BigSpaceDefaultPlugins)
        .add_plugins(BigSpacePlugin);
    app
}

/// Spawns a root `BigSpace` and a candidate of the given priority already parented to it, skipping
/// `GridFollowCamera`'s attach step so a test can set up several candidates in one known cell.
fn spawn_candidate(app: &mut App, root: Entity, priority: i32) -> Entity {
    app.world_mut()
        .spawn((
            FloatingOriginCandidate(priority),
            CellCoord::default(),
            Transform::default(),
            ChildOf(root),
        ))
        .id()
}

fn holds_origin(app: &App, entity: Entity) -> bool {
    app.world().entity(entity).contains::<FloatingOrigin>()
}

#[test]
fn a_grid_follow_camera_with_no_grid_present_stays_parentless_and_gains_no_cell_coord() {
    let mut app = test_app();
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::default()))
        .id();

    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert!(entity.contains::<BigSpaceCameraController>());
    assert!(!entity.contains::<CellCoord>());
    assert!(!entity.contains::<bevy::prelude::ChildOf>());
}

#[test]
fn a_grid_follow_camera_attaches_to_a_grid_that_appears_preserving_world_position() {
    let mut app = test_app();
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::from_xyz(3.0, 1.0, -2.0)))
        .id();
    app.update();

    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert!(entity.contains::<CellCoord>());
    assert!(entity.contains::<FloatingOrigin>());
    assert!(entity.contains::<bevy::prelude::ChildOf>());

    let transform = entity.get::<Transform>().unwrap();
    assert!(transform.translation.distance(Vec3::new(3.0, 1.0, -2.0)) < 0.01);
}

#[test]
fn a_grid_follow_camera_detaches_when_its_grid_component_is_removed_preserving_position() {
    let mut app = test_app();
    let grid = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    // Use a position that crosses cell boundaries (cell (25,0,0) at default 2000 cell_edge_length)
    // so the test can't pass by coincidence.
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::from_xyz(50_000.0, 0.0, 0.0)))
        .id();
    app.update();
    app.update();
    assert!(app.world().entity(camera).contains::<CellCoord>());

    // Remove just the `Grid` component; the despawn path has tests of its own below.
    app.world_mut().entity_mut(grid).remove::<Grid>();
    // No app.update() needed: the observer's commands are applied when `remove` returns.

    let entity = app.world().entity(camera);
    assert!(!entity.contains::<ChildOf>());
    assert!(!entity.contains::<CellCoord>());
    assert!(!entity.contains::<FloatingOrigin>());
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        transform
            .translation
            .distance(Vec3::new(50_000.0, 0.0, 0.0))
            < 0.01,
        "position should be preserved at 50000.0, got {}",
        transform.translation.x
    );
}

#[test]
fn freezing_the_origin_moves_floating_origin_to_a_stationary_anchor() {
    let mut app = test_app();
    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    let camera = app
        .world_mut()
        .spawn((GridFollowCamera, Transform::default()))
        .id();
    app.update();
    app.update();
    assert!(app.world().entity(camera).contains::<FloatingOrigin>());

    set_origin_frozen(app.world_mut(), camera, true);

    assert!(!app.world().entity(camera).contains::<FloatingOrigin>());
    let FrozenOrigin(anchor) = *app.world().entity(camera).get::<FrozenOrigin>().unwrap();
    assert!(app.world().entity(anchor).contains::<FloatingOrigin>());

    set_origin_frozen(app.world_mut(), camera, false);

    assert!(app.world().entity(camera).contains::<FloatingOrigin>());
    assert!(!app.world().entity(camera).contains::<FrozenOrigin>());
    assert!(
        app.world().get_entity(anchor).is_err(),
        "anchor should be despawned"
    );
}

#[test]
fn the_highest_priority_candidate_under_a_root_holds_the_origin() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let low = spawn_candidate(&mut app, root, 0);
    let high = spawn_candidate(&mut app, root, 100);
    app.update();

    assert!(holds_origin(&app, high));
    assert!(!holds_origin(&app, low));
}

#[test]
fn raising_a_losers_priority_moves_the_origin_to_it() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let editor = spawn_candidate(&mut app, root, 100);
    let game = spawn_candidate(&mut app, root, 0);
    app.update();
    assert!(holds_origin(&app, editor));

    // What entering play-in-editor will do: the editor camera drops below the stage's own
    // candidate rather than the game camera being raised, but the election sees the same thing.
    *app.world_mut()
        .get_mut::<FloatingOriginCandidate>(editor)
        .unwrap() = FloatingOriginCandidate(-100);
    app.update();

    assert!(holds_origin(&app, game));
    assert!(!holds_origin(&app, editor));
}

#[test]
fn despawning_the_holder_hands_the_origin_to_the_remaining_candidate() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let winner = spawn_candidate(&mut app, root, 100);
    let runner_up = spawn_candidate(&mut app, root, 0);
    app.update();
    assert!(holds_origin(&app, winner));

    app.world_mut().entity_mut(winner).despawn();
    app.update();

    assert!(holds_origin(&app, runner_up));
}

#[test]
fn each_root_elects_its_own_origin_independently() {
    let mut app = test_app();
    let first = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let second = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let in_first = spawn_candidate(&mut app, first, 0);
    let in_second = spawn_candidate(&mut app, second, 0);
    app.update();

    assert!(holds_origin(&app, in_first));
    assert!(holds_origin(&app, in_second));
}

#[test]
fn a_candidate_that_is_not_grid_attached_never_holds_the_origin() {
    let mut app = test_app();
    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    // No `CellCoord` and no parent: eligible on paper, but not in any `BigSpace` hierarchy, which
    // is the same walk `find_floating_origin` does.
    let loose = app
        .world_mut()
        .spawn((FloatingOriginCandidate(1000), Transform::default()))
        .id();
    app.update();
    app.update();

    assert!(!holds_origin(&app, loose));
}

/// A camera attached to a fresh root grid, far enough out (cell 25 at the default 2000 m edge)
/// that a position lost to the floating origin cannot pass by coincidence.
fn attached_camera(app: &mut App) -> (Entity, Entity) {
    let grid = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let camera = spawn_far_camera(app);
    app.update();
    app.update();
    assert_eq!(
        app.world().get::<ChildOf>(camera).map(ChildOf::parent),
        Some(grid),
        "the camera must be attached before the grid goes away"
    );
    (grid, camera)
}

const FAR: Vec3 = Vec3::new(50_000.0, 3.0, -7.0);

fn spawn_far_camera(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            GridFollowCamera,
            Transform::from_translation(FAR).with_rotation(Quat::from_rotation_y(0.5)),
        ))
        .id()
}

/// The camera survived its grid, detached, at its old world pose.
fn assert_rescued(app: &App, camera: Entity) {
    let entity = app
        .world()
        .get_entity(camera)
        .expect("the camera was despawned with its grid");
    assert!(!entity.contains::<ChildOf>());
    assert!(!entity.contains::<CellCoord>());
    assert!(!entity.contains::<FloatingOrigin>());
    assert!(!entity.contains::<FrozenOrigin>());
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        transform.translation.distance(FAR) < 0.01,
        "the camera moved to {}",
        transform.translation
    );
    assert!(transform.rotation.angle_between(Quat::from_rotation_y(0.5)) < 1e-4);
}

/// A new root grid appears and the camera attaches to it at the same world position.
fn assert_reattaches(app: &mut App, camera: Entity) {
    let grid = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    app.update();
    app.update();
    assert_attached_at_far(app, camera, grid);
    assert!(app.world().entity(camera).contains::<FloatingOrigin>());
}

fn assert_attached_at_far(app: &App, camera: Entity, grid: Entity) {
    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(grid));
    let position = app.world().get::<Grid>(grid).unwrap().grid_position(
        entity.get::<CellCoord>().unwrap(),
        entity.get::<Transform>().unwrap(),
    );
    assert!(
        position.distance(FAR) < 0.01,
        "the camera reattached at {position}"
    );
}

#[test]
fn despawning_the_grid_detaches_its_camera_instead_of_despawning_it() {
    let mut app = test_app();
    let (grid, camera) = attached_camera(&mut app);

    app.world_mut().despawn(grid);

    assert_rescued(&app, camera);
    app.update();
    assert_rescued(&app, camera);
    assert_reattaches(&mut app, camera);
}

#[test]
fn despawning_the_grid_through_commands_detaches_its_camera() {
    let mut app = test_app();
    let (grid, camera) = attached_camera(&mut app);

    app.world_mut().commands().entity(grid).despawn();
    app.world_mut().flush();

    assert_rescued(&app, camera);
    assert_reattaches(&mut app, camera);
}

#[test]
fn despawning_the_grids_ancestor_detaches_its_camera() {
    let mut app = test_app();
    let container = app.world_mut().spawn(Transform::default()).id();
    let grid = app
        .world_mut()
        .spawn((
            big_space::bundles::BigSpaceRootBundle::default(),
            ChildOf(container),
        ))
        .id();
    let camera = spawn_far_camera(&mut app);
    app.update();
    app.update();
    assert_eq!(
        app.world().get::<ChildOf>(camera).map(ChildOf::parent),
        Some(grid)
    );

    app.world_mut().despawn(container);

    assert!(app.world().get_entity(grid).is_err());
    assert_rescued(&app, camera);
    assert_reattaches(&mut app, camera);
}

#[test]
fn despawning_the_grid_of_a_frozen_camera_detaches_it_and_drops_the_anchor() {
    let mut app = test_app();
    let (grid, camera) = attached_camera(&mut app);
    set_origin_frozen(app.world_mut(), camera, true);
    let FrozenOrigin(anchor) = *app.world().get::<FrozenOrigin>(camera).unwrap();

    app.world_mut().despawn(grid);

    assert!(app.world().get_entity(anchor).is_err());
    assert_rescued(&app, camera);
    assert_reattaches(&mut app, camera);
}

#[test]
fn removing_the_grid_component_detaches_the_camera_and_it_reattaches_to_a_new_grid() {
    let mut app = test_app();
    let (grid, camera) = attached_camera(&mut app);

    app.world_mut().entity_mut(grid).remove::<Grid>();

    assert_rescued(&app, camera);
    assert_reattaches(&mut app, camera);
}

/// What a stage file edit does in the editor: Bevy's hot reload despawns the instance of the
/// modified `DynamicWorld`, root grid included, and spawns it again.
#[test]
fn hot_reloading_the_grids_dynamic_world_moves_the_camera_to_the_new_grid() {
    use bevy::world_serialization::{
        DynamicWorld, DynamicWorldBuilder, DynamicWorldRoot, WorldSerializationPlugin,
    };

    let mut app = test_app();
    app.add_plugins((AssetPlugin::default(), WorldSerializationPlugin));

    let mut source = World::new();
    let root = source
        .spawn((BigSpace::default(), Grid::default(), Transform::default()))
        .id();
    let dynamic_world = {
        let registry = app.world().resource::<AppTypeRegistry>().read();
        DynamicWorldBuilder::from_world(&source, &registry)
            .extract_entity(root)
            .build()
    };
    let handle = app
        .world_mut()
        .resource_mut::<Assets<DynamicWorld>>()
        .add(dynamic_world);
    app.world_mut().spawn(DynamicWorldRoot(handle.clone()));
    let camera = spawn_far_camera(&mut app);
    let first_grid = wait_for_attachment(&mut app, camera, None);
    // The spawner ignores a `Modified` within three frames of the asset's `Added`.
    for _ in 0..4 {
        app.update();
    }

    // Marks the asset modified, as a file watcher's reload does.
    app.world_mut()
        .resource_mut::<Assets<DynamicWorld>>()
        .get_mut(&handle)
        .unwrap()
        .into_inner();
    let second_grid = wait_for_attachment(&mut app, camera, Some(first_grid));

    assert!(app.world().get_entity(first_grid).is_err());
    assert_attached_at_far(&app, camera, second_grid);
}

/// Updates until `camera` is attached to a grid other than `previous`, and returns that grid.
fn wait_for_attachment(app: &mut App, camera: Entity, previous: Option<Entity>) -> Entity {
    for _ in 0..20 {
        app.update();
        if let Some(parent) = app.world().get::<ChildOf>(camera).map(ChildOf::parent)
            && Some(parent) != previous
        {
            return parent;
        }
    }
    panic!("the camera never attached to a new grid");
}

#[test]
fn despawning_the_grid_of_a_follower_parented_without_a_cell_detaches_it() {
    let mut app = test_app();
    let grid = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    // Parented by hand, before `sync_grid_attachment` has given it a cell.
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            Transform::from_xyz(4.0, 0.0, 0.0),
            ChildOf(grid),
        ))
        .id();

    app.world_mut().despawn(grid);

    let entity = app
        .world()
        .get_entity(camera)
        .expect("the camera was despawned with its grid");
    assert!(!entity.contains::<ChildOf>());
    assert_eq!(
        entity.get::<Transform>().unwrap().translation,
        Vec3::new(4.0, 0.0, 0.0)
    );
}

#[test]
fn a_rescued_camera_attaches_to_the_root_grid_nearest_its_true_position() {
    let mut app = test_app();
    let (grid, camera) = attached_camera(&mut app);
    app.world_mut().despawn(grid);

    let near = app
        .world_mut()
        .spawn((
            big_space::bundles::BigSpaceRootBundle::default(),
            Transform::from_translation(FAR),
        ))
        .id();
    app.world_mut().spawn((
        big_space::bundles::BigSpaceRootBundle::default(),
        Transform::default(),
    ));
    app.update();
    app.update();

    assert_eq!(
        app.world().get::<ChildOf>(camera).map(ChildOf::parent),
        Some(near)
    );
}

#[test]
fn a_camera_under_an_ordinary_parent_attaches_at_its_composed_pose() {
    let mut app = test_app();
    let parent_rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    let parent = app
        .world_mut()
        .spawn(Transform::from_xyz(100.0, 0.0, 0.0).with_rotation(parent_rotation))
        .id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            Transform::from_xyz(5.0, 0.0, 0.0),
            ChildOf(parent),
        ))
        .id();
    app.update();
    let grid = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(grid));
    let transform = entity.get::<Transform>().unwrap();
    let position = app
        .world()
        .get::<Grid>(grid)
        .unwrap()
        .grid_position(entity.get::<CellCoord>().unwrap(), transform);
    let expected = Vec3::new(100.0, 0.0, 0.0) + parent_rotation * Vec3::new(5.0, 0.0, 0.0);
    assert!(position.distance(expected) < 1e-3, "attached at {position}");
    assert!(
        // Not `angle_between`, which is NaN for equal rotations once rounding pushes the dot
        // product past 1.
        transform.rotation.dot(parent_rotation).abs() > 1.0 - 1e-6,
        "rotation {:?}",
        transform.rotation
    );
}

#[test]
fn a_second_root_grid_at_the_same_place_does_not_take_an_attached_camera() {
    let mut app = test_app();
    let (grid, camera) = attached_camera(&mut app);
    app.world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default());
    app.update();
    app.update();
    assert_eq!(
        app.world().get::<ChildOf>(camera).map(ChildOf::parent),
        Some(grid)
    );
}

/// A root grid, a sub-grid in its cell (2, 0, 0) offset and turned 90° about Y, and a camera in
/// the sub-grid's cell (1, 0, 0) at local X = 5. With the default 2000 m cells the camera sits at
/// (4010, 0, -2005) in the root grid's frame, facing the sub-grid's way.
///
/// big_space applies neither the root grid's own `Transform` nor a grid's scale to the grid's
/// contents, so the root is moved to X = 100 and the sub-grid scaled by 2, and neither may show.
fn camera_in_sub_grid(app: &mut App) -> (Entity, Entity, Entity) {
    let root = app
        .world_mut()
        .spawn((
            big_space::bundles::BigSpaceRootBundle::default(),
            Transform::from_xyz(100.0, 0.0, 0.0),
        ))
        .id();
    let sub_grid = app
        .world_mut()
        .spawn((
            Grid::default(),
            CellCoord::new(2, 0, 0),
            Transform::from_xyz(10.0, 0.0, 0.0)
                .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))
                .with_scale(Vec3::splat(2.0)),
            ChildOf(root),
        ))
        .id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            CellCoord::new(1, 0, 0),
            Transform::from_xyz(5.0, 0.0, 0.0),
            ChildOf(sub_grid),
        ))
        .id();
    (root, sub_grid, camera)
}

const IN_SUB_GRID: Vec3 = Vec3::new(4010.0, 0.0, -2005.0);

fn assert_rotation_y_quarter(rotation: Quat) {
    let expected = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    assert!(
        rotation.dot(expected).abs() > 1.0 - 1e-6,
        "rotation {rotation:?}"
    );
}

#[test]
fn despawning_a_sub_grid_places_its_camera_in_the_root_grids_frame() {
    let mut app = test_app();
    let (_, sub_grid, camera) = camera_in_sub_grid(&mut app);

    app.world_mut().despawn(sub_grid);

    let entity = app.world().entity(camera);
    assert!(!entity.contains::<ChildOf>());
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        transform.translation.distance(IN_SUB_GRID) < 0.01,
        "rescued at {}",
        transform.translation
    );
    assert_rotation_y_quarter(transform.rotation);
}

#[test]
fn a_camera_in_a_sub_grid_moves_to_the_root_grid_at_the_same_place() {
    let mut app = test_app();
    let (root, _, camera) = camera_in_sub_grid(&mut app);
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(root));
    let transform = entity.get::<Transform>().unwrap();
    let position = app
        .world()
        .get::<Grid>(root)
        .unwrap()
        .grid_position(entity.get::<CellCoord>().unwrap(), transform);
    assert!(
        position.distance(IN_SUB_GRID) < 0.01,
        "attached at {position}"
    );
    assert_rotation_y_quarter(transform.rotation);
}

#[test]
fn despawning_the_root_above_a_sub_grid_places_its_camera_in_the_root_grids_frame() {
    let mut app = test_app();
    let (root, _, camera) = camera_in_sub_grid(&mut app);

    // The cascade reaches the sub-grid only after the root is gone.
    app.world_mut().despawn(root);

    let entity = app
        .world()
        .get_entity(camera)
        .expect("the camera was despawned with its grids");
    assert!(!entity.contains::<ChildOf>());
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        transform.translation.distance(IN_SUB_GRID) < 0.01,
        "rescued at {}",
        transform.translation
    );
    assert_rotation_y_quarter(transform.rotation);
}

#[test]
fn a_root_grids_own_transform_does_not_move_a_rescued_camera() {
    let mut app = test_app();
    let grid = app
        .world_mut()
        .spawn((
            big_space::bundles::BigSpaceRootBundle::default(),
            Transform::from_xyz(100.0, 0.0, 0.0),
        ))
        .id();
    let camera = spawn_far_camera(&mut app);
    app.update();
    app.update();
    assert_eq!(
        app.world().get::<ChildOf>(camera).map(ChildOf::parent),
        Some(grid)
    );

    app.world_mut().despawn(grid);

    assert_rescued(&app, camera);
}

#[test]
fn a_camera_under_a_scaled_then_turned_parent_attaches_where_it_renders() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let stretched = app
        .world_mut()
        .spawn((
            CellCoord::default(),
            Transform::from_xyz(5.0, 0.0, 0.0).with_scale(Vec3::new(2.0, 1.0, 1.0)),
            ChildOf(root),
        ))
        .id();
    let turned = app
        .world_mut()
        .spawn((
            Transform::from_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
            ChildOf(stretched),
        ))
        .id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            Transform::from_xyz(1.0, 0.0, 0.0),
            ChildOf(turned),
        ))
        .id();
    app.update();
    app.update();

    // The turn maps local X onto Y, which the stretch along X leaves alone, as an affine does.
    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(root));
    let position = app.world().get::<Grid>(root).unwrap().grid_position(
        entity.get::<CellCoord>().unwrap(),
        entity.get::<Transform>().unwrap(),
    );
    assert!(
        position.distance(Vec3::new(5.0, 1.0, 0.0)) < 1e-3,
        "attached at {position}"
    );
}

#[test]
fn a_camera_under_a_sheared_parent_gets_a_unit_rotation() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let stretched = app
        .world_mut()
        .spawn((
            CellCoord::default(),
            Transform::from_scale(Vec3::new(2.0, 1.0, 1.0)),
            ChildOf(root),
        ))
        .id();
    let turned = app
        .world_mut()
        .spawn((
            Transform::from_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_4)),
            ChildOf(stretched),
        ))
        .id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            Transform::from_xyz(1.0, 0.0, 0.0),
            ChildOf(turned),
        ))
        .id();
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(root));
    let transform = entity.get::<Transform>().unwrap();
    assert!(
        (transform.rotation.length() - 1.0).abs() < 1e-5,
        "rotation {:?} is not a unit quaternion",
        transform.rotation
    );
    let half = std::f32::consts::FRAC_1_SQRT_2;
    let position = app
        .world()
        .get::<Grid>(root)
        .unwrap()
        .grid_position(entity.get::<CellCoord>().unwrap(), transform);
    assert!(
        position.distance(Vec3::new(2.0 * half, half, 0.0)) < 1e-3,
        "attached at {position}"
    );
}

#[test]
fn a_mirrored_sub_grid_does_not_mirror_or_turn_its_camera() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn(big_space::bundles::BigSpaceRootBundle::default())
        .id();
    let sub_grid = app
        .world_mut()
        .spawn((
            Grid::default(),
            CellCoord::default(),
            Transform::from_xyz(10.0, 0.0, 0.0).with_scale(Vec3::new(1.0, -1.0, 1.0)),
            ChildOf(root),
        ))
        .id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            CellCoord::default(),
            Transform::from_xyz(1.0, 0.0, 0.0),
            ChildOf(sub_grid),
        ))
        .id();
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(root));
    let transform = entity.get::<Transform>().unwrap();
    let position = app
        .world()
        .get::<Grid>(root)
        .unwrap()
        .grid_position(entity.get::<CellCoord>().unwrap(), transform);
    assert!(
        position.distance(Vec3::new(11.0, 0.0, 0.0)) < 1e-3,
        "attached at {position}"
    );
    assert!(
        transform.rotation.dot(Quat::IDENTITY).abs() > 1.0 - 1e-6,
        "rotation {:?}",
        transform.rotation
    );
}

#[test]
fn root_frame_pose_places_through_sub_grids_and_composes_gridless_parents() {
    let mut app = test_app();
    let (root, sub_grid, _) = camera_in_sub_grid(&mut app);
    let inside = app
        .world_mut()
        .spawn((
            CellCoord::new(1, 0, 0),
            Transform::from_xyz(5.0, 0.0, 0.0),
            ChildOf(sub_grid),
        ))
        .id();
    let (grid, position, rotation) = root_frame_pose(app.world_mut(), inside);
    assert_eq!(grid, Some(root));
    assert!(
        position.distance(IN_SUB_GRID.as_dvec3()) < 0.01,
        "at {position}"
    );
    assert_rotation_y_quarter(rotation.as_quat());

    let parent = app
        .world_mut()
        .spawn(
            Transform::from_xyz(100.0, 0.0, 0.0)
                .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
        )
        .id();
    let child = app
        .world_mut()
        .spawn((Transform::from_xyz(5.0, 0.0, 0.0), ChildOf(parent)))
        .id();
    let (grid, position, rotation) = root_frame_pose(app.world_mut(), child);
    assert_eq!(grid, None);
    assert!(
        position.distance(bevy::math::DVec3::new(100.0, 0.0, -5.0)) < 1e-3,
        "at {position}"
    );
    assert_rotation_y_quarter(rotation.as_quat());
}

#[test]
fn root_frame_pose_keeps_the_metre_of_a_cell_far_out() {
    let mut app = test_app();
    let root = app
        .world_mut()
        .spawn((big_space::bundles::BigSpaceRootBundle {
            grid: Grid::new(1024.0, 0.0),
            ..Default::default()
        },))
        .id();
    let far = app
        .world_mut()
        .spawn((
            CellCoord::new(16_384, 0, 0),
            Transform::from_xyz(1.0, 0.0, 0.0),
            ChildOf(root),
        ))
        .id();
    let (_, position, _) = root_frame_pose(app.world_mut(), far);
    assert_eq!(position.x, 16_384.0 * 1024.0 + 1.0);
}

#[test]
fn a_camera_far_out_keeps_its_metre_across_its_grid_being_replaced() {
    let mut app = test_app();
    let far_root = || big_space::bundles::BigSpaceRootBundle {
        grid: Grid::new(1024.0, 0.0),
        ..Default::default()
    };
    let root = app.world_mut().spawn(far_root()).id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            CellCoord::new(16_384, 0, 0),
            Transform::from_xyz(1.0, 0.0, 0.0),
            ChildOf(root),
        ))
        .id();
    app.update();
    app.update();

    // A hot reload: the grid goes, and an identical one comes back.
    app.world_mut().despawn(root);
    let root = app.world_mut().spawn(far_root()).id();
    app.update();
    app.update();

    let entity = app.world().entity(camera);
    assert_eq!(entity.get::<ChildOf>().map(ChildOf::parent), Some(root));
    let position = app.world().get::<Grid>(root).unwrap().grid_position_double(
        entity.get::<CellCoord>().unwrap(),
        entity.get::<Transform>().unwrap(),
    );
    assert!(
        (position.x - (16_384.0 * 1024.0 + 1.0)).abs() < 1e-3,
        "reattached at x = {}",
        position.x
    );
}

#[test]
fn a_rescued_camera_moved_away_and_back_reattaches_from_its_transform() {
    let mut app = test_app();
    let far_root = || big_space::bundles::BigSpaceRootBundle {
        grid: Grid::new(1024.0, 0.0),
        ..Default::default()
    };
    let root = app.world_mut().spawn(far_root()).id();
    let camera = app
        .world_mut()
        .spawn((
            GridFollowCamera,
            CellCoord::new(16_384, 0, 0),
            Transform::from_xyz(1.0, 0.0, 0.0),
            ChildOf(root),
        ))
        .id();
    app.update();
    app.update();
    app.world_mut().despawn(root);
    app.update();

    // Flown away and back to the same f32 value: the Transform is the truth now, not the
    // pose the rescue kept.
    let rescued = *app.world().get::<Transform>(camera).unwrap();
    app.world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .translation
        .x += 5000.0;
    app.update();
    *app.world_mut().get_mut::<Transform>(camera).unwrap() = rescued;
    app.update();

    let root = app.world_mut().spawn(far_root()).id();
    app.update();
    app.update();
    let entity = app.world().entity(camera);
    let position = app.world().get::<Grid>(root).unwrap().grid_position_double(
        entity.get::<CellCoord>().unwrap(),
        entity.get::<Transform>().unwrap(),
    );
    assert_eq!(position.x, f64::from(rescued.translation.x));
}
