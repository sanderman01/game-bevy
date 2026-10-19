//! Keeps a camera (or any entity) attached to the nearest root `Grid`, gaining `CellCoord` and
//! `FloatingOrigin` while one exists, and living parentless with a plain `Transform` when none
//! does. See `docs/superpowers/plans/2026-09-15-editor-scene-camera.md`.

use bevy::ecs::relationship::{RelationshipSourceCollection, RelationshipTarget};
use bevy::ecs::system::SystemParam;
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use big_space::camera::camera_controller;
use big_space::prelude::{BigSpace, BigSpaceCameraController, CellCoord, FloatingOrigin, Grid};

use super::grid::local_in_parent;

/// Attach this to any entity that should automatically become a child of the nearest root
/// `Grid` (an entity carrying both `BigSpace` and `Grid`) when one exists, and a parentless root
/// entity when none does. Always carries a [`BigSpaceCameraController`] so its
/// `speed`/`smoothness`/`speed_bounds` configuration is a single value that survives attaching
/// and detaching -- big_space's own systems simply never match this entity while it lacks
/// `CellCoord`.
#[derive(Component, Debug, Default, Clone, Copy)]
#[require(BigSpaceCameraController, FloatingOriginCandidate)]
pub struct GridFollowCamera;

/// Marks an entity as eligible to hold big_space's [`FloatingOrigin`], and says how strongly it
/// wants it: [`elect_floating_origins`] gives the origin to the highest-priority candidate under
/// each root `BigSpace`. Higher wins; `0` is the default.
///
/// Candidacy is a component of its own rather than something `MainCamera` requires, because the
/// origin belongs wherever the content author puts it -- a vehicle a camera is mounted to is as
/// reasonable a choice as the camera. Stages serialize it (unlike [`FloatingOrigin`] itself, which
/// `DynamicWorldFormat` denies): which entities are *eligible* is content, which one *holds* it is
/// runtime policy. See `docs/design/floating-origin.md`.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct FloatingOriginCandidate(pub i32);

/// Present on a [`GridFollowCamera`] entity while its floating-origin recentering is frozen:
/// names the stationary anchor entity holding [`FloatingOrigin`] in its place. Set and cleared by
/// [`set_origin_frozen`], and cleared (with the anchor despawned) automatically if the entity
/// detaches from its grid.
#[derive(Component, Debug, Clone, Copy)]
pub struct FrozenOrigin(pub Entity);

/// Runs before big_space's own `camera_controller` consumes `BigSpaceCameraInput`. A layer
/// driving a [`GridFollowCamera`] orders its `BigSpaceCameraInput`-writing system into this set
/// so big_space applies the same frame's input. This is the one ordering point a layer above the
/// engine needs; it never has to name `big_space::camera::camera_controller` itself.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GridCameraSystems {
    Apply,
}

/// Registers [`sync_grid_attachment`] and [`GridCameraSystems`]'s ordering.
pub struct GridFollowPlugin;

impl Plugin for GridFollowPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<FloatingOriginCandidate>()
            .configure_sets(
                PostUpdate,
                GridCameraSystems::Apply.before(camera_controller),
            )
            // `.chain()` is what lets the election observe the attachment `sync_grid_attachment`
            // just queued: `auto_insert_apply_deferred` defaults to true, so Bevy puts a sync
            // point between two systems with an explicit order dependency when the earlier one
            // defers. Reading a frame-old `ChildOf`/`CellCoord` here would cost a frame of
            // `find_floating_origin` errors on every stage load.
            //
            // Nothing needs ordering against `BigSpaceSystems`: `GridCameraSystems::Apply` is
            // already before `camera_controller`, which big_space schedules before
            // `TransformSystems::Propagate` -- and `find_floating_origin` runs inside that set.
            .add_systems(
                PostUpdate,
                (
                    sync_grid_attachment.run_if(grid_attachment_may_change),
                    elect_floating_origins,
                )
                    .chain()
                    .before(GridCameraSystems::Apply),
            )
            .add_observer(rescue_followers_on_grid_despawn)
            .add_observer(rescue_followers_on_grid_remove);
    }
}

/// Whether the set of root grids (entities with both `BigSpace` and `Grid`) changed or a follower
/// just spawned. [`sync_grid_attachment`] runs only then, not every frame, to avoid churn from
/// continuously recomputing "nearest grid" while nothing has actually changed.
fn grid_attachment_may_change(
    new_roots: Query<(), (Added<BigSpace>, With<Grid>)>,
    new_followers: Query<(), Added<GridFollowCamera>>,
    removed_grids: RemovedComponents<Grid>,
) -> bool {
    !(new_roots.is_empty() && new_followers.is_empty() && removed_grids.is_empty())
}

/// Re-evaluates every [`GridFollowCamera`]'s attachment.
#[allow(clippy::type_complexity)]
fn sync_grid_attachment(
    mut commands: Commands,
    followers: Query<(Entity, Option<&ChildOf>, Option<&FrozenOrigin>), With<GridFollowCamera>>,
    grid_roots: Query<(Entity, Option<&Transform>), (With<BigSpace>, With<Grid>)>,
    grids: Query<&Grid>,
    links: Query<LinkData>,
) {
    for (camera, child_of, frozen) in &followers {
        let current_grid = child_of
            .map(ChildOf::parent)
            .filter(|parent| grids.contains(*parent));
        let (position, rotation) = true_pose(camera, &links);

        // `position` is in the follower's root grid's frame, and the follower keeps those
        // coordinates when it changes roots: big_space ignores a root grid's own `Transform`, so
        // every root's contents share one frame. The roots' `Transform`s (zero without one) only
        // rank them, as if placed in that frame; a stage's root grid has none. The current grid
        // is kept on a tie, so a second root appearing at the same place does not steal the
        // follower.
        let distance_sq = |root: Option<&Transform>| {
            position.distance_squared(root.map_or(Vec3::ZERO, |root| root.translation))
        };
        let current_distance = current_grid
            .and_then(|grid| grid_roots.get(grid).ok())
            .map(|(_, root)| distance_sq(root));
        let nearest_grid = grid_roots
            .iter()
            .map(|(entity, root)| (entity, distance_sq(root)))
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .filter(|&(_, nearest)| current_distance.is_none_or(|current| nearest < current))
            .map(|(entity, _)| entity)
            .or(current_distance.and(current_grid));

        if current_grid == nearest_grid {
            continue;
        }

        if let Some(FrozenOrigin(anchor)) = frozen {
            commands.entity(*anchor).despawn();
            commands.entity(camera).remove::<FrozenOrigin>();
        }

        match nearest_grid {
            Some(grid_entity) => {
                let grid = grids
                    .get(grid_entity)
                    .expect("nearest_grid always has Grid");
                let (cell, remainder) = grid.translation_to_grid(position.as_dvec3());
                // No `FloatingOrigin` here: attaching only makes this entity *eligible*, and
                // `elect_floating_origins` (chained directly after this system) decides.
                commands.entity(camera).insert((
                    cell,
                    Transform {
                        translation: remainder,
                        rotation,
                        scale: Vec3::ONE,
                    },
                    ChildOf(grid_entity),
                ));
            }
            None => {
                commands
                    .entity(camera)
                    .remove::<(CellCoord, ChildOf)>()
                    .insert(Transform {
                        translation: position,
                        rotation,
                        scale: Vec3::ONE,
                    });
            }
        }
    }
}

/// Maintains big_space's one hard invariant: exactly one [`FloatingOrigin`] per root `BigSpace`.
/// The highest-priority [`FloatingOriginCandidate`] under each root gets it, everyone else loses
/// it, and a root with no candidate gets none -- the same state a detached camera already
/// produced. This is the only place `FloatingOrigin` is *granted*.
///
/// It is not the only place it is taken away. [`set_origin_frozen`] takes `&mut World` and is
/// expected to have landed by the time it returns, and a grid's loss detaches its followers
/// outside this system's schedule (`rescue_grid_followers`), so both strip (or move) the marker
/// themselves rather than leave the world holding a stale origin for a frame. Each leaves the
/// world in a state this system then agrees with, so the two never fight.
///
/// Both ways of breaking the invariant are logged by `BigSpace::find_floating_origin` every frame,
/// but they are not equally bad: a missing origin stops propagation for that space, while a
/// *duplicate* also clears `BigSpace::floating_origin` outright. That asymmetry is why this runs
/// as a single elect-and-diff rather than letting each interested party manage its own marker.
///
/// A camera holding [`FrozenOrigin`] is deliberately excluded: its anchor stands in for it and
/// carries a copy of its priority, so without the filter the tie between the two would be settled
/// by entity id. See [`set_origin_frozen`].
///
/// Runs every frame instead of on change detection. Candidates number one to three in practice,
/// and a change-detection version would have to react to attachment, priority, despawn and
/// `FrozenOrigin` edits at once; the diff below already makes a steady frame issue no commands.
#[allow(clippy::type_complexity)]
fn elect_floating_origins(
    mut commands: Commands,
    candidates: Query<(Entity, &FloatingOriginCandidate), (With<CellCoord>, Without<FrozenOrigin>)>,
    holders: Query<Entity, With<FloatingOrigin>>,
    parents: Query<&ChildOf>,
    roots: Query<(), With<BigSpace>>,
) {
    // Keyed by root, valued by the best `(priority, entity)` bid seen under it. Comparing the
    // tuple makes the entity id the tie-break, so an equal-priority pair elects deterministically
    // rather than flapping frame to frame.
    let mut best_per_root: HashMap<Entity, (i32, Entity)> = HashMap::default();
    for (entity, candidate) in &candidates {
        // The same walk `find_floating_origin` does, so eligibility here means exactly what
        // big_space will conclude later in the frame.
        let Some(root) = parents.iter_ancestors(entity).last() else {
            continue;
        };
        if !roots.contains(root) {
            continue;
        }
        let bid = (candidate.0, entity);
        best_per_root
            .entry(root)
            .and_modify(|best| *best = (*best).max(bid))
            .or_insert(bid);
    }

    let elected: HashSet<Entity> = best_per_root.values().map(|&(_, entity)| entity).collect();
    for holder in &holders {
        if !elected.contains(&holder) {
            commands.entity(holder).remove::<FloatingOrigin>();
        }
    }
    for entity in elected {
        if !holders.contains(entity) {
            commands.entity(entity).insert(FloatingOrigin);
        }
    }
}

type LinkData = (
    Option<&'static Transform>,
    Option<&'static CellCoord>,
    Option<&'static ChildOf>,
    Option<&'static Grid>,
);

/// Where `entity` is in its root grid's frame, composed the way big_space propagates: local
/// `Transform`s compose as affines, as Bevy's propagation does, a grid places its contents through
/// their cell and contributes only its rotation and translation (never its scale), and the root
/// grid's own `Transform` and anything above it are ignored. Absolute when no grid is above
/// `entity`. A missing `Transform` is the identity and a missing `CellCoord` is cell zero.
///
/// Never the propagated `GlobalTransform`: inside a grid it is floating-origin-relative, and for
/// an entity moved or detached earlier this frame it is stale until propagation runs.
fn true_pose(entity: Entity, links: &Query<LinkData>) -> (Vec3, Quat) {
    let pose = own_pose(entity, links);
    (pose.affine.translation(), pose.rotation.normalize())
}

/// An entity's pose as [`true_pose`] builds it. The position comes from the affine, which keeps
/// any shear a non-uniform scale under a rotation makes; the rotation is composed on its own,
/// since decomposing a sheared or mirrored affine gives no valid rotation.
#[derive(Clone, Copy)]
struct Pose {
    affine: GlobalTransform,
    rotation: Quat,
}

impl Pose {
    const IDENTITY: Self = Self {
        affine: GlobalTransform::IDENTITY,
        rotation: Quat::IDENTITY,
    };
}

/// [`true_pose`], built top-down with big_space's grid rule ([`local_in_parent`]).
fn own_pose(entity: Entity, links: &Query<LinkData>) -> Pose {
    let (transform, cell, child_of, _) = links.get(entity).unwrap_or_default();
    let local = transform.copied().unwrap_or_default();
    let Some(parent) = child_of.map(ChildOf::parent) else {
        return Pose {
            affine: local.into(),
            rotation: local.rotation,
        };
    };
    let (frame, parent_grid) = contents_of(parent, links);
    let step = local_in_parent(parent_grid, &cell.copied().unwrap_or_default(), &local);
    Pose {
        affine: frame.affine.mul_transform(step),
        rotation: frame.rotation * step.rotation,
    }
}

/// The frame `entity`'s children are placed in, and its `Grid` if it is one. The outermost grid's
/// contents frame is the root frame itself.
fn contents_of<'a>(entity: Entity, links: &'a Query<LinkData>) -> (Pose, Option<&'a Grid>) {
    let grid = links.get(entity).ok().and_then(|(.., grid)| grid);
    let outermost_grid = grid.is_some()
        && !std::iter::successors(Some(entity), |&current| {
            links
                .get(current)
                .ok()
                .and_then(|(_, _, child_of, _)| child_of.map(ChildOf::parent))
        })
        .skip(1)
        .any(|ancestor| links.get(ancestor).is_ok_and(|(.., grid)| grid.is_some()));
    let frame = if outermost_grid {
        Pose::IDENTITY
    } else if grid.is_some() {
        // A grid's own scale, mirroring included, is not applied to its contents: rebuild the
        // frame from its position and composed rotation alone.
        let own = own_pose(entity, links);
        Pose {
            affine: Transform::from_translation(own.affine.translation())
                .with_rotation(own.rotation)
                .into(),
            rotation: own.rotation,
        }
    } else {
        own_pose(entity, links)
    };
    (frame, grid)
}

/// Rescues a despawning grid's followers. `Despawn` observers run before any `on_despawn` hook,
/// so this gets to them before `Children::on_despawn` queues their despawn. That covers every
/// despawn path: a direct one, a cascade from an ancestor (which reaches the grid through
/// commands), stage unload and a `DynamicWorld` hot reload alike.
fn rescue_followers_on_grid_despawn(despawn: On<Despawn, Grid>, mut rescue: FollowerRescue) {
    rescue.below(despawn.entity);
}

/// Rescues the followers of a grid that loses its `Grid` component. Also fires on despawn, after
/// [`rescue_followers_on_grid_despawn`] has already taken the followers, so it finds none.
fn rescue_followers_on_grid_remove(remove: On<Remove, Grid>, mut rescue: FollowerRescue) {
    rescue.below(remove.entity);
}

#[derive(SystemParam)]
struct FollowerRescue<'w, 's> {
    commands: Commands<'w, 's>,
    followers: Query<
        'w,
        's,
        (Entity, &'static ChildOf, Option<&'static FrozenOrigin>),
        With<GridFollowCamera>,
    >,
    parents: Query<'w, 's, &'static ChildOf>,
    links: Query<'w, 's, LinkData>,
    children: Query<'w, 's, &'static mut Children>,
}

impl FollowerRescue<'_, '_> {
    /// Detaches every [`GridFollowCamera`] below `grid`, at any depth, placing it where it was in
    /// its root grid's frame while `grid` and every grid enclosing the follower are still
    /// readable. A cascade despawns the outer grids first, so the inner ones could not.
    ///
    /// Structural changes are deferred, so each follower also leaves its parent's `Children` right
    /// away: a despawn's cascade then skips it, and a second call finds it gone.
    fn below(&mut self, grid: Entity) {
        let Self {
            commands,
            followers,
            parents,
            links,
            children,
        } = self;
        for (follower, child_of, frozen) in followers.iter() {
            let parent = child_of.parent();
            if parent != grid
                && !parents
                    .iter_ancestors(parent)
                    .any(|ancestor| ancestor == grid)
            {
                continue;
            }
            let Ok(mut siblings) = children.get_mut(parent) else {
                continue;
            };
            if !siblings.contains(&follower) {
                continue;
            }
            let (position, rotation) = true_pose(follower, links);
            // The follower keeps its `ChildOf` until the queued removal below; that removal's
            // `on_discard` tolerates both the missing `Children` entry and a despawned parent.
            RelationshipSourceCollection::remove(siblings.collection_mut_risky(), follower);

            if let Some(FrozenOrigin(anchor)) = frozen {
                commands.entity(*anchor).try_despawn();
            }
            // `try_`: a command queued ahead of these may despawn the follower itself.
            commands
                .entity(follower)
                .try_remove::<(CellCoord, FloatingOrigin, FrozenOrigin, ChildOf)>()
                .try_insert(Transform {
                    translation: position,
                    rotation,
                    scale: Vec3::ONE,
                });
        }
    }
}

/// Toggles floating-origin recentering for a [`GridFollowCamera`] entity that currently has a
/// `CellCoord` (is attached to a grid). `frozen: true` spawns a stationary anchor as a sibling in
/// the same grid, snapshots the camera's current cell and transform onto it, and moves
/// [`FloatingOrigin`] there so the camera can keep flying without the world recentering around
/// it. `frozen: false` reverses this, despawning the anchor. A no-op if `camera` has no
/// `CellCoord` (nothing to freeze without a grid), or if it is already in the requested state.
pub fn set_origin_frozen(world: &mut World, camera: Entity, frozen: bool) {
    let Some(cell) = world.get::<CellCoord>(camera).copied() else {
        return;
    };
    let already_frozen = world.get::<FrozenOrigin>(camera).copied();

    match (frozen, already_frozen) {
        (true, None) => {
            let transform = *world
                .get::<Transform>(camera)
                .expect("CellCoord requires Transform");
            let grid_entity = world
                .get::<ChildOf>(camera)
                .expect("a CellCoord entity is always a child of its Grid")
                .parent();
            // The anchor spawns as a candidate carrying the camera's own priority, so
            // `elect_floating_origins` leaves the origin where this puts it instead of electing it
            // straight back to the camera. The camera itself is excluded from the election for as
            // long as `FrozenOrigin` is on it -- equal priorities would otherwise tie.
            let priority = world
                .get::<FloatingOriginCandidate>(camera)
                .copied()
                .unwrap_or_default();
            let anchor = world
                .spawn((
                    cell,
                    transform,
                    priority,
                    FloatingOrigin,
                    ChildOf(grid_entity),
                ))
                .id();
            world
                .entity_mut(camera)
                .remove::<FloatingOrigin>()
                .insert(FrozenOrigin(anchor));
        }
        (false, Some(FrozenOrigin(anchor))) => {
            if let Ok(anchor) = world.get_entity_mut(anchor) {
                anchor.despawn();
            }
            world
                .entity_mut(camera)
                .remove::<FrozenOrigin>()
                .insert(FloatingOrigin);
        }
        _ => {}
    }
}
