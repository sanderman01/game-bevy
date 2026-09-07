//! `ename_game_editor` -- editor tooling that knows about this game specifically.
//!
//! Deliberately empty. It exists so that game-specific tooling has somewhere to go that is not
//! `ename_editor`, which must stay game-agnostic, and not `ename_game`, which must not link the
//! editor. Every engine that ships an editor grows this arrow eventually; `EditorCamera` existed
//! because this project had no slot for it. See `docs/design/crate-layout.md`.

use bevy::{app::PluginGroupBuilder, prelude::*};

/// Game-specific editor tooling. Registers nothing yet.
pub struct GameEditorPlugins;

impl PluginGroup for GameEditorPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
    }
}
