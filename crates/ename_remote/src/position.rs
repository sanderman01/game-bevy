//! Absolute world position, over the floating origin.

use bevy::{
    ecs::hierarchy::ChildOf,
    math::DVec3,
    prelude::*,
    remote::{BrpError, BrpResult, builtin_methods::parse_some, error_codes},
};
use ename_engine::bigspace::{CellCoord, Grid};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `game.position.get`, `game.position.set`.
pub const GET_METHOD: &str = "game.position.get";
pub const SET_METHOD: &str = "game.position.set";

#[derive(Deserialize)]
pub(crate) struct GetParams {
    entity: Entity,
}

#[derive(Deserialize)]
pub(crate) struct SetParams {
    entity: Entity,
    /// Metres, absolute, in the root grid's frame.
    position: [f64; 3],
}

#[derive(Serialize)]
pub(crate) struct PositionResponse {
    entity: Entity,
    position: [f64; 3],
}

pub(crate) fn get(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let GetParams { entity } = parse_some(params)?;
    let position = absolute_position(world, entity)?;
    to_value(PositionResponse {
        entity,
        position: position.to_array(),
    })
}

/// Writes both halves of the position in one call. Setting `Transform::translation` alone
/// would place the entity relative to whatever cell it currently occupies, which is a
/// different point every time the floating origin moves.
pub(crate) fn set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let SetParams { entity, position } = parse_some(params)?;
    let position = DVec3::from(position);

    // Cloned so the immutable borrow of the world ends before the entity is written.
    let grid = grid_of(world, entity)?.clone();
    let (cell, offset) = grid.translation_to_grid(position);

    let mut entity_mut = world
        .get_entity_mut(entity)
        .map_err(|_| BrpError::entity_not_found(entity))?;
    match entity_mut.get_mut::<Transform>() {
        Some(mut transform) => transform.translation = offset,
        None => {
            entity_mut.insert(Transform::from_translation(offset));
        }
    }
    entity_mut.insert(cell);

    to_value(PositionResponse {
        entity,
        position: position.to_array(),
    })
}

/// The entity's position in metres, absolute, in the frame of the grid it belongs to.
pub(crate) fn absolute_position(world: &World, entity: Entity) -> Result<DVec3, BrpError> {
    let grid = grid_of(world, entity)?;
    let cell = world.get::<CellCoord>(entity).copied().unwrap_or_default();
    let transform = world.get::<Transform>(entity).copied().unwrap_or_default();
    Ok(grid.grid_position_double(&cell, &transform))
}

/// The nearest `Grid` at or above `entity`.
///
/// Nested grids compose their frames; this walks to the first one and stops, which is correct
/// while the scene has a single root grid. Revisit when one is nested inside another.
fn grid_of(world: &World, entity: Entity) -> Result<&Grid, BrpError> {
    if world.get_entity(entity).is_err() {
        return Err(BrpError::entity_not_found(entity));
    }
    let mut current = entity;
    loop {
        if let Some(grid) = world.get::<Grid>(current) {
            return Ok(grid);
        }
        match world.get::<ChildOf>(current) {
            Some(parent) => current = parent.parent(),
            None => {
                return Err(BrpError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!(
                        "entity {entity} is not under a big_space grid, so it has no world position"
                    ),
                    data: None,
                });
            }
        }
    }
}

pub(crate) fn to_value<T: Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value).map_err(|err| BrpError {
        code: error_codes::INTERNAL_ERROR,
        message: err.to_string(),
        data: None,
    })
}
