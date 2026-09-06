//! A guard in front of `world.mutate_components`.
//!
//! Upstream's handler calls `World::entity_mut` without checking, so a mutation naming an
//! entity that has since been despawned takes down the whole game process rather than
//! returning an error (`bevy_remote` 0.19.1, `builtin_methods.rs:1194`). An agent holds entity
//! ids across calls and will eventually hold a stale one, so this is a matter of when.
//!
//! Registered under the built-in's own name, replacing it, so the hole is closed for every
//! client rather than only for the tools in `ename_mcp`.

use bevy::{
    prelude::*,
    remote::{
        BrpError, BrpResult,
        builtin_methods::{parse_some, process_remote_mutate_components_request},
    },
};
use serde::Deserialize;
use serde_json::Value;

pub const METHOD: &str = bevy::remote::builtin_methods::BRP_MUTATE_COMPONENTS_METHOD;

/// Only the field the guard needs. The handler parses the rest for itself.
#[derive(Deserialize)]
struct EntityOnly {
    entity: Entity,
}

pub(crate) fn mutate_components(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let EntityOnly { entity } = parse_some(params.clone())?;
    if world.get_entity(entity).is_err() {
        return Err(BrpError::entity_not_found(entity));
    }
    process_remote_mutate_components_request(In(params), world)
}
