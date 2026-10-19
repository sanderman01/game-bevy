//! Low-detail colliders authored in glTF. A node named `COL_<what>` whose extras hold
//! `{"collider": "<shape>"}` becomes an avian `Collider` on that node, and its meshes are despawned
//! so it never renders. Primitive shapes are fitted to the node's mesh bounds in its local frame;
//! a capsule's and a cylinder's axis is local Y. See `docs/conventions.md`.

use avian3d::prelude::{Collider, Position, Rotation};
use bevy::{
    camera::primitives::Aabb, gltf::GltfExtras, prelude::*, world_serialization::WorldInstanceReady,
};
use serde::Deserialize;

/// Name prefix that marks a glTF node as a collider.
pub const COLLIDER_NODE_PREFIX: &str = "COL_";

/// On a node that a glTF `COL_` node became: which shape its extras asked for.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[reflect(Component, Debug)]
#[serde(rename_all = "snake_case")]
pub enum GltfColliderShape {
    Box,
    Sphere,
    Capsule,
    Cylinder,
    ConvexHull,
    /// For static structures only: avian can't compute a trimesh's mass properties well.
    Trimesh,
}

#[derive(Deserialize)]
struct ColliderExtras {
    collider: GltfColliderShape,
}

/// Turns every `COL_` node in a just-spawned world instance into a collider.
pub(super) fn build_gltf_colliders(
    ready: On<WorldInstanceReady>,
    children: Query<&Children>,
    nodes: Query<(&Name, &GltfExtras)>,
    primitives: Query<(&Mesh3d, &Aabb, &Transform)>,
    meshes: Res<Assets<Mesh>>,
    mut commands: Commands,
) {
    for node in children.iter_descendants(ready.entity) {
        let Ok((name, extras)) = nodes.get(node) else {
            continue;
        };
        if !name.as_str().starts_with(COLLIDER_NODE_PREFIX) {
            continue;
        }
        let shape = match serde_json::from_str::<ColliderExtras>(&extras.value) {
            Ok(parsed) => parsed.collider,
            Err(error) => {
                warn!("glTF collider node {name} has no valid `collider` extra: {error}");
                continue;
            }
        };
        let node_primitives: Vec<Entity> = children
            .get(node)
            .map(|c| c.iter().filter(|&e| primitives.contains(e)).collect())
            .unwrap_or_default();
        let mut parts = Vec::with_capacity(node_primitives.len());
        for &primitive in &node_primitives {
            let Ok((mesh, aabb, transform)) = primitives.get(primitive) else {
                continue;
            };
            match fit(shape, aabb, meshes.get(&mesh.0)) {
                Some((offset, collider)) => parts.push((
                    transform.transform_point(offset),
                    transform.rotation,
                    collider,
                )),
                None => warn!("glTF collider node {name}: could not build a {shape:?}"),
            }
        }
        let collider = match parts.len() {
            0 => {
                warn!("glTF collider node {name} has no mesh to fit a collider to");
                continue;
            }
            1 if parts[0].0.length_squared() < 1e-12 && parts[0].1 == Quat::IDENTITY => {
                parts.pop().map(|(_, _, collider)| collider).unwrap()
            }
            _ => Collider::compound(
                parts
                    .into_iter()
                    .map(|(at, rotation, collider)| {
                        (Position(at), Rotation::from(rotation), collider)
                    })
                    .collect(),
            ),
        };
        for primitive in node_primitives {
            commands.entity(primitive).despawn();
        }
        commands.entity(node).insert((collider, shape));
    }
}

/// The collider for one mesh, and where its centre sits in the mesh's frame.
fn fit(shape: GltfColliderShape, aabb: &Aabb, mesh: Option<&Mesh>) -> Option<(Vec3, Collider)> {
    let centre = Vec3::from(aabb.center);
    let half = Vec3::from(aabb.half_extents);
    let radial = half.x.max(half.z);
    match shape {
        GltfColliderShape::Box => Some((
            centre,
            Collider::cuboid(half.x * 2.0, half.y * 2.0, half.z * 2.0),
        )),
        GltfColliderShape::Sphere => Some((centre, Collider::sphere(half.max_element()))),
        GltfColliderShape::Capsule => Some((
            centre,
            Collider::capsule(radial, (half.y - radial).max(0.0) * 2.0),
        )),
        GltfColliderShape::Cylinder => Some((centre, Collider::cylinder(radial, half.y * 2.0))),
        GltfColliderShape::ConvexHull => {
            Collider::convex_hull_from_mesh(mesh?).map(|collider| (Vec3::ZERO, collider))
        }
        GltfColliderShape::Trimesh => {
            Collider::trimesh_from_mesh(mesh?).map(|collider| (Vec3::ZERO, collider))
        }
    }
}
