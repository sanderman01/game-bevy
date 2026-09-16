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

`EditorCamera` carries a priority above the default, so in `example_game_editor_bin` it outranks
whatever the stage nominated and the stage's own camera simply does not get the origin. When
play-in-editor lands, the Game View becomes authoritative and the editor drops `EditorCamera`'s
priority below the default; leaving play raises it again.

That rule lives in `ename_editor` because the editor owns the mode. The engine knows only "highest
priority wins" and never learns what play-in-editor means — the same split that
[input is intent](../design.md#input-is-intent) settled for key bindings, for the same reason: a
rule half-known by two crates is the shape that produced the W/E clash.

## Freezing

`set_origin_frozen` spawns a stationary anchor so an editor camera can keep flying without the world
recentering under it. The anchor is a candidate too, carrying a copy of its camera's priority, and
the election skips any camera that currently has `FrozenOrigin`. No tie-break and no special case,
and it composes with play-in-editor on its own: freeze the editor camera, enter play, and the game
camera outranks the frozen anchor.

## Ordering

The election runs in `PostUpdate`, immediately after `sync_grid_attachment` and before
`GridCameraSystems::Apply`. That placement already puts it ahead of `find_floating_origin`, which
big_space schedules inside `TransformSystems::Propagate`, after `camera_controller`, which
`GridCameraSystems::Apply` is ordered before.

`sync_grid_attachment` attaches through deferred `Commands`, so an explicit `ApplyDeferred` sits
between the two. Without it the election reads the previous frame's `ChildOf`/`CellCoord` and a
stage load costs a frame of `find_floating_origin` errors.
