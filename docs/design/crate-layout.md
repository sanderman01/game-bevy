# Crate layout

Part of the [design record](../design.md).

The long-form analysis behind this is in `scratch/crate-structure-review.md`.

## Naming

Project codename is `ename`. Every crate is prefixed with it: `ename_engine`, `ename_content`,
`ename_editor`, `ename_game`, `ename_game_editor`, and the binary crate `ename`. Bare names like
`engine` and `editor` collide with anything and resolve independently once there is more than one
workspace.

## Layer graph

```
ename (bin)        -> ename_engine, ename_content, ename_game
                      + ename_editor, ename_game_editor   (feature `editor`)
                      + ename_remote                      (feature `agent`)
ename_game_editor  -> ename_editor, ename_game          (allowed; empty today, so bevy only)
ename_remote       -> ename_game, ename_engine
ename_game         -> ename_engine, ename_content
ename_editor       -> ename_engine
ename_content      -> serde, toml, thiserror, bevy      (no first-party deps)
ename_engine       -> bevy, avian3d, big_space          (no first-party deps)

ename_mcp (bin)    -> rmcp, reqwest, tokio, serde_json  (no first-party deps, no bevy)
```

`ename_mcp` hangs off the bottom with no arrow to anything because it is not a layer. It is a
separate process that talks to the game over a socket. See [agent tooling](agent-tooling.md).

Dependencies point down only. Nothing points sideways between `ename_content` and `ename_editor`,
and nothing points up. There is no `core`, `common`, or `shared` crate on the release path. If a
cycle appears, the fix is to move the shared type down a layer or invert the call into an event.

`ename_content` is a leaf, not a layer above the engine. Manifests, packages, an alias registry
and a load state machine are asset-layer concerns with nothing engine-shaped in them. Keeping it a
leaf keeps it cheap to link from a tool.

`ename_game_editor` is reserved and empty. Game features grow editor tooling, and an
`ename_editor -> ename_engine` graph has no slot for tooling that knows the game. The slot costs a
manifest; discovering it is missing costs a duplicated concept, which is how `EditorCamera` came
to exist.

`scripts/check-layers.sh` enforces this in CI. It is a build failure, not a habit. It reads
`cargo tree --no-default-features`, so it also proves that the editor crates and `ename_remote`
are absent from a shipping build.

`EditorCamera` existed because `engine` depended on `editor` and the editor could not name
`engine::camera::MainCamera`. It is deleted. The editor now keys off `MainCamera` and attaches
`TransformGizmoCamera` itself, which keeps the gizmo requirement in the crate that has the gizmo.

## Composition happens in the binary

Each crate exports a `Plugin` or a `PluginGroup`. The binary adds them. Which modules link into a
target is a property of the target. A shipping build does not contain the editor at all, rather
than containing it behind a runtime check.

`EnginePlugins` owns `DefaultPlugins` and exposes the window through `with_window`. It has to own
it. The engine must call `.disable::<TransformPlugin>()` because big_space supplies propagation,
and a `PluginGroup` cannot disable a plugin belonging to a different group. If the binary owned
`DefaultPlugins`, every target would have to remember that call, and forgetting it gives double
propagation with no compile error.

## Crossing crate boundaries

An ordering constraint that crosses a crate boundary is a public `SystemSet` exported by the lower
crate. `ename_engine::bigspace::GridSystems::Recentered` means "big_space has finished recentering
and transforms have propagated"; `ename_engine::input::FlyCameraSystems` orders intent production
against intent application.

A third-party type that crosses a boundary is re-exported by the crate that owns the dependency.
`ename_engine::bigspace` re-exports the `big_space` `Grid` and `CellCoord` the editor needs, so
`ename_editor` never names the git-pinned dependency itself. `ename_game` still depends on
`big_space` directly, for `spawn_big_space` and the floating-origin components. The cost this does
not avoid is that `bigspace::grid` exposes `big_space::Grid` in its signatures, so every consumer
of `ename_engine` is pinned to that git revision. That is accepted deliberately for now.
