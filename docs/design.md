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
ename (bin)        -> ename_game, ename_content, ename_editor, ename_game_editor
ename_game_editor  -> ename_editor, ename_game
ename_game         -> ename_engine, ename_content
ename_editor       -> ename_engine
ename_content      -> serde, toml, thiserror, bevy      (no first-party deps)
ename_engine       -> bevy, avian3d, big_space          (no first-party deps)
```

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
