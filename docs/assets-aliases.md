# Asset aliases

Part of the [design record](design.md). See also [asset packages](assets-packages.md) for how
packages order and override aliases.

## Addressing

Assets are addressed by alias, not by path: `alias://core::airship#Scene0`. A handle is keyed on
the alias, so which package supplied the file is invisible to game code and to anything
serialized.

The reader awaits the alias index inside `bevy_asset`, then delegates to the platform's default
reader. Nothing above the asset layer sequences content loading.

## Naming an alias

An alias comes from a file, never a list.

- `_alias_rules.toml` names a whole folder with one template, `alias = "core::{stem}"`. It
  inherits into subfolders; the nearest rule wins outright, it does not merge with rules above it.
- An `.alias` sidecar next to one asset (e.g. `airship.glb.alias`) overrides the folder rule with a
  hand-chosen name. It carries the guid tooling tracks the file by, and an `alias_origin` field
  saying whether a human chose the name or tooling derived it. That field decides whether tooling
  may rewrite the name later.

Claiming an alias another package already has is the override. There is no separate override list.

Every file in a package is a candidate asset, whether or not any folder carries a rule. A
directory with no `_alias_rules.toml` of its own, and none inherited from a parent, falls back to
`{package_id}::{stem}` -- the package's id standing in for the missing rule. This is a package
root's baseline, not a special case: it behaves exactly like a rule an ancestor directory could
have written, so a real `_alias_rules.toml` anywhere in the tree still replaces it outright, and an
`.alias` sidecar still wins over both. A package needs no `_alias_rules.toml` at all to have every
one of its files addressable.

Files that describe other files are never candidates: `_alias_rules.toml`, `*.alias`, `*.meta`,
`manifest.toml` in a package, and any file whose extension is in the ignored sidecar list. That
list defaults to `license`, so a REUSE `airship.glb.license` does not also claim
`core::airship`. Replace it with `with_ignored_sidecar_extensions` on `AssetContentPlugin` or
`AliasScanPlugin`; add exact file names with `with_ignored_file_names`.

The fallback needs a package id, so it applies only where a package supplies one --
`ename_asset_package`'s scan, and so `ename_fix`. A bare `_alias_rules.toml`-driven tree with no
packages at all (`AliasScanPlugin` on its own) keeps the old rule: no rule covering a file, no
alias, same as `ename_asset_alias` has always behaved with nothing else on top of it.

## Resolving without an extension

An alias carries no file extension. `AssetLoaders::find` normally picks a loader by extension, and
skips that lookup whenever the `AssetPath` has a label, so `alias://core::airship#Scene0` would
resolve no loader at all.

The reader's `read_meta` closes that gap: where the resolved file has no `.meta` of its own, it
answers with the default meta of whichever loader claims that file's extension. A real `.meta`
still wins, so an author keeps control of loader settings.

Loader settings that read the format from the path's extension are pinned to the resolved file's
format in that synthesized meta. `ImageLoader` is the one case: its default `FromExtension` would
panic on an alias, so the meta names the format instead, e.g. `Format(Png)` for a `.png`. Callers
that override settings with `load_builder().with_settings` start from that meta and need no format
hint of their own. A hand-written `.meta` is used as is, so one for an image read by alias must
name its format.

Shaders need a loader of their own. Bevy's `ShaderLoader` reads the shader language off the path's
extension, panics on an alias, and has no setting to pin it. For a `.wgsl` or `.spv` file the
synthesized meta names `AliasShaderLoader` instead, with `extension` set to the resolved file's.
`AliasSourcePlugin` registers that loader itself, so `ShaderRef::Path("alias://core::terrain".into())`
works with nothing else added. `#import "alias://..."` lines load by alias the same way.

Two limits. GLSL (`.vert`, `.frag`, `.comp`) is not handled and keeps Bevy's loader, so it does not
load by alias. And a hand-written `.meta` beside a shader that names Bevy's `ShaderLoader` wins
over the synthesized one, and still panics by alias; such a meta must name
`ename_asset_alias::shader::AliasShaderLoader` with an `extension` setting.

## Scanning

The scan reads the default asset source directly. It must never read through `alias://`: that
source awaits an index only the scan can fill, and would hang.

`ename_asset_alias` owns `.alias`, `_alias_rules.toml`, and the walk that reads them. The crate
that knows what an alias is is the crate that knows how one is named. Adding `AliasPlugins` scans
the asset root on startup and fills the index the `alias://` source waits on. A project with a
`_alias_rules.toml` and no packages at all can address its assets by alias with no registration
code and no other first-party crate.

Scanning walks through a `Vfs` trait, never `std::fs` or a Bevy type directly. The game supplies an
`AssetReader` implementation, so Android's APK works with no second code path. Wasm does not:
`HttpWasmAssetReader::read_directory` and `is_directory` log an error and return `Ok` anyway (an
empty stream, `false`) instead of failing loudly, so a directory walk over wasm finds nothing and
every alias fails with no explanation. Shipping to wasm needs a manifest of packages instead of a
directory walk, not a third `Vfs` implementation. `ename_xtask` supplies a `std::fs` implementation
with no Bevy in its graph, which is why both asset crates keep Bevy behind a default feature and CI
checks each builds without it. One walk over one trait keeps the tool and the game from drifting
apart.

## Live reload

Wherever Bevy watches the default source (the `file_watcher` feature, which `bevy/dev` turns on),
the `alias://` source watches too. Bevy reloads by path and source, and the default source's
watcher reports `basegame/core/sky.ron` while every handle was loaded as `alias://core::sky`, so
the alias source runs a second watcher over the asset root and forwards its events renamed to
the aliases that resolve to each file. A consumer sees the usual `AssetEvent::Modified`.

What reloads: content and `.meta` edits to a file that won its alias, including saves that write
a temp file and rename it over the target. A removed `.meta` reloads with the synthesized one. A
removed file keeps its last good asset, and so does a failed reload; the next good save replaces
it. Editing a file another package overrides does nothing.

What needs a restart: new or removed files, and edits to `.alias`, `_alias_rules.toml`,
`manifest.toml` or the load order file. The index is filled once.

Limits: processed mode does not reload aliased assets. A `.meta` saved by renaming a temp file
that does not itself end in `.meta` is dropped by Bevy's watcher before the alias source sees it.
Index paths outside the asset root never match. Shipping builds have no `file_watcher`, so they
watch nothing.

## Ownership: `.alias` vs `.meta`

We own `.alias`; Bevy owns `.meta`. Neither writes the other's. Bevy reconstructs a `.meta` from
`AssetMeta` through its own serializer and drops every field it doesn't recognize, so anything of
ours in there gets deleted the next time somebody else's tool runs. `.alias` uses an extension
nobody else claims, and its contents are TOML.

`.alias` and `_alias_rules.toml` reject unknown keys, because we generate them: a key we don't
recognize is a mistake.

## Registration order

The alias source has to be registered before `AssetPlugin` builds. `App::register_asset_source`
only fills `AssetSourceBuilders`; `AssetPlugin` turns that resource into live sources once, when it
builds. Registering afterward logs an error and leaves the source dead.

`EnginePlugins` owns `DefaultPlugins`, so it owns `AssetPlugin` too. It adds `AssetContentPlugin`
with `add_before::<AssetPlugin>`, making the order structural rather than a rule a target has to
remember. `AliasSourcePlugin::build` asserts on the order as well, for anyone adding it by hand.
