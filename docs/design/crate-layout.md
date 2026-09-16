# Crate layout

Part of the [design record](../design.md).

The long-form analysis behind this is in `scratch/crate-structure-review.md`.

## Naming

Project codename is `ename`. Every engine-side crate is prefixed with it: `ename_engine`,
`ename_asset_alias`, `ename_asset_package`, `ename_asset_content`, `ename_editor`, `ename_remote`,
`ename_mcp`, `ename_xtask`. Bare names like `engine` and `editor` collide with anything and
resolve independently once there is more than one workspace.

`example_` crates are the exception: these are demo crates showing how a game project consumes
the engine crates, not part of the engine itself, so they take the `example_` prefix instead.
`example_game_lib` is the demo gameplay code. Two binaries compose it with the engine:
`example_game_bin` is the minimal case, engine plus game and nothing else; `example_game_editor_bin`
is the same composition with the editor and agent tooling linked in behind features. The workspace's
`members` glob lists both prefixes.

## Layer graph

```
example_game_bin (bin)        -> ename_engine, example_game_lib, dirs
example_game_editor_bin (bin) -> ename_engine, example_game_lib, dirs
                                 + ename_editor                      (feature `editor`)
                                 + ename_remote                      (feature `agent`)
ename_remote                  -> ename_engine
example_game_lib              -> ename_engine, ename_asset_alias
ename_editor                  -> ename_engine
ename_engine                  -> ename_asset_content, bevy, avian3d, big_space
ename_asset_content           -> ename_asset_alias, ename_asset_package, async-lock, bevy
ename_asset_package           -> ename_asset_alias, semver, serde, toml, thiserror, tracing
                                 + bevy, behind the default `bevy` feature
ename_asset_alias             -> async-lock, serde, toml, glob, uuid, thiserror   (no first-party deps)
                                 + bevy, behind the default `bevy` feature

ename_mcp (bin)               -> rmcp, reqwest, tokio, serde_json  (no first-party deps, no bevy)
ename_xtask (bin)             -> ename_asset_alias, ename_asset_package, dirs, futures-lite, toml, uuid
                                 (no bevy; no other first-party crate)
```

`ename_mcp` and `ename_xtask` hang off the bottom with no arrow to anything because they are not
layers. `ename_mcp` is a separate process that talks to the game over a socket. See
[agent tooling](agent-tooling.md). `ename_xtask` sits beside it as a second standalone tool, not a
layer: it reads the two bottom asset crates with `StdVfs` the same way the running game's scan
does, and `scripts/check-layers.sh` enforces that it names nothing above them.

Dependencies point down only. Nothing points sideways between the asset crates and `ename_editor`,
and nothing points up. There is no `core`, `common`, or `shared` crate on the release path. If a
cycle appears, the fix is to move the shared type down a layer or invert the call into an event.

The asset layer is three crates, not one. `ename_asset_alias` is the leaf. It owns the alias
end to end: what one is, the `.alias` sidecars and `_alias_rules.toml` folder rules that name one, the
walk that finds them, the validation that rejects one `AssetPath` would misread, and the `alias://`
source that serves it. Adding `AliasPlugins` is the whole setup, which is what lets the crate be
published and used on its own.

`ename_asset_package` sits on top and adds the one thing the walk has no opinion about: an order.
It finds packages on disk, parses their manifests, calls `scan_aliases` once per package root, and
puts the results in load order. It also folds that ordered scan into one index, because the index,
the contested aliases and the `removes` are three views of one walk over the ordered packages and
splitting them would be two implementations of load order that drift. `ename_xtask` needs all
three with no Bevy in its graph, which is the other reason the fold sits here. It is the only
first-party crate the alias leaf's consumers gain, and the arrow points down, so the two never grow
a second walk between them. `ename_asset_content` is above both and owns the `App` wiring, because
deciding what goes in an `App` is composition rather than asset logic.

Scanning is `AliasScanPlugin`, separate from `AliasSourcePlugin`, rather than a flag on one plugin.
`ename_asset_content` needs the source but supplies its own package-ordered index, and with two
plugins it composes -- it adds the source and never adds the scan -- instead of switching a default
off. A negative flag ages into a second code path; an unadded plugin does not.

Both crates' Bevy dependency is optional and on by default. `ename_xtask` in phase 4 links them
with `default-features = false` and scans the same tree through `StdVfs`, so the command line tool
and the running game share one implementation of the walk rather than growing two that drift. CI
proves each bevy-free build stays buildable; nothing else in the workspace would, because every
other consumer turns the feature on. The workspace dependency entry for `ename_asset_alias` sets
`default-features = false` for the same reason, so every consumer that wants the Bevy half asks for
it by name.

`ename_engine` depends on `ename_asset_content`. That is new, and it is the one place a lower
crate's constraint reaches upward. `App::register_asset_source` fills a resource that `AssetPlugin`
turns into live sources exactly once, when it builds, so the alias source has to be registered
before then. `EnginePlugins` already owns `DefaultPlugins` for the same class of reason it owns
`.disable::<TransformPlugin>()`, so it owns this too, and `add_before::<AssetPlugin>` makes the
ordering structural rather than a rule in `main.rs` that every future target has to remember.

`scripts/check-layers.sh` enforces this in CI. It is a build failure, not a habit. It reads
`cargo tree --no-default-features`, so it also proves that the editor crate and `ename_remote`
are absent from a shipping build.

An earlier `EditorCamera` was a historical example of the cost of getting a layer wrong. It lived
in the engine because `engine` depended on `editor` and the editor could not name
`engine::camera::MainCamera`. That one was deleted. `ename_editor::camera::EditorCamera` now
exists again, but as a different thing living in the right crate: the editor's own
always-present scene-view camera, distinct from `ename_engine::camera::MainCamera` (whatever
camera a loaded stage defines for the Game View, which may not exist at all). The gizmo keys off
this `EditorCamera` and attaches `TransformGizmoCamera` itself, which keeps the gizmo requirement
in the crate that has the gizmo.

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
and transforms have propagated"; `ename_engine::bigspace::GridCameraSystems::Apply` orders a
grid-attached camera's input-writing system before big_space's own `camera_controller` consumes
it, so a layer above the engine never has to name that big_space system itself.

A third-party type that crosses a boundary is re-exported by the crate that owns the dependency.
`ename_engine::bigspace` re-exports the `big_space` `Grid` and `CellCoord` the editor needs, so
`ename_editor` never names the git-pinned dependency itself. `example_game_lib` still depends on
`big_space` directly, for `spawn_big_space` and the floating-origin components. The cost this does
not avoid is that `bigspace::grid` exposes `big_space::Grid` in its signatures, so every consumer
of `ename_engine` is pinned to that git revision. That is accepted deliberately for now.

The captured log is a shared type that moved down. `ename_engine::log` owns the ring buffer and
the `tracing` layer that fills it, and `EnginePlugins` installs both. Two crates above it read
that buffer: the editor's Console panel and `ename_remote`'s `game.logs.get`. Neither can reach
the other, because `ename_editor -> ename_remote` is a sideways edge the layer check rejects, and
`LogPlugin::custom_layer` is a single `fn` slot that two readers cannot each claim. So capture is
always on, shipping build included, sized by `EnginePlugins::with_log_capacity` and 3000 entries
by default. `EnginePlugins::with_log_layer` and `ename_remote::capture_layer` are gone.

Owning the buffer means `EnginePlugins` also owns the subscriber's verbosity. An `EnvFilter` added
to a subscriber gates every layer on it, so the buffer can only be more verbose than stderr if the
subscriber-wide filter is the more permissive one and each consumer narrows itself back down. So
`EnginePlugins` opens it to `log::MAX_LEVEL` (trace) and sets both `fmt_layer` and `custom_layer`:
stderr gets a fixed `log::DEFAULT_LEVEL` (info) filter, and the buffer gets a reloadable one behind
the `log::CaptureLevel` resource. `RUST_LOG` still overrides both, as it did before.

`CaptureLevel` is what the editor's level checkboxes drive, which puts an editor concern in the
bottom crate. The alternative is capturing at trace always, and that is worse than the coupling:
per-layer filters take part in callsite interest, so a level nobody has ticked costs a filter check
instead of a formatted message, a `String` and a push. Measured on this game, always-on trace was a
few hundred allocations a second from the physics solver alone and filled the 3000-entry ring in
about fifteen seconds. The resource is a plain level, not a UI type, so the coupling stays one way.
