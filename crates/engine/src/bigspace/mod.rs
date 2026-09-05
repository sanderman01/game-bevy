//! big_space integration: grid helpers, and the ordering point layers above the engine hang
//! grid-sensitive work on.

pub mod grid;

use bevy::{prelude::*, transform::TransformSystems};
use big_space::prelude::BigSpaceSystems;

/// Grid types re-exported so layers above the engine can name them without taking the
/// `big_space` git dependency themselves.
pub use big_space::prelude::{CellCoord, Grid};

/// Ordering points for work that depends on big_space having finished with an entity.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GridSystems {
    /// Runs after big_space has moved entities into their new cells and transforms have
    /// propagated. A system correcting a value that went stale across a cell shift belongs
    /// here: the shift has happened and the corrected value is read next frame.
    Recentered,
}

/// Configures [`GridSystems`]. big_space's own plugins are added by
/// [`EnginePlugins`](crate::EnginePlugins); this plugin only publishes the ordering.
pub struct BigSpacePlugin;

impl Plugin for BigSpacePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            PostUpdate,
            GridSystems::Recentered
                .after(BigSpaceSystems::RecenterLargeTransforms)
                .after(TransformSystems::Propagate),
        )
        // Moves to `editor` in Phase 4, once the editor can depend on the engine. It is
        // editor-only behaviour and nothing in a shipping build needs it.
        .add_systems(
            PostUpdate,
            crate::gizmo_compat::reanchor_gizmo_drag_across_cells.in_set(GridSystems::Recentered),
        );
    }
}
