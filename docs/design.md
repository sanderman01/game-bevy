# Design record

Decisions, their reasoning, and the questions still open. `docs/code-style.md` says how code is
written; this file says why the project is shaped the way it is. Record a decision here before
acting on it.

## Contents

- [Crate layout](design/crate-layout.md). Naming, the layer graph, where composition happens, and
  how a constraint or a third-party type crosses a crate boundary.
- [Agent tooling](design/agent-tooling.md). Why the MCP server is a second process, how entities
  and positions cross the boundary, and what the agent can see.
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

`assets/` is a symlink outside the repository and `.cargo/config.toml` pins `BEVY_ASSET_ROOT` to the
workspace root. Cargo applies `[env]` to `cargo run` and `cargo test` only. A binary launched
directly from `target/` falls back to Bevy's executable-directory heuristic and gets a different
asset root. That hits the first real build, so it is a live bug.

Package search paths (`basegame`, `mods`) are game policy. `EnginePlugins` defaults to none and
`ename`'s `main` passes them in with `with_content_search_paths`.

Assets are addressed by alias through a custom asset source: `alias://core::airship#Scene0`. A
handle is keyed on the alias, not on the file it resolved to, so which package won an override is
invisible to game code and to anything serialized. The reader awaits the index inside `bevy_asset`
and delegates to the platform default reader, which is why nothing above the asset layer sequences
content loading any more.

An alias carries no file extension, and `AssetLoaders::find` skips the by-asset-type loader lookup
whenever an `AssetPath` has a label, so `alias://core::airship#Scene0` would resolve no loader at
all. The reader closes that in `read_meta`: where the resolved file has no `.meta` of its own, it
answers with the default meta of whichever loader claims that file's extension. A real `.meta`
still wins, so an author keeps control of loader settings.

The scan reads the default asset source directly and must never read through `alias://`. It would
await an index only the scan can fill, and hang.

An asset's alias comes from a file, never from a list. `_alias_rules.toml` names a whole folder with one
template -- `alias = "core::{stem}"` -- and inherits into the folders under it, with the nearest
rule winning outright rather than merging. An `airship.glb.alias` sidecar beside one asset overrides
that with a hand-chosen name, carries the guid tooling tracks the file by, and says with
`alias_origin` whether a human chose the name or tooling derived it, which is what decides whether
tooling may ever rewrite it. Claiming an alias another package already has *is* the override; there
is no override list anywhere, which is what removed the quoted TOML keys whose typos were silent.

Both files, and the walk that reads them, belong to `ename_asset_alias` -- the crate that knows
what an alias is is the crate that knows how one is named. That is what lets the crate stand alone:
adding `AliasPlugins` scans the asset root on startup and fills the index the `alias://` source
waits on, so a project with a `_alias_rules.toml` and no packages at all addresses its assets by alias
with no registration code and no other first-party crate. `ename_asset_package` adds the one thing
the walk has no opinion about -- an order -- by calling it once per package root and folding the
results in load order. Scanning is a second plugin rather than a flag on the first, so a project
that decides its own order composes rather than opts out: `ename_asset_content` adds
`AliasSourcePlugin` alone and never adds `AliasScanPlugin`, and there is nothing for it to switch
off.

We own `.alias` and Bevy owns `.meta`, and neither writes the other's. Bevy reconstructs a `.meta`
from `AssetMeta` through its own serializer and drops every field it does not recognise, so
anything of ours in there is one run of somebody else's tool away from being deleted. `.alias` is
an extension nobody else claims; its contents are TOML.

`.alias` and `_alias_rules.toml` reject unknown keys, because we generate them and a key we do not know
is a mistake. `manifest.toml` does not: a mod is written by a third party against whatever version
of the game they had, and one carrying a key from a later version must still load.

Nothing fails a scan. An unreadable directory, an unparseable manifest, a `.alias` naming a file
that is not there, an alias `AssetPath` would misread: each is recorded as a problem and skipped,
so one broken mod costs that mod and nothing else. A missing search path is not one of those: a
target may list a `mods` directory a fresh install has not created, so that case is logged and
passed over without being anybody's fault. `AliasScanPlugin` mirrors what it found into
`Res<AliasScan>`; `ename_asset_content` mirrors its own package-ordered version into
`Res<ContentIndex>` and `Res<ContentReport>` for the editor and the log. Those are copies, for
inspection -- the reader resolves through the `OnceCell` it was built with, because an
`AssetReader` cannot reach a resource.

The scanner reads through a `Vfs` trait rather than through `std::fs` or a Bevy type. The game
supplies an `AssetReader` implementation, so Android's APK works with no second code path; wasm
does not -- `HttpWasmAssetReader::read_directory` and `is_directory` log an error and return `Ok`
anyway (an empty stream, `false`) rather than failing loudly, so a directory walk over wasm finds
nothing and every alias fails with nothing explaining why. Shipping to wasm will need a manifest
of packages instead of a directory walk, not a third `Vfs` impl. `ename_xtask` will supply a
`std::fs` one with no Bevy in its graph at all, which is why both asset crates' Bevy dependency
sits behind a default feature and CI checks each builds without it. One walk over one trait is what
stops the tool and the game from drifting.

The source has to be registered before `AssetPlugin` builds. `App::register_asset_source` only
fills `AssetSourceBuilders`, and `AssetPlugin` turns that resource into live sources once, when it
builds; registering afterwards logs an error and leaves the source dead. `EnginePlugins` owns
`DefaultPlugins` and therefore owns `AssetPlugin`, so it adds `AssetContentPlugin` with
`add_before::<AssetPlugin>` and the ordering is structural rather than a rule a target has to
remember. `AliasSourcePlugin::build` asserts on it as well, for anyone adding it by hand.

A package version is a `semver` version and a `requires` entry is a `semver` requirement, written
as one string: `requires = ["core >= 2.0"]`. That is Cargo's dialect, which every author already
knows, and a syntax of our own would look exactly like it while behaving subtly differently. The id
ends at the first whitespace, so a package id containing whitespace has no single reading and is
rejected at the manifest.

`requires` is a dependency; `after` and `before` are not. An unsatisfied `requires` disables the
package, says which requirement failed and what version it found instead, and cascades to whatever
required it, because loading a package whose dependency is absent is the situation `requires`
exists to prevent. An `after` or `before` naming a package that is not installed is ignored in
silence: it is an ordering hint about something that is not there, and warning would fire on every
machine that lacks the popular optional package a mod happened to mention.

A cycle in `after`/`before` disables its members and reports every id in it. Panicking would let
one broken mod cost the whole session, and breaking the cycle with a tiebreaker would pick an order
nobody asked for. A package that only depends on a cycle is disabled too, under a reason of its own
rather than as a member. Telling an author their package sits in a loop it is not in sends them to
the wrong file. `xtask content check` in phase 4 is where the same problem becomes a non-zero exit,
which is the hard error the spec asked for.

The user's `load_order.toml` overrules a manifest that contradicts it, and the overrule is recorded
as a problem. "Why did my mod load in that order" is the question the report exists to answer, so
the manifest's edge cannot lose silently. The file holds `[[constraint]]` entries carrying the same
`after` and `before` an author writes, and no sequence, because a total order over the installed
set goes stale the moment a package is added. It lives outside the asset tree, so
`ename_asset_package` reads whatever path it is handed and `ename`'s `main` computes the default
from `dirs::config_dir()`. Platform config policy belongs to the target that owns platform policy.
Most players never write the file at all, so a missing one is not a problem.

Load order is Kahn's algorithm over those edges, with the phase 2 order -- search path as the
target listed them, then directory name -- as the ready-set tiebreaker. An unconstrained set comes
out exactly as it went in, and the baseline settles every tie the constraints leave open.

Every alias two packages both claim produces a record saying why the winner won: which constraint
ordered them and whether it did so directly, or `UNORDERED` and the tiebreak that settled it.
`UNORDERED` is the line worth acting on. Nothing relates those two packages, so the winner came
from an order neither author chose, and a `[[constraint]]` in the user's file is how to pin it.

The full design, including the `xtask` tooling that phase 4 adds, is in
`scratch/content-addressing-design.md`.

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
  (`crates/ename_asset_alias/tests/alias_reader.rs` and `alias_scan.rs`,
  `crates/ename_asset_content/tests/alias_source.rs`,
  `crates/ename_game/tests/scene_addressing.rs`), using `TaskPoolPlugin` plus `AssetPlugin` over
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
