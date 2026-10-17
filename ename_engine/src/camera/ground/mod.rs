//! Ground probes: where the surface under a point is, so a camera placement can keep clear of it
//! and a pose can report what its view centre looks at.
//!
//! [`CameraGroundPlugin`] registers two built-in probes: one over static colliders (`colliders`)
//! and one over render meshes marked [`GroundSurface`] (`meshes`). A game can add its own, for
//! streamed terrain say, with
//! [`GroundProbeAppExt`]. Which probes a query asks is its [`GroundGuardMode`]: the built-in
//! colliders probe, the built-in mesh probe, both, or none. Game probes are asked in every mode
//! but `none`. [`CameraGroundGuard`] is a game's default mode and clearance; it defaults to
//! `both` and 2 m. In a game without colliders or marked meshes the probes answer "no ground",
//! which is not waited for, so the guard does nothing there.
//!
//! A probe answers for a 3D point, so a room with a floor and a ceiling, a multi-storey building
//! and a cave all work: the answer is the support surface below the point. A heightfield probe may
//! ignore the point's Y and answer the terrain height at its XZ, which also covers a point under
//! the terrain. A probe can also answer rays directly ([`GroundProbeAppExt::add_ground_probe_with_ray`]);
//! a probe that answers heights only is ray-marched.

mod colliders;
mod meshes;

use avian3d::prelude::PhysicsSystems;
use bevy::{asset::AssetEventSystems, ecs::system::SystemId, math::DVec3, prelude::*};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Rays are followed this far, in metres.
pub const MAX_RAY_DISTANCE: f64 = 40_000.0;

/// First march step, in metres; later steps grow with distance.
const MIN_MARCH_STEP: f64 = 0.5;
/// Each march step is at least this fraction of the distance already covered.
const MARCH_GROWTH: f64 = 0.03;
const BISECTION_STEPS: u32 = 24;

/// Which ground probes a query asks.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroundGuardMode {
    /// No probe: no guard, no ground, no view-centre hit.
    None,
    /// The built-in probe over static colliders, and game probes.
    Colliders,
    /// The built-in probe over marked render meshes, and game probes.
    Meshes,
    /// Both built-in probes, and game probes.
    #[default]
    Both,
}

impl GroundGuardMode {
    /// Whether this mode asks probes of `kind`.
    fn asks(self, kind: ProbeKind) -> bool {
        match (self, kind) {
            (Self::None, _) => false,
            (_, ProbeKind::Custom) | (Self::Both, _) => true,
            (Self::Colliders, ProbeKind::Colliders) | (Self::Meshes, ProbeKind::Meshes) => true,
            _ => false,
        }
    }
}

/// The camera ground guard: the [`GroundSurface`] marker, the [`CameraGroundGuard`] defaults (no
/// guard unless a game changes them), the probe registry and the built-in colliders and mesh
/// probes.
pub struct CameraGroundPlugin;

impl Plugin for CameraGroundPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<GroundSurface>()
            .register_type::<CameraGroundGuard>()
            .init_resource::<CameraGroundGuard>()
            .init_resource::<GroundProbes>()
            .init_resource::<colliders::PhysicsFrame>()
            .init_resource::<meshes::MeshGroundCache>()
            .add_systems(
                FixedPostUpdate,
                colliders::record_physics_frame.in_set(PhysicsSystems::Last),
            )
            // Right after the frame's asset events are written, so every later query that frame,
            // a pending placement in `Last` included, sees a modified mesh's new bounds.
            .add_systems(
                PostUpdate,
                meshes::forget_changed_bounds
                    .after(AssetEventSystems)
                    .run_if(resource_exists::<Messages<AssetEvent<Mesh>>>),
            );
        add_probe(
            app,
            ProbeKind::Colliders,
            colliders::physics_ground,
            colliders::physics_ray,
        );
        add_probe(
            app,
            ProbeKind::Meshes,
            meshes::mesh_ground,
            meshes::mesh_ray,
        );
    }
}

/// A game's default for camera placement: which probes keep a placed camera above the ground,
/// and how far above it. Commands take each per call and fall back to this. The default is both
/// built-in probes and 2 m.
#[derive(Resource, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Resource, Default)]
pub struct CameraGroundGuard {
    pub ground: GroundGuardMode,
    /// Metres the camera is kept above the ground.
    pub min_clearance: f64,
}

impl Default for CameraGroundGuard {
    fn default() -> Self {
        Self {
            ground: GroundGuardMode::Both,
            min_clearance: 2.0,
        }
    }
}

/// Which probe answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// The built-in probe over static colliders.
    Colliders,
    /// The built-in probe over marked render meshes.
    Meshes,
    /// A probe the game registered.
    Custom,
}

/// A probe's answer about the support surface below a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GroundAnswer {
    /// The surface height in absolute metres.
    Surface(f64),
    /// Nothing below the point: open space, a void, outside the map.
    NoGround,
    /// The probe would know, but its data for this point has not loaded yet.
    NotLoaded,
}

/// A ray in absolute metres for a probe that answers rays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundRay {
    pub origin: DVec3,
    /// Unit length.
    pub direction: DVec3,
    pub max_distance: f64,
}

/// A probe's answer to a [`GroundRay`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayAnswer {
    /// Distance along the ray to the first surface.
    Hit(f64),
    Miss,
    NotLoaded,
}

/// Marks terrain for the ground probes: a static collider or the static body it belongs to (the
/// colliders probe), or a mesh or any of its ancestors (the mesh probe). A camera found under
/// marked terrain is lifted onto it; the mesh probe sees only marked meshes. Registered for
/// reflection by the engine's camera plugin, so a stage saved with it loads in any build.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component, Default)]
pub struct GroundSurface;

/// Every probe asked, combined.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GroundSample {
    /// The highest surface any probe reported, in absolute metres.
    pub surface: Option<f64>,
    /// The probe that reported it.
    pub from: Option<ProbeKind>,
    /// Some probe has not loaded its data for this point, so a higher surface may still come.
    pub not_loaded: bool,
}

#[derive(Clone, Copy)]
struct GroundProbe {
    kind: ProbeKind,
    height: SystemId<In<DVec3>, GroundAnswer>,
    ray: Option<SystemId<In<GroundRay>, RayAnswer>>,
}

/// The registered probes.
#[derive(Resource, Default)]
struct GroundProbes(Vec<GroundProbe>);

/// Registering ground probes on an [`App`].
pub trait GroundProbeAppExt {
    /// A probe answering the support surface below an absolute point. Rays are marched over it.
    fn add_ground_probe<M>(
        &mut self,
        height: impl IntoSystem<In<DVec3>, GroundAnswer, M> + 'static,
    ) -> &mut Self;

    /// A probe that also answers rays itself, which handles walls, ceilings and overhangs a
    /// march over heights cannot.
    fn add_ground_probe_with_ray<M, N>(
        &mut self,
        height: impl IntoSystem<In<DVec3>, GroundAnswer, M> + 'static,
        ray: impl IntoSystem<In<GroundRay>, RayAnswer, N> + 'static,
    ) -> &mut Self;
}

impl GroundProbeAppExt for App {
    fn add_ground_probe<M>(
        &mut self,
        height: impl IntoSystem<In<DVec3>, GroundAnswer, M> + 'static,
    ) -> &mut Self {
        let world = self.world_mut();
        let height = world.register_system(height);
        push_probe(world, ProbeKind::Custom, height, None);
        self
    }

    fn add_ground_probe_with_ray<M, N>(
        &mut self,
        height: impl IntoSystem<In<DVec3>, GroundAnswer, M> + 'static,
        ray: impl IntoSystem<In<GroundRay>, RayAnswer, N> + 'static,
    ) -> &mut Self {
        add_probe(self, ProbeKind::Custom, height, ray);
        self
    }
}

fn add_probe<M, N>(
    app: &mut App,
    kind: ProbeKind,
    height: impl IntoSystem<In<DVec3>, GroundAnswer, M> + 'static,
    ray: impl IntoSystem<In<GroundRay>, RayAnswer, N> + 'static,
) {
    let world = app.world_mut();
    let height = world.register_system(height);
    let ray = Some(world.register_system(ray));
    push_probe(world, kind, height, ray);
}

fn push_probe(
    world: &mut World,
    kind: ProbeKind,
    height: SystemId<In<DVec3>, GroundAnswer>,
    ray: Option<SystemId<In<GroundRay>, RayAnswer>>,
) {
    world
        .get_resource_or_init::<GroundProbes>()
        .0
        .push(GroundProbe { kind, height, ray });
}

/// Whether `mode` asks any probe. Without one, the ground guard has nothing to ask.
pub fn has_ground_probes(world: &World, mode: GroundGuardMode) -> bool {
    !probes(world, mode).is_empty()
}

/// The support surface below `point`, over the probes `mode` asks: the highest surface wins.
pub fn ground_below(world: &mut World, point: DVec3, mode: GroundGuardMode) -> GroundSample {
    ask(world, mode);
    sample(world, point, mode, false)
}

/// The mode of the query the probes are being asked for, so a built-in probe can tell which
/// others are asked with it.
#[derive(Resource, Clone, Copy, Debug)]
struct AskedMode(GroundGuardMode);

fn ask(world: &mut World, mode: GroundGuardMode) {
    world.insert_resource(AskedMode(mode));
}

/// Distance along the ray to the first surface the probes `mode` asks see within
/// `max_distance`, or `None`. Ray probes answer for themselves; height-only probes are marched.
pub fn cast_ground_ray(
    world: &mut World,
    origin: DVec3,
    direction: Dir3,
    max_distance: f64,
    mode: GroundGuardMode,
) -> Option<f64> {
    ask(world, mode);
    let direction = direction.as_vec3().as_dvec3().normalize();
    let ray = GroundRay {
        origin,
        direction,
        max_distance,
    };
    let direct = probes(world, mode)
        .into_iter()
        .filter_map(|probe| probe.ray)
        .filter_map(|id| match world.run_system_with(id, ray) {
            Ok(RayAnswer::Hit(distance)) => Some(distance),
            Ok(_) => None,
            Err(err) => {
                debug!("ground ray probe did not run: {err}");
                None
            }
        })
        .min_by(f64::total_cmp);
    let marched = march(world, ray, mode);
    [direct, marched]
        .into_iter()
        .flatten()
        .min_by(f64::total_cmp)
}

/// The lowest ceiling a probe that answers rays reports above a point, with that probe's ground.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ceiling {
    height: f64,
    /// The ground of the probe the ceiling belongs to; a lift is never capped below it.
    ground: Option<f64>,
}

/// The lowest ceiling above `point`: what each probe `mode` asks that answers rays hits straight
/// up, from the point or from that probe's ground when the ground is above the point (a camera
/// inside a solid or under the terrain). Height-only probes, such as a heightfield, have no
/// ceilings.
fn ceiling_above(world: &mut World, point: DVec3, mode: GroundGuardMode) -> Option<Ceiling> {
    ask(world, mode);
    let mut ceilings = Vec::new();
    for probe in probes(world, mode) {
        let Some(ray) = probe.ray else {
            continue;
        };
        let ground = match world.run_system_with(probe.height, point) {
            Ok(GroundAnswer::Surface(ground)) => Some(ground),
            _ => None,
        };
        let from = ground.map_or(point.y, |ground| ground.max(point.y));
        let up = GroundRay {
            origin: DVec3::new(point.x, from, point.z),
            direction: DVec3::Y,
            max_distance: MAX_RAY_DISTANCE,
        };
        if let Ok(RayAnswer::Hit(distance)) = world.run_system_with(ray, up) {
            ceilings.push(Ceiling {
                height: from + distance,
                ground,
            });
        }
    }
    ceilings
        .into_iter()
        .min_by(|a, b| a.height.total_cmp(&b.height))
}

/// A lift stops this far below a ceiling, in metres.
pub const CEILING_MARGIN: f64 = 0.1;

/// Where the guard would put a camera at a point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiftPlan {
    pub ground: GroundSample,
    /// The lowest ceiling above the ground.
    pub ceiling: Option<f64>,
    /// The height to lift the camera to, when that is above it.
    pub target: Option<f64>,
    /// A ceiling kept the lift below `ground + min_clearance`.
    pub limited: bool,
    /// The ceiling is within [`CEILING_MARGIN`] of its ground, so the lift goes onto the ground.
    pub crowded: bool,
}

/// Plans the lift of a camera at `point` to `min_clearance` above the ground below it, over the
/// probes `mode` asks, capped [`CEILING_MARGIN`] under the ceiling above that ground, and never
/// below the ground of the probe the ceiling belongs to: with too little room, the camera goes
/// onto the ground.
pub fn plan_lift(
    world: &mut World,
    point: DVec3,
    min_clearance: f64,
    mode: GroundGuardMode,
) -> LiftPlan {
    let ground = ground_below(world, point, mode);
    let ceiling = ceiling_above(world, point, mode);
    let Some(surface) = ground.surface else {
        return LiftPlan {
            ground,
            ceiling: ceiling.map(|c| c.height),
            ..default()
        };
    };
    let wanted = surface + min_clearance;
    let cap = ceiling.map(|c| (c.height - CEILING_MARGIN).max(c.ground.unwrap_or(f64::MIN)));
    let target = cap.map_or(wanted, |cap| wanted.min(cap));
    LiftPlan {
        ground,
        ceiling: ceiling.map(|c| c.height),
        target: (target > point.y).then_some(target),
        limited: cap.is_some_and(|cap| cap < wanted),
        crowded: ceiling.is_some_and(|c| c.ground.is_some_and(|g| c.height - CEILING_MARGIN <= g)),
    }
}

/// Where the view centre of a camera at `origin` looking along `direction` meets a surface,
/// within [`MAX_RAY_DISTANCE`], over the probes `mode` asks.
pub fn center_hit(
    world: &mut World,
    origin: DVec3,
    direction: Dir3,
    mode: GroundGuardMode,
) -> Option<DVec3> {
    let distance = cast_ground_ray(world, origin, direction, MAX_RAY_DISTANCE, mode)?;
    Some(origin + direction.as_vec3().as_dvec3().normalize() * distance)
}

fn probes(world: &World, mode: GroundGuardMode) -> Vec<GroundProbe> {
    world
        .get_resource::<GroundProbes>()
        .map(|probes| {
            probes
                .0
                .iter()
                .filter(|probe| mode.asks(probe.kind))
                .copied()
                .collect()
        })
        .unwrap_or_default()
}

/// Combines the probes `mode` asks, or only those that answer no rays when `height_only`.
fn sample(
    world: &mut World,
    point: DVec3,
    mode: GroundGuardMode,
    height_only: bool,
) -> GroundSample {
    let mut combined = GroundSample::default();
    for probe in probes(world, mode) {
        if height_only && probe.ray.is_some() {
            continue;
        }
        match world.run_system_with(probe.height, point) {
            Ok(GroundAnswer::Surface(surface)) => {
                if combined.surface.is_none_or(|s| surface > s) {
                    combined.surface = Some(surface);
                    combined.from = Some(probe.kind);
                }
            }
            Ok(GroundAnswer::NoGround) => {}
            Ok(GroundAnswer::NotLoaded) => combined.not_loaded = true,
            // A probe whose data is missing (no physics in this app) answers nothing.
            Err(err) => debug!("ground probe did not run: {err}"),
        }
    }
    combined
}

/// Marches the ray over the height-only probes and refines a crossing by bisection.
///
/// Refines only between a known-above and a known-below sample; an unknown sample resets the
/// bracket. Treating unknown as "above" would converge on the edge of the loaded data when a ray
/// passes over unloaded ground and comes out below loaded ground.
fn march(world: &mut World, ray: GroundRay, mode: GroundGuardMode) -> Option<f64> {
    if !probes(world, mode).iter().any(|probe| probe.ray.is_none()) {
        return None;
    }
    let height_at = |world: &mut World, t: f64| {
        let point = ray.origin + ray.direction * t;
        let sample = sample(world, point, mode, true);
        // Any probe still loading makes the sample unknown, even when another one answered: the
        // missing data may be the higher surface.
        if sample.not_loaded {
            return None;
        }
        sample.surface.map(|surface| point.y - surface)
    };
    let mut above: Option<f64> = None;
    let mut t = 0.0;
    loop {
        match height_at(world, t) {
            None => above = None,
            Some(clearance) if clearance > 0.0 => above = Some(t),
            Some(_) => {
                if let Some(start) = above {
                    if let Some(hit) = bisect(world, start, t, &height_at) {
                        return Some(hit);
                    }
                    // Unloaded data inside the bracket: no valid crossing here.
                    above = None;
                }
            }
        }
        if t >= ray.max_distance {
            return None;
        }
        t = (t + MIN_MARCH_STEP.max(t * MARCH_GROWTH)).min(ray.max_distance);
    }
}

fn bisect(
    world: &mut World,
    mut above: f64,
    mut below: f64,
    height_at: &impl Fn(&mut World, f64) -> Option<f64>,
) -> Option<f64> {
    for _ in 0..BISECTION_STEPS {
        let mid = (above + below) / 2.0;
        match height_at(world, mid) {
            Some(clearance) if clearance > 0.0 => above = mid,
            Some(_) => below = mid,
            // Unknown inside the bracket voids it, as an unknown sample does while marching.
            None => return None,
        }
    }
    Some(below)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The probes here are game probes, asked in every mode but `none`.
    fn center_hit(world: &mut World, origin: DVec3, direction: Dir3) -> Option<DVec3> {
        super::center_hit(world, origin, direction, GroundGuardMode::Meshes)
    }

    fn flat(In(_): In<DVec3>) -> GroundAnswer {
        GroundAnswer::Surface(0.0)
    }

    fn app_with(build: impl FnOnce(&mut App)) -> App {
        let mut app = App::new();
        build(&mut app);
        app
    }

    #[test]
    fn unloaded_data_found_while_refining_is_no_hit() {
        // Low ground, a gap narrower than one march step, then ground far above the ray.
        fn narrow_gap(In(p): In<DVec3>) -> GroundAnswer {
            match p.x {
                x if x < 4.1 => GroundAnswer::Surface(0.0),
                x if x < 4.4 => GroundAnswer::NotLoaded,
                _ => GroundAnswer::Surface(100.0),
            }
        }
        let mut app = app_with(|app| {
            app.add_ground_probe(narrow_gap);
        });
        assert_eq!(
            center_hit(app.world_mut(), DVec3::new(0.0, 50.0, 0.0), Dir3::X),
            None
        );
        // A loaded probe beside it does not hide the gap.
        let mut app = app_with(|app| {
            app.add_ground_probe(narrow_gap).add_ground_probe(flat);
        });
        assert_eq!(
            center_hit(app.world_mut(), DVec3::new(0.0, 50.0, 0.0), Dir3::X),
            None
        );
    }

    #[test]
    fn each_mode_asks_its_probes_and_game_probes_in_all_but_none() {
        fn at(height: f64) -> impl Fn(In<DVec3>) -> GroundAnswer {
            move |In(_): In<DVec3>| GroundAnswer::Surface(height)
        }
        fn miss(In(_): In<GroundRay>) -> RayAnswer {
            RayAnswer::Miss
        }
        let mut app = App::new();
        add_probe(&mut app, ProbeKind::Colliders, at(1.0), miss);
        add_probe(&mut app, ProbeKind::Meshes, at(2.0), miss);
        app.add_ground_probe(at(0.5));
        let world = app.world_mut();
        let sample = |world: &mut World, mode| super::ground_below(world, DVec3::Y * 9.0, mode);
        assert_eq!(
            sample(world, GroundGuardMode::None),
            GroundSample::default()
        );
        let colliders = sample(world, GroundGuardMode::Colliders);
        assert_eq!(
            (colliders.surface, colliders.from),
            (Some(1.0), Some(ProbeKind::Colliders))
        );
        let meshes = sample(world, GroundGuardMode::Meshes);
        assert_eq!(
            (meshes.surface, meshes.from),
            (Some(2.0), Some(ProbeKind::Meshes))
        );
        let both = sample(world, GroundGuardMode::Both);
        assert_eq!(
            (both.surface, both.from),
            (Some(2.0), Some(ProbeKind::Meshes))
        );
        assert!(!super::has_ground_probes(world, GroundGuardMode::None));
    }

    #[test]
    fn a_lift_in_a_room_clears_the_slab_it_is_in_and_stops_under_a_ceiling() {
        // A floor slab with its top at 0 and a ceiling slab from 2.5 to 2.8 (the upper floor).
        const SLABS: [(f64, f64); 2] = [(-1.0, 0.0), (2.5, 2.8)];
        fn room(In(point): In<DVec3>) -> GroundAnswer {
            SLABS
                .iter()
                .rposition(|&(bottom, _)| bottom <= point.y)
                .map_or(GroundAnswer::NoGround, |i| {
                    GroundAnswer::Surface(SLABS[i].1)
                })
        }
        fn room_ray(In(ray): In<GroundRay>) -> RayAnswer {
            [0.0, 2.5, 2.8]
                .into_iter()
                .map(|plane| (plane - ray.origin.y) / ray.direction.y)
                .filter(|&t| t > 0.0 && t <= ray.max_distance)
                .min_by(f64::total_cmp)
                .map_or(RayAnswer::Miss, RayAnswer::Hit)
        }
        let mut app = app_with(|app| {
            app.add_ground_probe_with_ray(room, room_ray);
        });
        // Inside the ceiling slab: onto its top, with nothing above it.
        let plan = ground_lift(app.world_mut(), DVec3::Y * 2.6);
        assert_eq!(plan.ground.surface, Some(2.8));
        assert_eq!(plan.ceiling, None);
        assert!((plan.target.unwrap() - 4.8).abs() < 1e-9, "{plan:?}");

        // A heightfield reporting terrain at 10 over the room: capped under the room's ceiling.
        app.add_ground_probe(|In(_): In<DVec3>| GroundAnswer::Surface(10.0));
        let plan = ground_lift(app.world_mut(), DVec3::Y);
        assert_eq!(plan.ground.surface, Some(10.0));
        assert_eq!(plan.ceiling, Some(2.5));
        assert!((plan.target.unwrap() - 2.4).abs() < 1e-9, "{plan:?}");
        assert!(plan.limited && !plan.crowded, "{plan:?}");
    }

    fn ground_lift(world: &mut World, point: DVec3) -> LiftPlan {
        super::plan_lift(world, point, 2.0, GroundGuardMode::Both)
    }
}
