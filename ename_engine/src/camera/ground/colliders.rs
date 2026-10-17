//! A ground probe over avian's static colliders, for games whose ground is physics geometry.
//!
//! avian positions are render-space (`transform_to_position` reads `GlobalTransform`), and
//! big_space puts the render origin at the floating origin's cell. Physics positions and the
//! collider tree only update in the fixed and physics schedules, which do not run while the world
//! is paused, so after a paused cross-cell jump they are still in the old render frame. So the
//! frame is recorded when physics syncs, and the probe converts with that one.
//!
//! Only static geometry counts: a collider with no body, or one whose body is
//! `RigidBody::Static`. Dynamic and kinematic bodies, their child colliders, and sensors (trigger
//! volumes) are skipped.
//!
//! # What it answers
//!
//! It answers only what one ray each way can say reliably:
//!
//! - **Ground**: the first static surface a ray straight down from the point hits, whichever way
//!   the face points.
//! - **Ceiling** (the ray answer, used by the guard straight up): the first static surface a ray
//!   hits.
//! - **Inside a solid**: only for shapes whose inside a point query answers reliably: convex
//!   shapes (cuboid, sphere, capsule, cylinder, cone, convex hull, half-space) and the convex
//!   parts of a compound. A point inside or on one leaves it by a hollow ray straight up through
//!   that part alone, again from there while it is inside another (flush stacks), up to 8 times.
//!   Where it comes out is its ground.
//! - The vertical rays share one start, a few f32 steps of the render-space Y above the point, so
//!   a surface the point lies on (rounded either way) is ground going down and never ceiling going
//!   up, and nothing in between is missed by both.
//! - Whichever way the ground was found (leaving a convex solid, the top of marked terrain, the
//!   first surface below), the camera then rises through every convex solid resting flush on it,
//!   at most 8 times.
//! - **Under the terrain**: opt-in. A game marks its terrain colliders with [`GroundSurface`].
//!   When the first surface straight up from the point is a marked collider, the point is under
//!   the terrain, and the ground is that collider's top along the vertical.
//!
//! Limits: a camera inside a closed triangle mesh or a non-convex solid is not seen as inside,
//! and unmarked terrain is a plain surface, so a camera under it stays under it.

use avian3d::prelude::{
    Collider, ColliderOf, Position, RigidBody, Rotation, Sensor, SpatialQuery, SpatialQueryFilter,
};
use bevy::{math::DVec3, prelude::*};

use super::{GroundAnswer, GroundRay, GroundSurface, MAX_RAY_DISTANCE as RANGE, RayAnswer};
use crate::bigspace::render_frame::{RenderFrame, RootGrids};

/// At most this many stacked solids are left one after another.
const MAX_EXITS: usize = 8;

/// The render frame physics last synced against: the root grid, its floating origin's cell and
/// the transform from that cell to render space. `None` until physics has synced once; `Some(None)`
/// with zero or several root grids, which this probe does not handle.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PhysicsFrame(Option<Option<RenderFrame>>);

/// Runs in avian's `PhysicsSystems::Last`.
pub(super) fn record_physics_frame(roots: RootGrids, mut current: ResMut<PhysicsFrame>) {
    current.set_if_neq(PhysicsFrame(Some(RenderFrame::of_single_root(
        roots.iter(),
    ))));
}

#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct StaticGeometry<'w, 's> {
    spatial: SpatialQuery<'w, 's>,
    frame: Res<'w, PhysicsFrame>,
    owners: Query<'w, 's, &'static ColliderOf>,
    bodies: Query<'w, 's, &'static RigidBody>,
    sensors: Query<'w, 's, (), With<Sensor>>,
    marked: Query<'w, 's, (), With<GroundSurface>>,
    colliders: Query<'w, 's, (&'static Position, &'static Rotation, &'static Collider)>,
}

/// A static collider a ray hit.
struct Hit {
    collider: Entity,
    distance: f64,
}

impl StaticGeometry<'_, '_> {
    /// Static geometry only: no dynamic or kinematic body, and no sensor.
    fn is_static_solid(&self, collider: Entity) -> bool {
        if self.sensors.contains(collider) {
            return false;
        }
        match self.owners.get(collider) {
            Ok(owner) => self
                .bodies
                .get(owner.body)
                .map_or(true, RigidBody::is_static),
            Err(_) => true,
        }
    }

    fn is_terrain(&self, collider: Entity) -> bool {
        self.marked.contains(collider)
            || self
                .owners
                .get(collider)
                .is_ok_and(|owner| self.marked.contains(owner.body))
    }

    /// The first static surface along the ray from `origin` itself, `None` on a miss.
    fn cast(&self, frame: &RenderFrame, origin: DVec3, direction: DVec3, max: f64) -> Option<Hit> {
        let dir = frame.direction_to_render(direction)?;
        self.spatial
            .cast_ray_predicate(
                frame.render_point(origin),
                dir,
                max as f32,
                false,
                &SpatialQueryFilter::default(),
                &|collider| self.is_static_solid(collider),
            )
            .map(|hit| Hit {
                collider: hit.entity,
                distance: f64::from(hit.distance),
            })
    }

    /// As [`Self::cast`], but starting [`y_step`] along an upward ray, so the surface a point
    /// lies on is not hit at distance 0. The distance is still from `origin`.
    fn cast_up(&self, frame: &RenderFrame, origin: DVec3, max: f64) -> Option<Hit> {
        let step = frame.y_step(origin);
        self.cast(frame, origin + DVec3::Y * step, DVec3::Y, max - step)
            .map(|hit| Hit {
                collider: hit.collider,
                distance: hit.distance + step,
            })
    }

    /// How far up from `point` the convex static parts containing it end, the furthest over all
    /// of them; `None` when no convex part contains it. Convex shapes, and the convex parts of a
    /// compound, are the ones whose inside a point query answers reliably. A hollow ray from
    /// inside a part leaves through it, so the distance is that part's own exit, whatever the
    /// rest of a compound or the shape's bounds are.
    fn exit_up(&self, frame: &RenderFrame, point: DVec3) -> Option<f64> {
        use avian3d::parry::{math::Pose3, query::Ray, shape::Shape};
        let render = frame.render_point(point);
        let up = frame.direction_to_render(DVec3::Y)?;
        let ray = Ray::new(render, up.as_vec3());
        let exits = |collider: Entity| -> Option<f64> {
            let (position, rotation, shape) = self.colliders.get(collider).ok()?;
            let pose = Pose3::from_parts(position.0, rotation.0);
            let shape = shape.shape_scaled();
            let parts: Vec<(Pose3, &dyn Shape)> = match shape.as_compound() {
                Some(compound) => compound
                    .shapes()
                    .iter()
                    .map(|(part_pose, part)| (pose * *part_pose, part.as_ref()))
                    .collect(),
                None => vec![(pose, shape.as_ref())],
            };
            parts
                .into_iter()
                .filter(|(pose, part)| part.is_convex() && part.contains_point(pose, render))
                .filter_map(|(pose, part)| part.cast_ray(&pose, &ray, RANGE as f32, false))
                .map(f64::from)
                .max_by(f64::total_cmp)
        };
        self.spatial
            .point_intersections(render, &SpatialQueryFilter::default())
            .into_iter()
            .filter(|&collider| self.is_static_solid(collider))
            .filter_map(exits)
            .max_by(f64::total_cmp)
    }

    /// From a candidate ground at `top` on the vertical through `point`, leaves every convex
    /// solid resting flush on it, and on the next, at most [`MAX_EXITS`] times. Every way of
    /// finding a ground goes through this, so a solid stacked on a floor, on a solid or on marked
    /// terrain is never the camera's ceiling.
    fn rise_through_solids(&self, frame: &RenderFrame, point: DVec3, mut top: f64) -> f64 {
        for _ in 0..MAX_EXITS {
            let above = DVec3::new(point.x, top, point.z);
            let probe = above + DVec3::Y * frame.y_step(above);
            let Some(exit) = self.exit_up(frame, probe) else {
                break;
            };
            top = probe.y + exit;
        }
        top
    }

    /// The first static surface straight down, from the same start as [`Self::cast_up`], so the
    /// two rays share their start and no surface between the point and that start is missed by
    /// both. Returns its height.
    fn surface_below(&self, frame: &RenderFrame, point: DVec3) -> Option<f64> {
        let start = point + DVec3::Y * frame.y_step(point);
        self.cast(frame, start, DVec3::NEG_Y, RANGE)
            .map(|hit| start.y - hit.distance)
    }

    /// The top of the marked collider `hit` along the vertical through `point`: its last surface
    /// going up, followed against that collider alone.
    fn top_of_terrain(&self, frame: &RenderFrame, point: DVec3, hit: &Hit) -> f64 {
        let mut top = point.y + hit.distance;
        let Ok((position, rotation, shape)) = self.colliders.get(hit.collider) else {
            return top;
        };
        let Some(up) = frame.direction_to_render(DVec3::Y) else {
            return top;
        };
        for _ in 0..MAX_EXITS {
            let from = DVec3::new(point.x, top, point.z);
            let step = frame.y_step(from);
            let start = frame.render_point(from + DVec3::Y * step);
            let Some((distance, _)) = shape.cast_ray(
                *position,
                *rotation,
                start,
                up.as_vec3(),
                RANGE as f32,
                false,
            ) else {
                break;
            };
            top += step + f64::from(distance);
        }
        top
    }
}

pub(super) fn physics_ground(In(point): In<DVec3>, geometry: StaticGeometry) -> GroundAnswer {
    let Some(frame) = geometry.frame.0 else {
        return GroundAnswer::NotLoaded;
    };
    let Some(frame) = frame else {
        return GroundAnswer::NoGround;
    };
    // A candidate ground: out of the convex solid the point is in (or exactly on), else the top
    // of marked terrain the point is under, else the first surface below. Then up through any
    // convex solids resting on it.
    let candidate = if let Some(exit) = geometry.exit_up(&frame, point) {
        Some(point.y + exit)
    } else if let Some(hit) = geometry.cast_up(&frame, point, RANGE)
        && geometry.is_terrain(hit.collider)
    {
        Some(geometry.top_of_terrain(&frame, point, &hit))
    } else {
        geometry.surface_below(&frame, point)
    };
    match candidate {
        Some(top) => GroundAnswer::Surface(geometry.rise_through_solids(&frame, point, top)),
        None => GroundAnswer::NoGround,
    }
}

/// The first static surface along the ray: the ceiling straight up, and `center_hit`. A ray going
/// up starts [`y_step`] above its origin, so the ground it is cast from is not its ceiling.
pub(super) fn physics_ray(In(query): In<GroundRay>, geometry: StaticGeometry) -> RayAnswer {
    let Some(frame) = geometry.frame.0 else {
        return RayAnswer::NotLoaded;
    };
    let Some(frame) = frame else {
        return RayAnswer::Miss;
    };
    let hit = if query.direction.y > 0.0 && query.direction.x == 0.0 && query.direction.z == 0.0 {
        geometry.cast_up(&frame, query.origin, query.max_distance)
    } else {
        geometry.cast(&frame, query.origin, query.direction, query.max_distance)
    };
    match hit {
        Some(hit) => RayAnswer::Hit(hit.distance),
        None => RayAnswer::Miss,
    }
}

#[cfg(test)]
mod tests;
