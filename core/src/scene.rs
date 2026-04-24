use bevy::prelude::*;

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

    // Setup PerspectiveCamera for 3D rendering
    commands.spawn((
        Camera::default(),
        Camera3d::default(),
        Transform::from_translation(cam_pos).looking_at(target_pos, up),
        bevy::camera_controller::free_camera::FreeCamera::default(),
        bevy::light::AmbientLight {
            brightness: 10.,
            ..default()
        },
    ));

    // Add ambient light
    //commands.spawn((bevy::light::AmbientLight { ..default() },));

    //Add directional light
    commands.spawn((
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

    commands.spawn((
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

    commands.spawn((
        Mesh3d(cube_mesh),
        meshmat.clone(),
        Transform::from_translation(cube_pos),
    ));
}
