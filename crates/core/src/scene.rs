use bevy::prelude::*;
use big_space::commands::*;

/// Sets up a basic game world with a 3D scene containing a cube, plane, and lighting
pub fn new_simple_scene(
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
        let (grid_cell, cell_offset) = root_grid
            .grid()
            .translation_to_grid(bevy::math::DVec3::from(cam_pos));

        root_grid.spawn_spatial((
            Camera3d::default(),
            Transform::from_translation(cell_offset).looking_at(target_pos, up),
            grid_cell,
            big_space::floating_origins::FloatingOrigin,
            big_space::camera::BigSpaceCameraController::default().with_speed_bounds([1e1, 1e30]),
        ));

        //Add directional light
        root_grid.spawn_spatial((
            bevy::light::DirectionalLight {
                illuminance: 1000.,
                shadows_enabled: true,
                ..default()
            },
            Transform::from_translation(light_pos).looking_at(target_pos, up),
        ));

        let mat = materials.add(Color::WHITE);
        let meshmat = MeshMaterial3d(mat);

        // Create a plane entity
        let plane = Plane3d::new(Vec3::new(0.0, 1.0, 0.0), Vec2::new(10.0, 10.0));
        let plane_mesh = meshes.add(Mesh::from(plane).with_computed_normals());

        root_grid.spawn_spatial((
            Mesh3d(plane_mesh),
            meshmat.clone(),
            Transform::from_translation(plane_pos),
        ));

        // Create a cube entity
        let cube = Cuboid::new(1.0, 1.0, 1.0);
        let cube_mesh = meshes.add(
            Mesh::from(cube)
                .with_duplicated_vertices()
                .with_computed_flat_normals(),
        );

        root_grid.spawn_spatial((
            Mesh3d(cube_mesh),
            meshmat.clone(),
            Transform::from_translation(cube_pos),
        ));
    });
}

pub fn load_model(
    commands: Commands,
    asset_server: ResMut<AssetServer>,
    grid_query: Query<crate::grid::GridQuery>,
) {
    let grid_entity = grid_query
        .single()
        .expect("Failed to spawn entity on grid. Grid not present!");

    crate::grid::spawn_and_position_entity_on_grid(
        commands,
        grid_entity.entity,
        grid_entity.grid.clone(),
        bevy::math::DVec3::ZERO,
        |new_entity| {
            let path = "map.glb#Scene0";
            let scene = SceneRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(path)));
            new_entity.insert(Name::new(path)).insert(scene);
        },
    );
}
