# Design record

Decisions, their reasoning, and the questions still open. `docs/code-style.md` says how code is
written; this file says why the project is shaped the way it is. Record a decision here before
acting on it.

The long-form analysis behind the 2026-09 entries is in `scratch/crate-structure-review.md`.

## Naming

Project codename is `ename`. Every crate is prefixed with it: `ename_engine`, `ename_content`,
`ename_editor`, `ename_game`, `ename_game_editor`, and the binary crate `ename`. Bare names like
`engine` and `editor` collide with anything and resolve independently once there is more than one
workspace.

## Layer graph

```
ename (bin)        -> ename_game, ename_content, ename_editor, ename_game_editor, ename_remote
ename_game_editor  -> ename_editor, ename_game
ename_remote       -> ename_game, ename_engine
ename_game         -> ename_engine, ename_content
ename_editor       -> ename_engine
ename_content      -> serde, toml, thiserror, bevy      (no first-party deps)
ename_engine       -> bevy, avian3d, big_space          (no first-party deps)

ename_mcp (bin)    -> rmcp, reqwest, serde_json         (no first-party deps, no bevy)
```

`ename_mcp` is not in the graph above it because it is not in the graph at all: it is a separate
process that talks to the game over a socket. See "Agent tooling".

Dependencies point down only. Nothing points sideways between `ename_content` and `ename_editor`,
and nothing points up. There is no `core`, `common`, or `shared` crate on the release path: if a
cycle appears, the fix is to move the shared type down a layer or invert the call into an event.

`ename_content` is a leaf, not a layer above the engine. Manifests, packages, an alias registry
and a load state machine are asset-layer concerns with nothing engine-shaped in them, and keeping
it a leaf keeps it cheap to link from a tool. This corrects the graph previously stated in
`docs/code-style.md`.

`ename_game_editor` is reserved and empty. Game features grow editor tooling, and a game crate is
the one arrow an `ename_editor -> ename_engine` graph otherwise has no slot for. The slot costs a
manifest; discovering it is missing costs a duplicated concept, which is exactly how `EditorCamera`
came to exist.

`scripts/check-layers.sh` enforces this in CI. It is a build failure, not a habit.

`EditorCamera` existed because `engine` depended on `editor` and the editor could not name
`engine::camera::MainCamera`. It is deleted. The editor keys off `MainCamera` and attaches
`TransformGizmoCamera` to it itself, which keeps the gizmo requirement in the crate that has the
gizmo.

## Composition happens in the binary

Each crate exports a `Plugin` or a `PluginGroup`. The binary adds them. Which modules link into a
target is a property of the target, so a shipping build does not contain the editor rather than
containing it behind a runtime check.

`EnginePlugins` owns `DefaultPlugins` and exposes the window through `with_window`. It has to: the
engine must call `.disable::<TransformPlugin>()` because big_space supplies propagation, and a
`PluginGroup` cannot disable a plugin belonging to a different group. If the binary owned
`DefaultPlugins`, every target would have to remember that call, and forgetting it gives double
propagation with no compile error.

## Agent tooling

The full plan is in `scratch/mcp-server-plan.md`. The decisions it fixes:

**Two processes.** The game runs a `bevy_remote` BRP server; `ename_mcp` is a standalone binary
speaking MCP over stdio and forwarding to BRP over HTTP. Unreal puts its MCP server inside the
editor; this does not. The engine gains one optional dependency instead of an HTTP server, an MCP
implementation and an async runtime. The agent's client launches the sidecar itself, so there is
no port to allocate and no client config to regenerate. The catalogue survives a game crash, so
the agent can still read the logs after a panic. The cost is one hop and a reserialization per
call, which is irrelevant at this call volume.

**`ename_remote` is behind a non-default `agent` feature and must never ship.** BRP is
unauthenticated read and write access to the running world. Localhost is not a trust boundary:
any process running as the same user can connect. `scripts/check-layers.sh` asserts the crate is
absent from the binary's default-features-off dependency graph, which is the same mechanism that
keeps the editor out.

**Every tool takes a name or an id, and every result carries both.** An `Entity` is a
generation-and-index bit pattern that changes every run, so an id cannot be written into a plan
or quoted back to the user. Names can. Neither works alone: an entity need not have a `Name`, and
names are not unique -- the starting scene has three entities named `VirtualCamera`. An ambiguous
name is an error listing the candidates, never a silent pick of the first match.

**Positions cross the boundary as absolute double-precision metres.** Under big_space a position
is a `CellCoord` plus a `Transform` relative to a floating origin that moves with the camera. A
bare `Transform.translation` is a number that is correct for one frame in one cell. The conversion
happens engine-side in `game.position.get` / `.set`, not in the sidecar: the write has to set both
halves at once, and finding the entity's grid means walking its ancestors.

`ename_remote` sits above `ename_game` rather than above `ename_engine` alone, because
`game.run_state.get` reports `GameState`, which is a gameplay concept. It adds no arrow the layer
graph forbids.

### What the agent can actually see

Established against a running game on 2026-09-06, because everything in the catalogue assumes it.

Registered and readable: `Transform`, `Name`, `big_space::grid::Grid`, `big_space::grid::cell::CellCoord`,
`avian3d`'s `RigidBody`, `LinearVelocity`, `ComputedMass` and the rest of its component set, and
this project's own reflected types (`MainCamera`, `CameraDriver`, `VirtualCamera`,
`FlyCameraIntent`, and `ename_content`'s manifest types). `registry.schema` returns 1330 types and
775 KB, which is why `world_list_component_types` filters and caps.

Three gaps the tools have to live with:

- **`avian3d::collision::collider::parry::Collider` is not in the registry at all.** Reading it
  fails with "Unknown component type", so collider shapes are invisible to the agent.
  `ColliderConstructor`, `ColliderAabb` and `ColliderMassProperties` are registered, so a
  collider can be requested and its bounds and mass read. The shape itself cannot be.
- **Asset handles are registered but not serializable.** `Mesh3d`, `MeshMaterial3d` and anything
  else holding a `Handle` fail to read with a `ReflectSerialize` error. The agent sees that the
  component is present, never its value. Assets are edited as files, which is the design anyway.
- **`GameState` is not registered.** `game.run_state.get` reports it through `Debug`, not
  reflection. Registering it would let `world.get_resources` read it directly.

Registration is now load-bearing rather than a convenience for the inspector: an unregistered
component is one the agent cannot see, and it fails silently.

One more thing the first real agent session showed. Bevy stores resources, observers and
registered systems as entities, so an unfiltered listing of this world is 613 rows of which 17
are scene content. `game.entities.list` excludes those three markers by default and sorts named
entities first, because archetype iteration order is arbitrary and a `limit` applied to it
truncates a different arbitrary set every call.

## Crossing crate boundaries

An ordering constraint that crosses a crate boundary is a public `SystemSet` exported by the lower
crate. `ename_engine::bigspace::GridSystems::Recentered` means "big_space has finished recentering
and transforms have propagated"; `ename_engine::input::FlyCameraSystems` orders intent production
against intent application.

A third-party type that crosses a boundary is re-exported by the crate that owns the dependency.
`ename_engine` re-exports the `big_space` types the editor needs, so the git-pinned dependency
stays in one crate. Note the cost this does not avoid: `bigspace::grid` exposes `big_space::Grid`
in its signatures, so every consumer of `ename_engine` is pinned to that git revision. That is
accepted deliberately for now.

## Input is intent

`ename_engine` owns the big_space glue and exposes `FlyCameraIntent`: forward/right/up/roll/pitch/
yaw/boost, with no `KeyCode` in it. Whichever layer owns the bindings writes the intent — the
editor today, a spectator mode in the game later. Two crates each half-knowing one key binding is
what produced the W/E clash between the gizmo shortcuts and the fly camera.

The editor writes `FlyCameraIntent` in `FlyCameraSystems::Intent`; the engine applies and clears
it in `FlyCameraSystems::Apply`. With no editor linked in, nothing writes it and the fly camera
does not move, which is correct for a shipping build. `crate::camera::fly_camera_active` in the
editor is the one place the "the camera claims W and E while the right mouse button is held" rule
is written down.

## Bevy dependencies

Depend on `bevy` and use its re-exports (`bevy::camera`, `bevy::window`, `bevy::reflect`,
`bevy::log`). A direct sub-crate pin puts two `bevy_reflect` versions in the graph the moment Bevy
bumps one, and the resulting type errors read as nonsense.

`dynamic_linking` and `dev` are features of the binary, never of a library. Every cargo command
goes through the `cargo db` / `dr` / `dc` / `dt` aliases so the feature set never varies between
commands and `target/` holds one copy of Bevy.

`reflect_auto_register` is already enabled workspace-wide: it arrives through `bevy`'s default
features (`default` -> `3d` -> `default_app`). The rule that a library must not turn it on is
therefore not enforceable here without `default-features = false`, which is not worth it today.
Revisit if the engine crates are ever consumed outside this workspace.

Compile-time log filtering (`log`'s `max_level_*` features) is not configured. Those features are
global and additive across the whole dependency graph, so only the top-level binary may set them,
and there is no shipping build yet to set them for.

## Assets

`assets/` is a symlink outside the repository and `.cargo/config.toml` pins `BEVY_ASSET_ROOT` to
the workspace root. Cargo applies `[env]` to `cargo run` and `cargo test` only: a binary launched
directly from `target/` falls back to Bevy's executable-directory heuristic and gets a different
asset root. This is a live bug against the first real build, not just a concern for a future
repository split.

Package search paths (`basegame`, `mods`) are game policy. `ename_content` defaults to none and
the binary passes them in.

## Open questions

- **Scene format.** The editor mutates a live `World` and cannot persist an edit. Bevy's
  `DynamicScene` and `.scn.ron` exist today; the `Template` trait landing in `bevy_ecs` says
  upstream is heading somewhere better. Deliberately unresolved, and out of scope for the crate
  refactor. Revisit when keeping an edit starts to matter.
- **Undo.** Retrofitting undo onto an editor where every feature mutates the `World` directly means
  rewriting every feature. Decide before the editor grows past the handful of mutation sites it
  has.
- **Hot reload.** Shapes the whole layout: getting it Fyrox-style requires `prefer-dynamic` and a
  game-as-plugin structure, which is a different crate graph from the one above.
- **`docs/bevy-best-practices.md`.** 575 lines written against Bevy 0.11–0.14 with an outdated-API
  table at the top. Background reading or dead weight — undecided.
- **Publishing.** `big_space` is a git dependency pinned to a rev, and crates.io rejects any crate
  with one. Versioning the engine crates therefore means git tags, not crates.io, unless big_space
  gets a release or is vendored.

## Known gaps

- The editor has no save path and no command layer, so every panel mutates the `World` directly.
  Adding undo later means touching every mutation site; there are five today.
- `ename_engine::bigspace::grid` exposes `big_space::Grid` in its signatures, so every consumer is
  pinned to that git revision. Accepted deliberately; newtype it if the engine is ever consumed
  outside this workspace.
- There are no tests. The structural prerequisites now exist: the game is a library, every feature
  is a plugin that can be added to a headless `App`, and camera input goes through an injectable
  intent resource. A harness would need `MinimalPlugins` plus `BigSpaceDefaultPlugins`, because
  this project disables Bevy's `TransformPlugin`.
- `GameState::{Loading, Scene, Play}` all live in one `App` with the editor resident. The moment
  `Play` mutates the world, entering and leaving play will destroy authored state.
- `ename_editor` exposes no ordering point or selection access to the layer above it:
  `EditorSystems` and `UiState` are both `pub(crate)`. `ename_game_editor` cannot order against
  selection or read what is selected until `ename_editor` widens one or both.
- Frame pacing for input lag (`bevy_framepace`) was a commented-out plugin line before the
  refactor and was not carried forward. Evaluated, not adopted.
