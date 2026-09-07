//! Reporting an entity in the form a reader recognises.
//!
//! `Entity::to_bits` is documented as opaque, and it is: the index half is a `NonMax` held
//! complemented, so entity `4294966729` prints as `566v0` and no arithmetic outside Bevy should
//! be trusted to know that. A client that wants the id the editor's hierarchy panel shows has to
//! be told it rather than derive it, which is what this type carries.

use bevy::prelude::*;
use serde::Serialize;

/// An entity in both the forms that matter: the one a reader quotes, and the one BRP speaks.
#[derive(Clone, Serialize)]
pub(crate) struct EntityId {
    /// `{index}v{generation}`, as `Entity`'s `Display` writes it and the hierarchy panel shows
    /// it.
    id: String,
    /// `Entity::to_bits`. BRP's built-in methods take this and nothing else, so it travels
    /// alongside for any client that has to call them.
    bits: u64,
}

impl EntityId {
    /// The opaque bits, for the one thing they are good for: a deterministic ordering.
    pub(crate) fn bits(&self) -> u64 {
        self.bits
    }
}

impl From<Entity> for EntityId {
    fn from(entity: Entity) -> Self {
        Self {
            id: entity.to_string(),
            bits: entity.to_bits(),
        }
    }
}
