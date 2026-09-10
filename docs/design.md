# Design record

Decisions, their reasoning, and the questions still open. `docs/code-style.md` says how code is
written; this file says why the project is shaped the way it is. Record a decision here before
acting on it.

## Contents

- [Crate layout](design/crate-layout.md). Naming, the layer graph, where composition happens, and
  how a constraint or a third-party type crosses a crate boundary.
- [Agent tooling](design/agent-tooling.md). Why the MCP server is a second process, how entities
  and positions cross the boundary, and what the agent can see.
- [Asset aliases](assets-aliases.md). How an asset gets a name, and how the alias source resolves
  it.
- [Asset packages](assets-packages.md). Search paths, manifests, and load order.
- [Input is intent](#input-is-intent)
- [Bevy dependencies](#bevy-dependencies)
- [Assets](#assets)
- [Open questions](#open-questions)
- [Known gaps](#known-gaps)

## Input is intent

`ename_engine` owns the big_space glue and exposes `FlyCameraIntent`: forward, right, up, roll,
pitch, yaw, boost, with no `KeyCode` in it. Whichever layer owns the bindings writes the intent.
The editor writes it today; a spectator mode could write it later. Two crates each half-knowing one
key binding is what produced the W/E clash between the gizmo shortcuts and the fly camera.

The editor writes the intent in `FlyCameraSystems::Intent`. The engine applies and clears it in
`FlyCameraSystems::Apply`, before big_space's `camera_controller`, and switches big_space's own
bindings off there. With no editor linked in, nothing writes the intent and the fly camera does not
move, which is right for a shipping build. `fly_camera_active` in `ename_editor::camera` is the one
place the rule is written down. While the right mouse button is held the fly camera claims the
keyboard and the gizmo's W/E/R/X shortcuts stand down.

## Bevy dependencies

Depend on `bevy` and use its re-exports (`bevy::camera`, `bevy::window`, `bevy::reflect`,
`bevy::log`). A direct sub-crate pin puts two `bevy_reflect` versions in the graph the moment Bevy
bumps one, and the resulting type errors read as nonsense.

`dynamic_linking` and `dev` are features of the binary, never of a library. Only `ename` declares
them, behind its own `dev` feature, which its defaults turn on. So any workspace-wide command
resolves Bevy with `dynamic_linking` and `target/` holds one copy. Build a subset that leaves
`ename` out, `cargo check -p ename_engine` for instance, and Bevy resolves without it: a second copy
and a full rebuild each way. The `cargo db` / `dr` / `dc` / `dt` aliases in `.cargo/config.toml` are
the workspace-wide commands written out, so reaching for one avoids that.

`reflect_auto_register` is on workspace-wide already, through `bevy`'s defaults
(`default` -> `3d` -> `default_app`). Forbidding a library to turn it on is not enforceable without
`default-features = false`, which is not worth it today. Revisit if the engine crates are ever
consumed outside this workspace.

Compile-time log filtering (`log`'s `max_level_*` features) is not configured. Those features are
global and additive across the whole graph, so only the top-level binary may set them, and there is
no shipping build yet.

## Assets

Content assets should be referred to through aliases. An alias is a unique human readable identifier
and  pointer to an asset. Multiple packages can define the same alias. This allows content packages
to override and replace assets originally defined elsewhere.

See [asset aliases](assets-aliases.md) and [asset packages](assets-packages.md).

Depending on local development environment, `assets/` may be a dir or symlink outside the repository.
This is where all content assets live. Only test fixtures assets live in crates.

`.cargo/config.toml` points `BEVY_ASSET_ROOT` at the workspace root containing `assets/`,
but Cargo only applies `[env]` to `cargo run` and `cargo test`.
A binary aunched straight from `target/` falls back to Bevy's executable-directory heuristic and gets
 a different asset root. That is not a bug. That is by-design on the side of Bevy.

## Open questions

- **Scene format.** The editor mutates a live `World` and cannot persist an edit. `DynamicScene`
  and `.scn.ron` exist today, but the `Template` trait landing in `bevy_ecs` says upstream is
  heading somewhere better. Revisit when keeping an edit starts to matter.
- **Undo.** Every feature mutates the `World` directly, so retrofitting undo means rewriting every
  feature. `ename_remote` mutates the same `World` over BRP, so the design has to cover that path
  too. Decide before the editor grows past its handful of mutation sites.
- **Hot reload.** Shapes the whole layout. Fyrox-style hot reload requires `prefer-dynamic` and a
  game-as-plugin structure, a different crate graph from [crate layout](design/crate-layout.md).
- **`docs/bevy-best-practices.md`.** 575 lines written against Bevy 0.11-0.14 with an outdated-API
  table at the top. Background reading or dead weight, undecided.
- **Publishing.** `big_space` is a git dependency pinned to a rev, and crates.io rejects any crate
  with one. Versioning the engine crates means git tags, not crates.io, unless big_space gets a
  release or is vendored.

## Known gaps

- The editor has no save path and no command layer, so every panel mutates the `World` directly.
  Undo later means touching every mutation site; there are five today, the four `bevy_inspector`
  calls in `panels.rs` and the transform gizmo drag.
- `ename_engine::bigspace::grid` exposes `big_space::Grid` in its signatures, so every consumer is
  pinned to that git revision. Accepted deliberately; newtype it if the engine is ever consumed
  outside this workspace.
- The asset crates and `ename_game` have integration tests that run a headless `App`
  (`ename_asset_alias/tests/alias_reader.rs` and `alias_scan.rs`,
  `ename_asset_content/tests/alias_source.rs`,
  `ename_game/tests/scene_addressing.rs`), using `TaskPoolPlugin` plus `AssetPlugin` over
  committed fixture trees in each crate's own `tests/fixtures`, never the `assets/` symlink, which
  is not present on a fresh clone. The walk itself needs none of that, because it runs through a
  `Vfs` trait: `ename_asset_alias/tests/asset_discovery.rs` drives it over an in-memory `FakeVfs`
  and `ename_asset_package/tests/package_scan.rs` over `StdVfs` against a fixture tree, with no
  `App` and no Bevy in the graph either way. A `.meta` names its loader by Rust type path, and that
  path is scoped to the test binary that defines the type, so the one fixture carrying a `.meta`
  sits outside the tree `alias_scan.rs` walks. Nothing that needs a renderer or a window is covered: `ScenePlugin`,
  `ename_engine` and `ename_editor` still have no tests. A harness for those needs `MinimalPlugins`
  plus `BigSpaceDefaultPlugins`, because transform propagation comes from big_space and this
  project disables Bevy's `TransformPlugin`.
- `GameState::{Loading, Scene, Play}` all live in one `App` with the editor resident. The moment
  `Play` mutates the world, entering and leaving play will destroy authored state.
- `ename_editor` exposes no ordering point or selection access to the layer above it.
  `EditorSystems` and `UiState` are both `pub(crate)`, so `ename_game_editor` cannot order against
  selection or read what is selected until `ename_editor` widens one or both.
- Frame pacing for input lag (`bevy_framepace`) was a commented-out plugin line before the refactor
  and did not survive it. Evaluated, not adopted.
