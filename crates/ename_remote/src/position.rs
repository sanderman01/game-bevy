//! Absolute world position, over the floating origin.

use bevy::{
    ecs::hierarchy::ChildOf,
    math::DVec3,
    prelude::*,
    remote::{BrpError, BrpResult, builtin_methods::parse_some, error_codes},
};
use ename_engine::bigspace::{CellCoord, Grid};

use crate::entity_id::EntityId;
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
    entity: EntityId,
    position: [f64; 3],
}

#[derive(Serialize)]
pub(crate) struct GetResponse {
    entity: EntityId,
    position: [f64; 3],
    /// The entity holding the `Grid` the position is expressed in. Naming it is what lets a
    /// caller tell which frame a `Transform` and a `CellCoord` belong to.
    grid: EntityId,
}

pub(crate) fn get(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let GetParams { entity } = parse_some(params)?;
    let (grid_entity, grid) = grid_of(world, entity)?;
    let position = position_in(grid, world, entity);
    crate::to_value(GetResponse {
        entity: entity.into(),
        position: position.to_array(),
        grid: grid_entity.into(),
    })
}

/// Writes both halves of the position in one call. Setting `Transform::translation` alone
/// would place the entity relative to whatever cell it currently occupies, which is a
/// different point every time the floating origin moves.
pub(crate) fn set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let SetParams { entity, position } = parse_some(params)?;
    let position = DVec3::from(position);

    // Cloned so the immutable borrow of the world ends before the entity is written.
    let grid = grid_of(world, entity)?.1.clone();
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

    crate::to_value(PositionResponse {
        entity: entity.into(),
        position: position.to_array(),
    })
}

/// The entity's position in metres, absolute, in the frame of the grid it belongs to.
pub(crate) fn absolute_position(world: &World, entity: Entity) -> Result<DVec3, BrpError> {
    let (_, grid) = grid_of(world, entity)?;
    Ok(position_in(grid, world, entity))
}

/// The entity's position in metres within `grid`'s frame.
fn position_in(grid: &Grid, world: &World, entity: Entity) -> DVec3 {
    let cell = world.get::<CellCoord>(entity).copied().unwrap_or_default();
    let transform = world.get::<Transform>(entity).copied().unwrap_or_default();
    grid.grid_position_double(&cell, &transform)
}

/// The nearest `Grid` at or above `entity`, with the entity holding it.
///
/// Nested grids compose their frames; this walks to the first one and stops, which is correct
/// while the scene has a single root grid. Revisit when one is nested inside another.
fn grid_of(world: &World, entity: Entity) -> Result<(Entity, &Grid), BrpError> {
    if world.get_entity(entity).is_err() {
        return Err(BrpError::entity_not_found(entity));
    }
    let mut current = entity;
    loop {
        if let Some(grid) = world.get::<Grid>(current) {
            return Ok((current, grid));
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
