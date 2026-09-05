//! Helper functions for working with big_space grids.

use bevy::{math::DVec3, prelude::*};

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
