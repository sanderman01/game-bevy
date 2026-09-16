//! big_space integration: grid helpers, and the ordering point layers above the engine hang
//! grid-sensitive work on.

pub mod camera;
pub mod grid;

use bevy::{prelude::*, transform::TransformSystems};
use big_space::prelude::BigSpaceSystems;

pub use big_space::camera::BigSpaceCameraInput;
pub use big_space::plugin::BigSpaceDefaultPlugins;
pub use big_space::prelude::{BigSpaceCameraController, CellCoord, Grid, Grids};
pub use camera::{
    FloatingOriginCandidate, FrozenOrigin, GridCameraSystems, GridFollowCamera, detach_from_grid,
    set_origin_frozen,
};

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
        .add_plugins(camera::GridFollowPlugin);
    }
}
