//! Helper functions for working with big_space grids.

use bevy::{math::DVec3, prelude::*};
use big_space::commands::BigSpaceCommands;
use big_space::prelude::*;

#[derive(bevy::ecs::query::QueryData)]
pub struct GridQuery {
    pub entity: Entity,
    pub grid: &'static big_space::grid::Grid,
}

pub fn spawn_and_position_entity_on_grid(
    mut commands: Commands,
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
