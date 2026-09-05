//! The starting scene. Temporary: a Rust function that spawns hardcoded entities, standing in
//! for scene data the editor cannot yet produce. See the open question in `docs/design.md`.

use avian3d::{
    collision::collider::{Collider, ColliderConstructorHierarchy},
    dynamics::rigid_body::RigidBody,
};
use bevy::{math::DVec3, prelude::*};
use big_space::commands::*;
use editor::editor::EditorCamera;
use engine::{
    bigspace::grid::{GridQuery, on_grid, on_grid_looking_at},
    camera::{CameraDriver, MainCamera, VirtualCamera},
};
use modloader::AssetRegistry;

use crate::GameState;

/// Ordering within scene setup.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
enum SceneSetup {
    /// Spawns the big space and everything parented to it.
    World,
    /// Spawns package assets onto the grid the previous set created.
    Models,
}

/// Spawns the starting scene on entry to [`GameState::Scene`].
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            OnEnter(GameState::Scene),
            (SceneSetup::World, SceneSetup::Models).chain(),
        )
        .add_systems(
            OnEnter(GameState::Scene),
            (
                spawn_scene.in_set(SceneSetup::World),
                load_models.in_set(SceneSetup::Models),
            ),
        );
    }
}

/// Sets up a basic game world with a 3D scene containing a cube, plane, and lighting
fn spawn_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let cube_pos = Vec3::new(0., 1.0, 0.);
    let plane_pos = Vec3::ZERO;
    let light_pos = 5. * Vec3::ONE;
    let cam_pos = Vec3::new(5., 2., 10.);
    let target_pos = Vec3::ZERO;
    let up = Vec3::Y;

    // Setup big space
    let grid = big_space::grid::Grid::new(2_000f32, 100f32);
    commands.spawn_big_space(grid, |root_grid| {
        root_grid.insert(Name::new("Grid"));
        // Setup PerspectiveCamera for 3D rendering
        let grid = root_grid.grid().clone();

        let (grid_cell, cell_offset) = grid.translation_to_grid(bevy::math::DVec3::from(cam_pos));

        //Add directional light
        root_grid.spawn_spatial((
            bevy::light::DirectionalLight {
                illuminance: 1000.,
                shadow_maps_enabled: true,
                ..default()
            },
            Transform::from_translation(light_pos).looking_at(target_pos, up),
        ));

        // Add camera
        root_grid.spawn_spatial((
            Name::new("Main Camera"),
            Camera3d::default(),
            CameraDriver::default(),
            MainCamera,
            EditorCamera,
            on_grid_looking_at(&grid, DVec3::ZERO, DVec3::ZERO, up),
        ));

        root_grid.spawn_spatial((
            Name::new("BigSpaceCameraController"),
            VirtualCamera {
                priority: 1,
                ..default()
            },
            Transform::from_translation(cell_offset).looking_at(target_pos, up),
            grid_cell,
            big_space::floating_origins::FloatingOrigin,
            big_space::camera::BigSpaceCameraController::default().with_speed_bounds([1e1, 1e30]),
        ));

        // Add virtual cameras
        root_grid.spawn_spatial((
            Name::new("VirtualCamera"),
            VirtualCamera::default(),
            on_grid_looking_at(&grid, DVec3::new(4000., 2000., 4000.), DVec3::ZERO, up),
        ));
        root_grid.spawn_spatial((
            Name::new("VirtualCamera"),
            VirtualCamera::default(),
            on_grid_looking_at(&grid, DVec3::new(-4000., 2000., 4000.), DVec3::ZERO, up),
        ));
        root_grid.spawn_spatial((
            Name::new("VirtualCamera"),
            VirtualCamera::default(),
            on_grid_looking_at(&grid, DVec3::new(-4000., 2000., -4000.), DVec3::ZERO, up),
        ));

        let mat = materials.add(Color::WHITE);
        let meshmat = MeshMaterial3d(mat);

        // Create a plane entity
        let plane = Plane3d::new(Vec3::new(0.0, 1.0, 0.0), Vec2::new(10.0, 10.0));
        let plane_mesh = meshes.add(Mesh::from(plane).with_computed_normals());
        root_grid.spawn_spatial((
            Name::new("Plane"),
            Mesh3d(plane_mesh),
            meshmat.clone(),
            Transform::from_translation(plane_pos),
            Collider::cuboid(20.0, 1.0, 20.0),
            RigidBody::Static,
        ));

        // Create a cube entity
        let cube = Cuboid::new(1.0, 1.0, 1.0);
        let cube_mesh = meshes.add(
            Mesh::from(cube)
                .with_duplicated_vertices()
                .with_computed_flat_normals(),
        );

        root_grid.spawn_spatial((
            Name::new("Cube"),
            Mesh3d(cube_mesh),
            meshmat.clone(),
            Transform::from_translation(cube_pos),
        ));
    });
}

fn load_models(
    mut commands: Commands,
    asset_server: ResMut<AssetServer>,
    asset_registry: ResMut<AssetRegistry>,
    grid_query: Query<GridQuery>,
) {
    info!("{}", "Loading models");
    let grid_entity = grid_query
        .single()
        .expect("Failed to spawn entity on grid. Grid not present!");

    let alias = "core::map";
    let Some(path) = asset_registry.get_path(alias) else {
        return;
    };
    info!("{}", &path);
    let label = GltfAssetLabel::Scene(0).from_asset(path + "#Scene0");
    commands.spawn((
        Name::new(alias),
        WorldAssetRoot(asset_server.load(label)),
        ChildOf(grid_entity.entity),
        on_grid(grid_entity.grid, DVec3::ZERO),
    ));

    let alias = "core::airship";
    let Some(path) = asset_registry.get_path(alias) else {
        return;
    };
    info!("{}", &path);
    let label = GltfAssetLabel::Scene(0).from_asset(path + "#Scene0");
    commands.spawn((
        Name::new(alias),
        WorldAssetRoot(asset_server.load(label)),
        ChildOf(grid_entity.entity),
        on_grid(grid_entity.grid, DVec3::new(0.0, 5.0, 0.0)),
        ColliderConstructorHierarchy {
            default_constructor: Some(
                avian3d::collision::collider::ColliderConstructor::ConvexHullFromMesh,
            ),
            ..default()
        },
        //Collider::capsule(0.5, 2.0),
        RigidBody::Dynamic,
    ));
}
