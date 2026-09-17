//! Runtime switches for the debug gizmo overlays the engine's third-party plugins draw:
//! avian3d's physics wireframes and big_space's grid gizmos. Both plugins are always installed,
//! and both overlays start off; [`DebugOverlays`] is what turns them on.

use avian3d::debug_render::PhysicsGizmos;
use bevy::prelude::*;

/// Which debug gizmo overlays are drawn. Both off by default -- these are authoring aids, and a
/// shipping build that never flips them should look like the plugins were not there.
#[derive(Resource, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Resource, Default)]
pub struct DebugOverlays {
    /// avian3d: collider wireframes, body axes, joints, and the rest of `PhysicsGizmos`.
    pub physics: bool,
    /// big_space: per-grid axes and the bounds of the cell partitions near the floating origin.
    pub grids: bool,
}

/// Owns [`DebugOverlays`] and writes it into the gizmo configs the two plugins read.
pub struct DebugOverlayPlugin;

impl Plugin for DebugOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebugOverlays>()
            .register_type::<DebugOverlays>()
            // `PreUpdate`, so the `false` defaults land before the first frame's gizmos are
            // drawn in `PostUpdate`. `resource_changed` is also true the first time a just-added
            // resource is seen, which is what makes that first pass happen at all.
            .add_systems(
                PreUpdate,
                apply_debug_overlays.run_if(resource_changed::<DebugOverlays>),
            );
    }
}

/// Prefix of the gizmo config group big_space draws its grid axes through. The type itself is
/// private to that crate, so it can only be reached by reflected type path; matching the crate
/// rather than the struct name keeps this working if the struct is renamed.
const BIG_SPACE_GIZMO_GROUP_PREFIX: &str = "big_space::";

fn apply_debug_overlays(overlays: Res<DebugOverlays>, mut store: ResMut<GizmoConfigStore>) {
    store.config_mut::<PhysicsGizmos>().0.enabled = overlays.physics;

    // big_space splits its gizmos over two config groups: the grid axes go through a private
    // group of its own, and the cell partition bounds through the default group. Nothing else
    // draws through the default group -- the editor has `EditorGizmos` -- so taking it here is
    // what makes the bounds switchable at all.
    store.config_mut::<DefaultGizmoConfigGroup>().0.enabled = overlays.grids;
    let mut found_axes_group = false;
    for (_, config, ext) in store.iter_mut() {
        if ext
            .reflect_type_path()
            .starts_with(BIG_SPACE_GIZMO_GROUP_PREFIX)
        {
            config.enabled = overlays.grids;
            found_axes_group = true;
        }
    }
    if !found_axes_group {
        warn_once!(
            "no `{BIG_SPACE_GIZMO_GROUP_PREFIX}*` gizmo config group is registered; big_space's \
             grid axes cannot be switched off"
        );
    }
}
