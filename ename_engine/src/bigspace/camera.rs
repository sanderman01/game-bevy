//! Keeps a camera (or any entity) attached to the nearest root `Grid`, gaining `CellCoord` and
//! `FloatingOrigin` while one exists, and living parentless with a plain `Transform` when none
//! does. See `docs/superpowers/plans/2026-09-15-editor-scene-camera.md`.

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use big_space::camera::camera_controller;
use big_space::prelude::{BigSpaceCameraController, CellCoord, FloatingOrigin, Grid};

/// Attach this to any entity that should automatically become a child of the nearest root
/// `Grid` (an entity carrying both `BigSpace` and `Grid`) when one exists, and a parentless root
/// entity when none does. Always carries a [`BigSpaceCameraController`] so its
/// `speed`/`smoothness`/`speed_bounds` configuration is a single value that survives attaching
/// and detaching -- big_space's own systems simply never match this entity while it lacks
/// `CellCoord`.
#[derive(Component, Debug, Default, Clone, Copy)]
#[require(BigSpaceCameraController)]
pub struct GridFollowCamera;

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
        app.configure_sets(
            PostUpdate,
            GridCameraSystems::Apply.before(camera_controller),
        )
        .add_systems(
            PostUpdate,
            sync_grid_attachment.before(GridCameraSystems::Apply),
        );
        app.world_mut()
            .register_component_hooks::<Grid>()
            .on_remove(rescue_children_on_grid_removed);
    }
}

/// Re-evaluates every [`GridFollowCamera`]'s attachment only when the set of root grids
/// (entities with both `BigSpace` and `Grid`) has changed, or a follower has just spawned --
/// not every frame, to avoid churn from continuously recomputing "nearest grid" while nothing has
/// actually changed.
#[allow(clippy::type_complexity)]
fn sync_grid_attachment(
    mut commands: Commands,
    followers: Query<
        (
            Entity,
            &GlobalTransform,
            &Transform,
            Option<&CellCoord>,
            Option<&ChildOf>,
            Option<&FrozenOrigin>,
        ),
        With<GridFollowCamera>,
    >,
    #[allow(clippy::type_complexity)] grid_roots: Query<
        (Entity, &GlobalTransform),
        (With<big_space::prelude::BigSpace>, With<Grid>),
    >,
    grids: Query<&Grid>,
    new_roots: Query<Entity, (Added<big_space::prelude::BigSpace>, With<Grid>)>,
    new_followers: Query<Entity, Added<GridFollowCamera>>,
    removed_grids: RemovedComponents<Grid>,
) {
    if new_roots.is_empty() && new_followers.is_empty() && removed_grids.is_empty() {
        return;
    }

    for (camera, camera_global, camera_local, cell, child_of, frozen) in &followers {
        let current_grid = child_of
            .map(ChildOf::parent)
            .filter(|parent| grids.contains(*parent));

        let nearest_grid = grid_roots
            .iter()
            .min_by(|(_, a), (_, b)| {
                dist_sq(camera_global, a).total_cmp(&dist_sq(camera_global, b))
            })
            .map(|(entity, _)| entity);

        if current_grid == nearest_grid {
            continue;
        }

        if let Some(FrozenOrigin(anchor)) = frozen {
            commands.entity(*anchor).despawn();
            commands.entity(camera).remove::<FrozenOrigin>();
        }

        // Resolve the true position through whichever frame is currently authoritative:
        // grid-relative if attached (never `GlobalTransform`, which is floating-origin-relative
        // and reads as "near the origin" if this entity currently *is* the floating origin), or
        // the plain `Transform` if already parentless (where `GlobalTransform` trivially equals
        // it, since there is no ancestor chain).
        let (position, rotation) = match current_grid {
            Some(grid_entity) => {
                let grid = grids
                    .get(grid_entity)
                    .expect("current_grid always has Grid");
                grid_relative_position(grid, cell.copied().unwrap_or_default(), camera_local)
            }
            None => (camera_global.translation(), camera_global.rotation()),
        };

        match nearest_grid {
            Some(grid_entity) => {
                let grid = grids
                    .get(grid_entity)
                    .expect("nearest_grid always has Grid");
                let (cell, remainder) = grid.translation_to_grid(position.as_dvec3());
                commands.entity(camera).insert((
                    cell,
                    Transform {
                        translation: remainder,
                        rotation,
                        scale: Vec3::ONE,
                    },
                    FloatingOrigin,
                    ChildOf(grid_entity),
                ));
            }
            None => {
                commands
                    .entity(camera)
                    .remove::<(CellCoord, FloatingOrigin, ChildOf)>()
                    .insert(Transform {
                        translation: position,
                        rotation,
                        scale: Vec3::ONE,
                    });
            }
        }
    }
}

fn grid_relative_position(grid: &Grid, cell: CellCoord, local: &Transform) -> (Vec3, Quat) {
    (grid.grid_position(&cell, local), local.rotation)
}

fn dist_sq(a: &GlobalTransform, b: &GlobalTransform) -> f32 {
    a.translation().distance_squared(b.translation())
}

/// Runs when a `Grid` component is explicitly removed via `remove::<Grid>()` while the `Grid`
/// component is still present and readable. `RemovedComponents` only observes this a frame later,
/// by which point the `Grid`'s data is already gone. Detaches any `GridFollowCamera` children
/// immediately, using this still-live `Grid` to compute their correct world positions via
/// `Grid::grid_position` synchronously, deferring only structural changes (remove/insert) via
/// `Commands`.
///
/// Note: when a Grid entity is despawned (not just `.remove::<Grid>()`), its `Children`'s
/// cascade-despawn runs before this hook, leaving no children to rescue. Task 2's explicit
/// `detach_from_grid` call in stage unload handles that path and must remain in place.
fn rescue_children_on_grid_removed(mut world: DeferredWorld, context: HookContext) {
    let grid_entity = context.entity;
    let Some(grid) = world.get::<Grid>(grid_entity).cloned() else {
        return;
    };
    let Some(children) = world.get::<Children>(grid_entity) else {
        return;
    };
    let followers: Vec<Entity> = children
        .iter()
        .filter(|&child| world.get::<GridFollowCamera>(child).is_some())
        .collect();

    for follower in followers {
        let Some(cell) = world.get::<CellCoord>(follower).copied() else {
            continue;
        };
        let local = *world
            .get::<Transform>(follower)
            .expect("CellCoord requires Transform");
        let position = grid.grid_position(&cell, &local);
        let frozen_anchor = world
            .get::<FrozenOrigin>(follower)
            .map(|FrozenOrigin(a)| *a);

        let mut commands = world.commands();
        if let Some(anchor) = frozen_anchor {
            commands.entity(anchor).try_despawn();
        }
        commands
            .entity(follower)
            .remove::<(CellCoord, FloatingOrigin, FrozenOrigin, ChildOf)>()
            .insert(Transform {
                translation: position,
                rotation: local.rotation,
                scale: Vec3::ONE,
            });
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
            let anchor = world
                .spawn((cell, transform, FloatingOrigin, ChildOf(grid_entity)))
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

/// Detaches `entity` from its current grid (if any -- a no-op otherwise), restoring a plain
/// parentless `Transform` that preserves its true position. Resolved through the grid's own
/// coordinate frame via [`Grid::grid_position`], not `GlobalTransform` (see
/// [`sync_grid_attachment`]'s doc comment for why). If `entity` was frozen, its anchor is
/// despawned first.
///
/// Used by [`sync_grid_attachment`]'s own detach case (inlined there, since that runs as a
/// regular system with deferred `Commands`) and, directly, by stage unload
/// (`ename_engine::stage::workflow`), which must rescue any `GridFollowCamera` descendant of a
/// container it is about to despawn -- despawning cascades to children immediately, so a
/// reactive system would react one frame too late.
pub fn detach_from_grid(world: &mut World, entity: Entity) {
    let Some(cell) = world.get::<CellCoord>(entity).copied() else {
        return;
    };
    let local = *world
        .get::<Transform>(entity)
        .expect("CellCoord requires Transform");
    let grid_entity = world
        .get::<ChildOf>(entity)
        .expect("a CellCoord entity is always a child of its Grid")
        .parent();
    let grid = world
        .get::<Grid>(grid_entity)
        .expect("a CellCoord's parent always has Grid")
        .clone();
    let position = grid.grid_position(&cell, &local);

    if let Some(FrozenOrigin(anchor)) = world.get::<FrozenOrigin>(entity).copied()
        && let Ok(anchor) = world.get_entity_mut(anchor)
    {
        anchor.despawn();
    }

    world
        .entity_mut(entity)
        .remove::<(CellCoord, FloatingOrigin, FrozenOrigin, ChildOf)>()
        .insert(Transform {
            translation: position,
            rotation: local.rotation,
            scale: Vec3::ONE,
        });
}
