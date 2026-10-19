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

A `GridFollowCamera` is a child of its grid, so a grid's despawn would take the camera with it.
Two observers on `Grid` rescue it instead, through one function: `On<Despawn, Grid>` and
`On<Remove, Grid>`. Each reads the still-present `Grid` to place the camera at its world pose,
takes it out of the grid's `Children`, and queues the detach (no `ChildOf` or `CellCoord`, a plain
`Transform`, a frozen anchor despawned).

The despawn observer is what makes every path safe. Bevy runs `Despawn` observers before any
`on_despawn` hook, and it is `Children::on_despawn` that queues the despawn of every child, so the
camera is out of the list before the cascade reads it. That covers a direct despawn, a cascade from
an ancestor (which reaches the grid through commands), stage unload and a `DynamicWorld` hot reload,
which despawns the instance and spawns it again. An `on_remove` hook or `Remove` observer alone is
too late: it runs after the cascade has been queued. On despawn the `Remove` observer fires too and
finds no followers left.

`sync_grid_attachment` reattaches the camera when a root grid appears. A camera detached earlier in
the same frame still has a floating-origin-relative `GlobalTransform`, so a parentless camera's
position is read from its `Transform`.
