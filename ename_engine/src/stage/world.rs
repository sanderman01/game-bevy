//! World content that is not a stage: what a game spawns itself, such as a world built from
//! content assets instead of a stage file. It has no [`StageMember`](super::StageMember), so no
//! save can write it; these hooks keep it in step with stage opens instead.

use bevy::prelude::*;

use super::workflow::despawn_stage_members;

/// The world's content is being replaced: by [`unload_world`], which
/// [`open_stage`](super::open_stage) and [`new_stage`](super::new_stage) call first. Triggered
/// before any stage is unloaded; an observer despawns the content it owns. Its queued commands
/// are applied before the call goes on, so the old content is gone before the new content spawns.
/// Not triggered by [`open_stage_additive`](super::open_stage_additive).
///
/// Named in the present tense, unlike most events, because it fires before the unload.
///
/// An observer must only remove its own content. Unloading the world or opening a stage from it
/// logs an error and has no lasting effect: the outer unload removes whatever it opened.
#[derive(Event, Debug, Clone, Copy)]
pub struct WorldUnloading;

/// Game code put new world content in place after [`unload_world`]: the counterpart of
/// [`StageOpened`](super::StageOpened) with `replaced_others` for content that is not a stage.
/// The editor moves its camera to the Main Camera under `root`; without this event the editor
/// camera keeps its root-frame pose from the previous content.
///
/// Trigger it once the content is spawned, Main Camera included: unlike `StageOpened`, nothing
/// waits for nested loads first.
#[derive(Event, Debug, Clone, Copy)]
pub struct WorldLoaded {
    /// The content's top-level `BigSpace` entity.
    pub root: Entity,
}

/// Present while [`unload_world`] runs, so a call from a [`WorldUnloading`] observer fails
/// instead of recursing until the stack overflows.
#[derive(Resource)]
struct UnloadingWorld;

/// Unloads the world's content: triggers [`WorldUnloading`] and applies what its observers
/// queued, then unloads every open stage and every stage load in flight.
///
/// It despawns at once, so commands other systems queued for that content earlier in the same
/// schedule find it gone when they apply. Call it at the start of a frame (`First`) or between
/// schedules, or make such commands tolerant (`try_insert` and friends). The editor's File menu
/// calls [`open_stage`](super::open_stage) and [`new_stage`](super::new_stage) from the egui pass,
/// which runs in `PostUpdate`, so `PostUpdate` systems that queue commands on world content must
/// be tolerant.
///
/// Called from a [`WorldUnloading`] observer, it logs an error and does nothing.
pub fn unload_world(world: &mut World) {
    if world.contains_resource::<UnloadingWorld>() {
        error!(
            "the world was unloaded from a WorldUnloading observer (unload_world, open_stage or \
             new_stage); observers must only remove their own content, so this is ignored"
        );
        return;
    }
    world.insert_resource(UnloadingWorld);
    world.trigger(WorldUnloading);
    world.flush();
    despawn_stage_members(world, None);
    world.remove_resource::<UnloadingWorld>();
}
