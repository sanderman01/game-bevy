# Floating origin

Who holds big_space's `FloatingOrigin`, and why the engine decides it rather than the scene file.

## The constraint

big_space requires exactly one `FloatingOrigin` per root `BigSpace`. `BigSpace::find_floating_origin`
logs an error every frame when a space has none, and when a space has more than one it also clears
`BigSpace::floating_origin`, which stops high-precision propagation for that whole space. Two
origins is therefore strictly worse than none.

## The origin is runtime policy, not content

`FloatingOrigin` says "recenter the world around this entity right now". Which entity deserves that
depends on who is looking, and that differs per binary: `example_game_bin` wants the stage's own
camera, `example_game_editor_bin` wants `EditorCamera`. Both binaries load the same
`assets/examples/examples-stage/stage.scn.ron`, so the file cannot answer the question.

A stage therefore never stores `FloatingOrigin`. `DynamicWorldFormat::serialize` denies the
component outright, for the same reason it already denies `Children` and `VisibilityClass`: a saved
stage must not carry a decision that only the running app can make.

## Candidacy is content; election is engine

A stage says which entities are *eligible* to be the origin, by carrying
`ename_engine::bigspace::camera::FloatingOriginCandidate(i32)` — a priority, defaulting to `0`. The
example stage puts it on its `Main Camera`.

Candidacy is deliberately a separate component rather than something `MainCamera` requires. The
origin belongs wherever the content author wants it: a vehicle the camera is mounted to is as
reasonable a choice as the camera itself, and a `#[require]` on `MainCamera` would take that choice
away.

`ename_engine::bigspace::camera::elect_floating_origins` enforces the invariant. Each frame, for
every root `BigSpace`, it gives `FloatingOrigin` to the highest-priority grid-attached candidate
whose ancestor chain tops out at that root, and takes it from everyone else. Zero candidates under a
root means no origin, the same state a detached `GridFollowCamera` already produced. Ties break on
`Entity` so the outcome is deterministic.

This is the only place `FloatingOrigin` is inserted or removed. `GridFollowCamera` used to insert it
directly on attaching, which made "who is the origin" a rule with exactly one possible answer and no
room for a second camera; it now only requires `FloatingOriginCandidate(0)` and leaves the decision
to the election.

## The editor owns edit-versus-play

`EditorCamera` carries a priority above the default while the Scene View is shown, so it outranks
whatever the stage nominated. While the Game View is shown, the editor drops `EditorCamera`'s
priority below the default, so the stage's camera gets the origin, and with it whatever details the
world around the origin, such as terrain LOD. Switching back raises it again. A frozen origin's
anchor copied the camera's priority, so the editor ranks it along with the camera. Play-in-editor,
when it lands, can reuse the same switch.

That rule lives in `ename_editor` because the editor owns the mode. The engine knows only "highest
priority wins" and never learns what play-in-editor means — the same split that
[input is intent](../design.md#input-is-intent) settled for key bindings, for the same reason: a
rule half-known by two crates is the shape that produced the W/E clash.

## Freezing

`set_origin_frozen` spawns a stationary anchor so an editor camera can keep flying without the world
recentering under it. It takes `&mut World` and its effect is visible on return, so it keeps moving
`FloatingOrigin` to the anchor itself rather than waiting a frame for the election to agree.

Two things make the election agree instead of undoing it. The anchor is spawned as a candidate
carrying a copy of its camera's priority, so the election does not strip its origin; and the
election skips any camera that currently has `FrozenOrigin`, so it does not hand the origin back.
The second is not redundant with the first: the camera and its anchor hold equal priority, and
without the filter the tie would be settled by entity id.

This composes with the Game View: freeze the editor camera, show the Game View, and the game
camera outranks the frozen anchor, because the editor lowers the anchor's priority too.

## Ordering

The election runs in `PostUpdate`, immediately after `sync_grid_attachment` and before
`GridCameraSystems::Apply`. That placement already puts it ahead of `find_floating_origin`, which
big_space schedules inside `TransformSystems::Propagate`, after `camera_controller`, which
`GridCameraSystems::Apply` is ordered before.

`sync_grid_attachment` attaches through deferred `Commands`, so the election has to see those
commands applied or it reads the previous frame's `ChildOf`/`CellCoord` and a stage load costs a
frame of `find_floating_origin` errors. `.chain()` is enough: `auto_insert_apply_deferred` defaults
to true, so Bevy puts a sync point between two systems with an explicit order dependency when the
earlier one defers. No hand-written `ApplyDeferred`.

## When a grid goes away

A `GridFollowCamera` is a descendant of its grid, so a grid's despawn would take the camera with
it. Two observers on `Grid` rescue it instead, through one function: `On<Despawn, Grid>` and
`On<Remove, Grid>`. Each finds every follower below the grid, at any depth, places it at its pose
in the root grid's frame while every enclosing grid is still readable, takes it out of its
parent's `Children`, and queues the detach (no `ChildOf` or `CellCoord`, a plain `Transform`, a
frozen anchor despawned). Rescuing at any depth matters for a cascade: despawning a root reaches a
sub-grid only after the root is gone, too late to read the root's cell offset.

The despawn observer is what makes every path safe. Bevy runs `Despawn` observers before any
`on_despawn` hook, and it is `Children::on_despawn` that queues the despawn of every child, so the
camera is out of the list before the cascade reads it. That covers a direct despawn, a cascade from
an ancestor (which reaches the grid through commands), stage unload and a `DynamicWorld` hot reload,
which despawns the instance and spawns it again. An `on_remove` hook or `Remove` observer alone is
too late: it runs after the cascade has been queued. On despawn the `Remove` observer fires too and
finds no followers left.

`sync_grid_attachment` reattaches the camera when a root grid appears. A camera detached earlier in
the same frame still has a floating-origin-relative `GlobalTransform`, so the camera's position is
composed from local `Transform`s up its hierarchy the way big_space propagates: through every
enclosing grid's cell, with a grid contributing its rotation and translation but never its scale,
and stopping in the root grid's frame, since big_space ignores the root's own `Transform`. The
rescue places it the same way. A follower keeps its root-frame coordinates when it moves to another
root; the roots' `Transform`s only rank them, by distance from that position, and on a tie the
camera stays on its current grid.

Keeping root-frame coordinates is right for a hot reload, which respawns the same stage, and wrong
for opening a different one: stages do not share a frame. So the engine triggers `StageOpened`
when a stage is deliberately opened (the first spawn after `open_stage` or `open_stage_additive`,
or `new_stage`), never on a hot reload, and says whether it replaced every other stage. The editor
owns the policy: on an open that replaced the others it places `EditorCamera` at the new stage's
`MainCamera` (by `MainCameraFilter`, as the Game View picks it), in the root grid's frame through
`root_frame_pose` (absolute, parentless, for a stage without a grid), or at its default start pose
in the stage's root grid when there is none, then resets its motion and puts the orbit pivot the
default distance ahead. An additive open keeps the camera where it is.

`StageOpened` waits for the stage's nested loaded content (a `WorldAssetRoot` or
`DynamicWorldRoot` inside it), which spawns frames later and is marked `StageLoading` until it
does, so a Main Camera inside it is there when the event fires. A failed nested load, or a handle
with no load behind it, is not waited for.

An open does not reload a file the asset server already holds; it spawns that copy. Bevy's
`AssetServer::reload` cannot be tied to its own completion, so an open that reloaded could not tell
the cached spawn from the fresh one, and every attempt to do so traded one race for another. With
file watching on, as in the editor's dev build, the copy follows every edit anyway. What that costs:
an edit saved moments before the open, whose hot reload lands just after it, is announced from the
old copy (the camera starts at the old Main Camera pose) and then respawned fresh without an
announcement; and with watching off, a reopened edited file keeps the old copy until restart.

## Physics in a recentering world

avian works in floating-origin-relative coordinates: `Position` is copied from `GlobalTransform`
before each step (`transform_to_position`) and the engine writes it back into the grid-relative
`Transform` after (`physics::grid_writeback`), as an offset, so a body that big_space moved to
another cell keeps its cell. avian's own propagation before the step is off because it ignores
`CellCoord`; `physics::grid_propagate` replaces it, recomputing `GlobalTransform` from
`Grid::global_transform` for bodies and colliders directly under a grid. big_space only
propagates in `PostUpdate`, so without that a frame's second fixed tick would start every moving
body from last frame's pose. An origin shift lands in `PostUpdate` too, and the first tick after it
moves every `Position` into the new frame at once. A nested grid that moves within a frame is the
known gap: its bodies get last frame's grid pose until the next propagation.

Collider-to-body transforms (`ColliderTransform`) have the same problem twice over. avian seeds
them from `GlobalTransform`s on every `ColliderOf` insert, which is cell-less or stale for anything
new, and its propagation starts only at a hierarchy root with a `Transform`, which a stage's root
grid does not have. `physics::collider_grid_transform` propagates them itself before each step,
composing from local `Transform`s componentwise, as avian does, except at grids, where it uses
big_space's rule, the same one the follower camera is placed with
(`bigspace::grid::translation_in_parent`): contents are placed through their cell, and a grid's scale
applies to the grid entity but not to its contents. (The camera composes the levels between grids
as affines instead, as Bevy's propagation does, since it must land where it renders.) A vehicle
whose body is a grid, with sub-grids below it, so gets every collider's offset through every cell
on the way. It starts only from changed colliders, their marked ancestors (marks follow
reparenting and detaching) and colliders avian rewrote, so a tick
in which nothing moved costs a change-tick scan, and a value avian computes differently is corrected
in the same tick.
