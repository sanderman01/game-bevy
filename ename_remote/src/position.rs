//! Absolute world position, over the floating origin.
//!
//! The arithmetic is `ename_engine::bigspace::grid`'s; this module only speaks BRP.

use bevy::{
    math::DVec3,
    prelude::*,
    remote::{BrpError, BrpResult, builtin_methods::parse_some, error_codes},
};
use ename_engine::bigspace::grid::{self, PositionError};
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

#[derive(Serialize)]
pub(crate) struct GetResponse {
    entity: Entity,
    position: [f64; 3],
    /// The entity holding the `Grid` the position is expressed in. Naming it is what lets a
    /// caller tell which frame a `Transform` and a `CellCoord` belong to.
    grid: Entity,
}

pub(crate) fn get(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let GetParams { entity } = parse_some(params)?;
    let found = grid::absolute_position(world, entity).map_err(brp_error)?;
    crate::to_value(GetResponse {
        entity,
        position: found.position.to_array(),
        grid: found.grid,
    })
}

pub(crate) fn set(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let SetParams { entity, position } = parse_some(params)?;
    let placed =
        grid::set_absolute_position(world, entity, DVec3::from(position)).map_err(brp_error)?;
    crate::to_value(PositionResponse {
        entity,
        position: placed.position.to_array(),
    })
}

fn brp_error(err: PositionError) -> BrpError {
    match err {
        PositionError::NoEntity(entity) => BrpError::entity_not_found(entity),
        PositionError::NotOnGrid(_) => BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: err.to_string(),
            data: None,
        },
    }
}
