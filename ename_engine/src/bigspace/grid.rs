//! Helper functions for working with big_space grids.

use bevy::{
    math::{Affine3A, DQuat, DVec3},
    prelude::*,
};

use super::{CellCoord, Grid};

#[derive(bevy::ecs::query::QueryData)]
pub struct GridQuery {
    pub entity: Entity,
    pub grid: &'static Grid,
}

pub fn on_grid(grid: &Grid, translation: DVec3) -> (CellCoord, Transform) {
    let (coord, offset) = grid.translation_to_grid(translation);
    let tr = Transform::from_translation(offset);
    (coord, tr)
}

pub fn on_grid_looking_at(
    grid: &Grid,
    translation: DVec3,
    target: DVec3,
    up: Vec3,
) -> (CellCoord, Transform) {
    let dir = target - translation;
    on_grid_looking_to(grid, translation, dir.as_vec3(), up)
}

pub fn on_grid_looking_to(
    grid: &Grid,
    translation: DVec3,
    direction: Vec3,
    up: Vec3,
) -> (CellCoord, Transform) {
    let (coord, pos_offset) = grid.translation_to_grid(translation);
    let tr = Transform::from_translation(pos_offset).looking_to(direction, up);
    (coord, tr)
}

/// Where an entity is, in metres, in the frame of the nearest grid at or above it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AbsolutePosition {
    /// The entity holding the `Grid` the position is expressed in.
    pub grid: Entity,
    pub position: DVec3,
}

/// Why an entity has no absolute position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PositionError {
    #[error("entity {0} does not exist")]
    NoEntity(Entity),
    #[error("entity {0} is not under a big_space grid, so it has no world position")]
    NotOnGrid(Entity),
}

/// An entity's frame relative to its grid: the absolute position of the grid child it hangs
/// from, plus the affine from the entity's local space to that child's origin, in grid axes.
struct GridFrame {
    grid: Entity,
    origin: DVec3,
    affine: Affine3A,
}

impl GridFrame {
    fn position(&self) -> DVec3 {
        self.origin + self.affine.translation.as_dvec3()
    }
}

/// The entity's position in metres, absolute, in the frame of the nearest grid at or above it.
///
/// Composes a non-grid parent chain (a camera parented to an airship) up to the grid's child that
/// holds the `CellCoord`. Stops at the first grid, which is correct while there is one root grid.
pub fn absolute_position(world: &World, entity: Entity) -> Result<AbsolutePosition, PositionError> {
    let frame = grid_frame(world, entity)?;
    Ok(AbsolutePosition {
        grid: frame.grid,
        position: frame.position(),
    })
}

/// Moves the entity to `position`, absolute metres in its grid's frame, and returns where it
/// ended up.
///
/// A direct child of a grid gets both halves written, `CellCoord` and `Transform::translation`,
/// because the translation alone is relative to whatever cell it occupies. An entity under a
/// non-grid parent gets a local translation through the inverse of the parent's absolute affine,
/// and never a `CellCoord`, which big_space reads only on a grid's children.
pub fn set_absolute_position(
    world: &mut World,
    entity: Entity,
    position: DVec3,
) -> Result<AbsolutePosition, PositionError> {
    if world.get_entity(entity).is_err() {
        return Err(PositionError::NoEntity(entity));
    }
    let parent = world.get::<ChildOf>(entity).map(ChildOf::parent);
    // The grid whose cells this entity occupies directly: its own, as before parents were
    // walked, or its parent's.
    let cell_grid = [Some(entity), parent]
        .into_iter()
        .flatten()
        .find_map(|e| world.get::<Grid>(e).map(|grid| (e, grid)));
    let (translation, cell, grid) = match (cell_grid, parent) {
        (Some((grid_entity, grid)), _) => {
            let (cell, offset) = grid.translation_to_grid(position);
            (offset, Some(cell), grid_entity)
        }
        (None, Some(parent)) => {
            let frame = grid_frame(world, parent)?;
            let local = frame
                .affine
                .inverse()
                .transform_point3((position - frame.origin).as_vec3());
            (local, None, frame.grid)
        }
        (None, None) => return Err(PositionError::NotOnGrid(entity)),
    };
    let mut entity_mut = world.entity_mut(entity);
    match entity_mut.get_mut::<Transform>() {
        Some(mut transform) => transform.translation = translation,
        None => {
            entity_mut.insert(Transform::from_translation(translation));
        }
    }
    if let Some(cell) = cell {
        entity_mut.insert(cell);
    }
    let position = absolute_position(world, entity)?.position;
    Ok(AbsolutePosition { grid, position })
}

/// The entity's absolute position and rotation in its grid's frame, over the same parent chain as
/// [`absolute_position`].
pub fn absolute_pose(world: &World, entity: Entity) -> Result<(DVec3, DQuat), PositionError> {
    let frame = grid_frame(world, entity)?;
    let (_, rotation, _) = frame.affine.to_scale_rotation_translation();
    Ok((frame.position(), rotation.as_dquat().normalize()))
}

/// Moves and turns the entity to an absolute pose in its grid's frame. Under a non-grid parent the
/// rotation is written local to the parent's absolute rotation, like the position in
/// [`set_absolute_position`].
pub fn set_absolute_pose(
    world: &mut World,
    entity: Entity,
    position: DVec3,
    rotation: DQuat,
) -> Result<(), PositionError> {
    set_absolute_position(world, entity, position)?;
    let parent = world.get::<ChildOf>(entity).map(ChildOf::parent);
    let parent_rotation = match parent {
        Some(parent)
            if world.get::<Grid>(entity).is_none() && world.get::<Grid>(parent).is_none() =>
        {
            let frame = grid_frame(world, parent)?;
            frame.affine.to_scale_rotation_translation().1
        }
        _ => Quat::IDENTITY,
    };
    let local = (parent_rotation.inverse() * rotation.as_quat()).normalize();
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        transform.rotation = local;
    }
    Ok(())
}

fn grid_frame(world: &World, entity: Entity) -> Result<GridFrame, PositionError> {
    if world.get_entity(entity).is_err() {
        return Err(PositionError::NoEntity(entity));
    }
    let transform_of = |e: Entity| world.get::<Transform>(e).copied().unwrap_or_default();
    // An entity holding a grid is positioned in its own grid's frame, as before this walked
    // parents.
    if let Some(grid) = world.get::<Grid>(entity) {
        let cell = world.get::<CellCoord>(entity).copied().unwrap_or_default();
        return Ok(GridFrame {
            grid: entity,
            origin: grid.grid_position_double(&cell, &transform_of(entity)),
            affine: Affine3A::IDENTITY,
        });
    }
    let mut affine = Affine3A::IDENTITY;
    let mut current = entity;
    loop {
        let Some(parent) = world.get::<ChildOf>(current).map(ChildOf::parent) else {
            return Err(PositionError::NotOnGrid(entity));
        };
        let transform = transform_of(current);
        if let Some(grid) = world.get::<Grid>(parent) {
            let cell = world.get::<CellCoord>(current).copied().unwrap_or_default();
            let rotation_scale = Affine3A::from_scale_rotation_translation(
                transform.scale,
                transform.rotation,
                Vec3::ZERO,
            );
            return Ok(GridFrame {
                grid: parent,
                origin: grid.grid_position_double(&cell, &transform),
                affine: rotation_scale * affine,
            });
        }
        affine = transform.compute_affine() * affine;
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_with_grid() -> (World, Entity) {
        let mut world = World::new();
        let grid = world.spawn(Grid::new(100.0, 0.0)).id();
        (world, grid)
    }

    fn assert_near(a: DVec3, b: DVec3) {
        assert!((a - b).length() < 1e-3, "{a} != {b}");
    }

    #[test]
    fn a_grid_child_round_trips_through_set_and_get() {
        let (mut world, grid) = world_with_grid();
        let entity = world.spawn((ChildOf(grid), Transform::default())).id();
        let target = DVec3::new(1234.5, -20.0, 777.25);
        let set = set_absolute_position(&mut world, entity, target).unwrap();
        assert_eq!(set.grid, grid);
        assert_near(set.position, target);
        assert_near(absolute_position(&world, entity).unwrap().position, target);
        assert_ne!(
            *world.get::<CellCoord>(entity).unwrap(),
            CellCoord::default()
        );
    }

    #[test]
    fn a_child_of_a_rotated_non_grid_parent_round_trips_without_a_cell() {
        let (mut world, grid) = world_with_grid();
        let parent = world
            .spawn((
                ChildOf(grid),
                CellCoord::new(3, 0, -2),
                Transform::from_xyz(10.0, 5.0, -7.0)
                    .with_rotation(Quat::from_rotation_y(0.7))
                    .with_scale(Vec3::splat(2.0)),
            ))
            .id();
        let child = world
            .spawn((ChildOf(parent), Transform::from_xyz(0.0, 30.0, 65.0)))
            .id();

        // The parent's rotated, scaled offset, on top of its cell and translation.
        let parent_origin = DVec3::new(310.0, 5.0, -207.0);
        let offset = (Quat::from_rotation_y(0.7) * Vec3::new(0.0, 60.0, 130.0)).as_dvec3();
        assert_near(
            absolute_position(&world, child).unwrap().position,
            parent_origin + offset,
        );

        let target = DVec3::new(250.0, 12.0, -190.0);
        let set = set_absolute_position(&mut world, child, target).unwrap();
        assert_eq!(set.grid, grid);
        assert_near(set.position, target);
        assert_near(absolute_position(&world, child).unwrap().position, target);
        assert!(world.get::<CellCoord>(child).is_none());
    }

    #[test]
    fn a_pose_round_trips_under_a_rotated_parent_outside_cell_zero() {
        let (mut world, grid) = world_with_grid();
        let parent = world
            .spawn((
                ChildOf(grid),
                CellCoord::new(-4, 1, 7),
                Transform::from_xyz(20.0, -3.0, 11.0).with_rotation(Quat::from_euler(
                    EulerRot::YXZ,
                    1.1,
                    0.3,
                    -0.2,
                )),
            ))
            .id();
        let child = world
            .spawn((ChildOf(parent), Transform::from_xyz(0.0, 30.0, 65.0)))
            .id();
        let position = DVec3::new(-380.0, 140.0, 690.5);
        let rotation = DQuat::from_euler(EulerRot::YXZ, -0.6, -0.25, 0.0);
        set_absolute_pose(&mut world, child, position, rotation).unwrap();
        let (read, read_rotation) = absolute_pose(&world, child).unwrap();
        assert_near(read, position);
        assert!(
            read_rotation.abs_diff_eq(rotation, 1e-5),
            "{read_rotation} != {rotation}"
        );
        assert!(world.get::<CellCoord>(child).is_none());

        // A direct grid child gets the rotation as is.
        let direct = world.spawn((ChildOf(grid), Transform::default())).id();
        set_absolute_pose(&mut world, direct, position, rotation).unwrap();
        let local = world.get::<Transform>(direct).unwrap().rotation;
        assert!(local.abs_diff_eq(rotation.as_quat(), 1e-6));
    }

    #[test]
    fn an_entity_off_the_grid_has_no_position() {
        let mut world = World::new();
        let loose = world.spawn(Transform::default()).id();
        assert_eq!(
            absolute_position(&world, loose),
            Err(PositionError::NotOnGrid(loose))
        );
        world.despawn(loose);
        assert_eq!(
            absolute_position(&world, loose),
            Err(PositionError::NoEntity(loose))
        );
    }
}
