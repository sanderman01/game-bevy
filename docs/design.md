# Design record

Decisions, their reasoning, and the questions still open. `docs/code-style.md` says how code is
written; this file says why the project is shaped the way it is. Record a decision here before
acting on it.

## Contents

- [Crate layout](design/crate-layout.md). Naming, the layer graph, where composition happens, and
  how a constraint or a third-party type crosses a crate boundary.
- [Agent tooling](design/agent-tooling.md). Why the MCP server is a second process, how entities
  and positions cross the boundary, and what the agent can see of the world.
- [Input is intent](#input-is-intent)
- [Bevy dependencies](#bevy-dependencies)
- [Assets](#assets)
- [Open questions](#open-questions)
- [Known gaps](#known-gaps)

## Input is intent

`ename_engine` owns the big_space glue and exposes `FlyCameraIntent`: forward, right, up, roll,
pitch, yaw, boost, with no `KeyCode` in it. Whichever layer owns the bindings writes the intent.
The editor writes it today; a spectator mode in the game could write it later. Two crates each
half-knowing one key binding is what produced the W/E clash between the gizmo shortcuts and the
fly camera.

The editor writes `FlyCameraIntent` in `FlyCameraSystems::Intent`. The engine applies and clears it
in `FlyCameraSystems::Apply`, before big_space's `camera_controller`, and switches big_space's own
bindings off there. With no editor linked in, nothing writes the intent and the fly camera does not
move, which is correct for a shipping build. `fly_camera_active` in `ename_editor::camera` is the
one place the rule is written down. While the right mouse button is held the fly camera claims the
keyboard, and the gizmo's W/E/R/X shortcuts stand down for it.

## Bevy dependencies

Depend on `bevy` and use its re-exports (`bevy::camera`, `bevy::window`, `bevy::reflect`,
`bevy::log`). A direct sub-crate pin puts two `bevy_reflect` versions in the graph the moment Bevy
bumps one, and the resulting type errors read as nonsense.

`dynamic_linking` and `dev` are features of the binary, never of a library. Only `ename` declares
them, behind its own `dev` feature, and `ename`'s defaults turn that feature on. So any command
covering the whole workspace resolves Bevy with `dynamic_linking` and `target/` holds one copy of
it. Build a subset that leaves `ename` out, `cargo check -p ename_engine` for instance, and Bevy
resolves without `dynamic_linking`, which is a second copy and a full rebuild each way. The
`cargo db` / `dr` / `dc` / `dt` aliases in `.cargo/config.toml` are the workspace-wide commands
written out, so reaching for one is how you avoid doing that by accident.

`reflect_auto_register` is already enabled workspace-wide. It arrives through `bevy`'s default
features (`default` -> `3d` -> `default_app`). The rule that a library must not turn it on is
therefore not enforceable here without `default-features = false`, which is not worth it today.
Revisit if the engine crates are ever consumed outside this workspace.

Compile-time log filtering (`log`'s `max_level_*` features) is not configured. Those features are
global and additive across the whole dependency graph, so only the top-level binary may set them,
and there is no shipping build yet to set them for.

## Assets

`assets/` is a symlink outside the repository and `.cargo/config.toml` pins `BEVY_ASSET_ROOT` to
the workspace root. Cargo applies `[env]` to `cargo run` and `cargo test` only. A binary launched
directly from `target/` falls back to Bevy's executable-directory heuristic and gets a different
asset root. That hits the first real build, so it is a live bug, not a concern for a future
repository split.

Package search paths (`basegame`, `mods`) are game policy. `ename_content` defaults to none and
`ename`'s `main` passes them in.

## Open questions

- **Scene format.** The editor mutates a live `World` and cannot persist an edit. Bevy's
  `DynamicScene` and `.scn.ron` exist today; the `Template` trait landing in `bevy_ecs` says
  upstream is heading somewhere better. Deliberately unresolved, and out of scope for the crate
  refactor. Revisit when keeping an edit starts to matter.
- **Undo.** Retrofitting undo onto an editor where every feature mutates the `World` directly means
  rewriting every feature. The editor is not the only writer any more. `ename_remote` mutates the
  same `World` over BRP, so an undo design has to cover that path too. Decide before the editor
  grows past the handful of mutation sites it has.
- **Hot reload.** Shapes the whole layout. Getting it Fyrox-style requires `prefer-dynamic` and a
  game-as-plugin structure, which is a different crate graph from the one in
  [crate layout](design/crate-layout.md).
- **`docs/bevy-best-practices.md`.** 575 lines written against Bevy 0.11–0.14 with an outdated-API
  table at the top. Background reading or dead weight, undecided.
- **Publishing.** `big_space` is a git dependency pinned to a rev, and crates.io rejects any crate
  with one. Versioning the engine crates therefore means git tags, not crates.io, unless big_space
  gets a release or is vendored.

## Known gaps

- The editor has no save path and no command layer, so every panel mutates the `World` directly.
  Adding undo later means touching every mutation site; there are five today, the four
  `bevy_inspector` calls in `panels.rs` and the transform gizmo drag.
- `ename_engine::bigspace::grid` exposes `big_space::Grid` in its signatures, so every consumer is
  pinned to that git revision. Accepted deliberately; newtype it if the engine is ever consumed
  outside this workspace.
- Nothing that runs an `App` is tested. `ename_mcp` and `ename_remote` have unit tests over pure
  helpers, and the engine, game and editor crates have none. The structural prerequisites now
  exist: the game is a library, every feature is a plugin that can be added to a headless `App`,
  and camera input goes through an injectable intent resource. A harness would need
  `MinimalPlugins` plus `BigSpaceDefaultPlugins`, because transform propagation comes from
  big_space and this project disables Bevy's `TransformPlugin`.
- `GameState::{Loading, Scene, Play}` all live in one `App` with the editor resident. The moment
  `Play` mutates the world, entering and leaving play will destroy authored state.
- `ename_editor` exposes no ordering point or selection access to the layer above it.
  `EditorSystems` and `UiState` are both `pub(crate)`, so `ename_game_editor` cannot order against
  selection or read what is selected until `ename_editor` widens one or both.
- Frame pacing for input lag (`bevy_framepace`) was a commented-out plugin line before the
  refactor, and the refactor did not carry it forward. Evaluated, not adopted.
