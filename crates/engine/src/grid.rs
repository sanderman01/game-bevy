//! Helper functions for working with big_space grids.

use bevy::{math::DVec3, prelude::*};
use big_space::commands::BigSpaceCommands;
use big_space::prelude::*;

#[derive(bevy::ecs::query::QueryData)]
pub struct GridQuery {
    pub entity: Entity,
    pub grid: &'static big_space::grid::Grid,
}

// TODO This is ugly. find a different way.
pub fn spawn_and_position_entity_on_grid(
    commands: &mut Commands,
    grid_entity: Entity,
    grid: Grid,
    translation: DVec3,
    spatial: impl FnOnce(&mut SpatialEntityCommands<'_>),
) {
    let (cell_coord, offset) = grid.translation_to_grid(translation);
    commands.grid(grid_entity, grid).with_spatial(|new_entity| {
        new_entity.insert(cell_coord);
        new_entity.insert(Transform {
            translation: offset,
            ..default()
        });
        spatial(new_entity);
    });
}

pub fn from_grid_translation(grid: &Grid, translation: DVec3) -> (CellCoord, Transform) {
    let (coord, offset) = grid.translation_to_grid(translation);
    let tr = Transform::from_translation(offset);
    (coord, tr)
}

pub fn from_grid_translation_looking_at(
    grid: &Grid,
    translation: DVec3,
    target: DVec3,
    up: Vec3,
) -> (CellCoord, Transform) {
    let dir = target - translation;
    from_grid_translation_looking_to(grid, translation, dir.as_vec3(), up)
}

pub fn from_grid_translation_looking_to(
    grid: &Grid,
    translation: DVec3,
    direction: Vec3,
    up: Vec3,
) -> (CellCoord, Transform) {
    let (coord, pos_offset) = grid.translation_to_grid(translation);
    let tr = Transform::from_translation(pos_offset).looking_to(direction, up);
    (coord, tr)
}
