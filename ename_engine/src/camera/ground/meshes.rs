//! A ground probe over marked render meshes, for terrain that is drawn but has no collider.
//!
//! Always registered; asked in the `meshes` and `both` [`GroundGuardMode`](super::GroundGuardMode)s. It sees only meshes marked [`GroundSurface`],
//! on the mesh entity or any ancestor (a glTF scene puts its meshes on children of the scene
//! root), so sky domes, water, foliage, gizmos and characters are ignored. When the colliders
//! probe is asked too (`both`), a marked mesh with a `Collider` on itself or an ancestor is
//! skipped: the colliders probe answers for it, and sees the unmarked colliders (a tunnel's
//! ceiling) this probe cannot. With this probe alone (`meshes`) it counts. It mirrors the physics probe's rules
//! for marked terrain; a mesh has no inside:
//!
//! - ground: the first marked surface straight down, whichever way the face points;
//! - under the terrain: when a marked surface is straight up, the ground is the top of that mesh
//!   on the vertical (a ray down from above its own bounds, against it alone);
//! - rays (the ceiling, `center_hit`): against marked meshes only.
//!
//! Meshes are cast against with their `GlobalTransform`, in render space, converted with the root
//! grid's current floating origin. Both come from the same transform propagation, so they agree
//! even while the world is paused. The mesh must keep its data in the main world
//! (`RenderAssetUsages::MAIN_WORLD`); a GPU-displaced surface is not seen.
//!
//! Cost per call: the target set is found once a frame and cached; a mesh's computed bounds are
//! cached per asset until it changes; a ray tests meshes in the order it enters their bounds and
//! stops once the next bounds start beyond the nearest hit.
//!
//! Uses bevy_picking's mesh intersection functions rather than `MeshRayCast`, which skips back
//! faces unless every entity carries `RayCastBackfaces`.

use avian3d::prelude::Collider;
use bevy::{
    asset::{LoadState, RenderAssetUsages},
    camera::primitives::{Aabb, MeshAabb},
    diagnostic::FrameCount,
    ecs::system::SystemParam,
    math::{Affine3A, DVec3, Ray3d, bounding::Aabb3d},
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    picking::mesh_picking::ray_cast::{Backfaces, ray_aabb_intersection_3d, ray_mesh_intersection},
    platform::collections::{HashMap, HashSet},
    prelude::*,
};

use super::{
    AskedMode, GroundAnswer, GroundRay, GroundSurface, MAX_RAY_DISTANCE as RANGE, ProbeKind,
    RayAnswer,
};
use crate::bigspace::render_frame::{RenderFrame, RootGrids};

/// What the probe keeps between calls.
#[derive(Resource, Default)]
pub(super) struct MeshGroundCache {
    /// The frame `targets` was found in.
    frame: Option<u32>,
    /// Marked mesh entities, and whether a collider is in their hierarchy.
    targets: Vec<(Entity, bool)>,
    /// Bounds computed from a mesh asset, for entities with no `Aabb`.
    bounds: HashMap<AssetId<Mesh>, Option<Aabb>>,
}

pub(super) fn forget_changed_bounds(
    mut events: MessageReader<AssetEvent<Mesh>>,
    mut cache: ResMut<MeshGroundCache>,
) {
    for event in events.read() {
        match event {
            AssetEvent::Modified { id }
            | AssetEvent::Removed { id }
            | AssetEvent::Unused { id } => {
                cache.bounds.remove(id);
            }
            _ => {}
        }
    }
}

#[derive(SystemParam)]
pub(super) struct MarkedMeshes<'w, 's> {
    roots: RootGrids<'w, 's>,
    frame_count: Option<Res<'w, FrameCount>>,
    asked: Option<Res<'w, AskedMode>>,
    cache: ResMut<'w, MeshGroundCache>,
    meshes: Query<
        'w,
        's,
        (
            Entity,
            &'static Mesh3d,
            &'static GlobalTransform,
            Option<&'static Aabb>,
        ),
    >,
    parents: Query<'w, 's, &'static ChildOf>,
    marked: Query<'w, 's, (), With<GroundSurface>>,
    colliders: Query<'w, 's, (), With<Collider>>,
    assets: Res<'w, Assets<Mesh>>,
    server: Option<Res<'w, AssetServer>>,
    /// Marked meshes already reported as having no main-world data.
    warned: Local<'s, HashSet<Entity>>,
}

/// A marked mesh ready to be cast against.
struct Target {
    mesh: AssetId<Mesh>,
    transform: Affine3A,
    aabb: Aabb,
}

impl Target {
    /// The highest point of its bounds, in render-space Y.
    fn top(&self) -> f32 {
        let extent = self.transform.matrix3.abs() * self.aabb.half_extents;
        self.transform.transform_point3a(self.aabb.center).y + extent.y
    }

    /// Where the ray enters its bounds, if it does within `max`.
    fn entry(&self, ray: Ray3d, max: f32) -> Option<f32> {
        let aabb = Aabb3d::new(self.aabb.center, self.aabb.half_extents);
        ray_aabb_intersection_3d(ray, &aabb, &self.transform).filter(|near| *near <= max)
    }
}

impl MarkedMeshes<'_, '_> {
    /// Marked, and no collider on the way up: the physics probe answers for a mesh with one.
    /// `None` for an unmarked mesh; for a marked one, whether a collider is on it or an
    /// ancestor.
    fn marking(&self, mut entity: Entity) -> Option<bool> {
        let (mut marked, mut collider) = (false, false);
        loop {
            collider |= self.colliders.contains(entity);
            marked |= self.marked.contains(entity);
            match self.parents.get(entity) {
                Ok(child_of) => entity = child_of.parent(),
                Err(_) => return marked.then_some(collider),
            }
        }
    }

    /// The marked mesh entities, found once a frame.
    fn target_entities(&mut self) -> Vec<Entity> {
        let frame = self.frame_count.as_ref().map(|count| count.0);
        if frame.is_none() || self.cache.frame != frame {
            let targets = self
                .meshes
                .iter()
                .filter_map(|(entity, ..)| self.marking(entity).map(|collider| (entity, collider)))
                .collect();
            self.cache.targets = targets;
            self.cache.frame = frame;
        }
        // A collider-backed mesh is the colliders probe's when that is asked too: it answers for
        // it, and sees the unmarked colliders around it. With this probe alone it counts.
        let colliders_asked = self
            .asked
            .as_ref()
            .is_some_and(|asked| asked.0.asks(ProbeKind::Colliders));
        self.cache
            .targets
            .iter()
            .filter(|(_, collider)| !(colliders_asked && *collider))
            .map(|(entity, _)| *entity)
            .collect()
    }

    /// The marked meshes, or `None` while the asset of one of them is still loading.
    fn targets(&mut self) -> Option<Vec<Target>> {
        let mut targets = Vec::new();
        for entity in self.target_entities() {
            let Ok((_, mesh, transform, aabb)) = self.meshes.get(entity) else {
                continue;
            };
            let id = mesh.id();
            let Some(asset) = self.assets.get(id) else {
                let state = self.server.as_ref().and_then(|s| s.get_load_state(id));
                if matches!(state, Some(LoadState::NotLoaded | LoadState::Loading)) {
                    return None;
                }
                // Loaded but gone from the main world, or never there.
                warn_once(&mut self.warned, entity);
                continue;
            };
            if !asset.asset_usage.contains(RenderAssetUsages::MAIN_WORLD) {
                warn_once(&mut self.warned, entity);
                continue;
            }
            // No `Aabb` yet (bounds come a frame after the asset), or never (`NoFrustumCulling`):
            // the asset's own bounds, computed once per asset.
            let aabb = match aabb {
                Some(aabb) => Some(*aabb),
                None => *self
                    .cache
                    .bounds
                    .entry(id)
                    .or_insert_with(|| asset.compute_aabb()),
            };
            let Some(aabb) = aabb else {
                continue;
            };
            targets.push(Target {
                mesh: id,
                transform: transform.affine(),
                aabb,
            });
        }
        Some(targets)
    }

    /// The nearest marked surface along the render-space ray within `max`, and which target it
    /// is on. Targets are tested in the order the ray enters their bounds, and the search stops
    /// once the next bounds start beyond the nearest hit.
    fn cast(&self, targets: &[Target], ray: Ray3d, max: f32) -> Option<(f32, usize)> {
        let mut entries: Vec<(f32, usize)> = targets
            .iter()
            .enumerate()
            .filter_map(|(index, target)| target.entry(ray, max).map(|near| (near, index)))
            .collect();
        entries.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut best: Option<(f32, usize)> = None;
        for (near, index) in entries {
            if best.is_some_and(|(distance, _)| near > distance) {
                break;
            }
            let target = &targets[index];
            let Some(mesh) = self.assets.get(target.mesh) else {
                continue;
            };
            if let Some(distance) = intersect(mesh, &target.transform, ray)
                && distance <= max
                && best.is_none_or(|(nearest, _)| distance < nearest)
            {
                best = Some((distance, index));
            }
        }
        best
    }

    /// The nearest marked surface along an absolute ray, from `origin`, and its target; for a
    /// vertical ray the start moves [`RenderFrame::y_step`] up first, as the physics probe's does.
    fn cast_from(
        &self,
        frame: &RenderFrame,
        targets: &[Target],
        origin: DVec3,
        direction: DVec3,
        max: f64,
    ) -> Option<(f64, usize)> {
        let vertical = direction.x == 0.0 && direction.z == 0.0;
        let lift = if vertical { frame.y_step(origin) } else { 0.0 };
        let start = origin + DVec3::Y * lift;
        let offset = lift * direction.y;
        let ray = Ray3d::new(
            frame.render_point(start),
            frame.direction_to_render(direction)?,
        );
        self.cast(targets, ray, (max - offset).max(0.0) as f32)
            .map(|(distance, index)| (f64::from(distance) + offset, index))
    }
}

fn warn_once(warned: &mut HashSet<Entity>, entity: Entity) {
    if warned.insert(entity) {
        warn!(
            "mesh ground probe: {entity} is marked GroundSurface, but its mesh has no main-world \
             data, so it cannot be ray cast and is ignored. Load it with RenderAssetUsages \
             MAIN_WORLD | RENDER_WORLD, or mark a collider instead"
        );
    }
}

/// The nearest intersection of a render-space ray with `mesh`, both faces counted.
fn intersect(mesh: &Mesh, transform: &Affine3A, ray: Ray3d) -> Option<f32> {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return None;
    }
    let positions = mesh
        .try_attribute(Mesh::ATTRIBUTE_POSITION)
        .ok()?
        .as_float3()?;
    let normals = mesh
        .try_attribute(Mesh::ATTRIBUTE_NORMAL)
        .ok()
        .and_then(VertexAttributeValues::as_float3);
    fn cast<I: TryInto<usize> + Copy>(
        ray: Ray3d,
        transform: &Affine3A,
        positions: &[[f32; 3]],
        normals: Option<&[[f32; 3]]>,
        indices: Option<&[I]>,
    ) -> Option<f32> {
        ray_mesh_intersection(
            ray,
            transform,
            positions,
            normals,
            indices,
            None,
            Backfaces::Include,
        )
        .map(|hit| hit.distance)
    }
    match mesh.try_indices().ok() {
        Some(Indices::U16(indices)) => cast(ray, transform, positions, normals, Some(indices)),
        Some(Indices::U32(indices)) => cast(ray, transform, positions, normals, Some(indices)),
        None => cast::<u32>(ray, transform, positions, normals, None),
    }
}

pub(super) fn mesh_ground(In(point): In<DVec3>, mut meshes: MarkedMeshes) -> GroundAnswer {
    let Some(frame) = RenderFrame::of_single_root(meshes.roots.iter()) else {
        return GroundAnswer::NoGround;
    };
    let Some(targets) = meshes.targets() else {
        return GroundAnswer::NotLoaded;
    };
    if targets.is_empty() {
        return GroundAnswer::NoGround;
    }
    // Under marked terrain: onto the top of the mesh straight up, from above its own bounds.
    if let Some((_, index)) = meshes.cast_from(&frame, &targets, point, DVec3::Y, RANGE) {
        let target = &targets[index..=index];
        let above = f64::from(target[0].top() - frame.render_point(point).y) + 1.0;
        let start = point + DVec3::Y * above;
        if let Some((distance, _)) = meshes.cast_from(&frame, target, start, DVec3::NEG_Y, RANGE) {
            return GroundAnswer::Surface(start.y - distance);
        }
    }
    match meshes.cast_from(&frame, &targets, point, DVec3::NEG_Y, RANGE) {
        Some((distance, _)) => GroundAnswer::Surface(point.y - distance),
        None => GroundAnswer::NoGround,
    }
}

pub(super) fn mesh_ray(In(query): In<GroundRay>, mut meshes: MarkedMeshes) -> RayAnswer {
    let Some(frame) = RenderFrame::of_single_root(meshes.roots.iter()) else {
        return RayAnswer::Miss;
    };
    let Some(targets) = meshes.targets() else {
        return RayAnswer::NotLoaded;
    };
    match meshes.cast_from(
        &frame,
        &targets,
        query.origin,
        query.direction,
        query.max_distance,
    ) {
        Some((distance, _)) => RayAnswer::Hit(distance),
        None => RayAnswer::Miss,
    }
}

#[cfg(test)]
mod tests;
