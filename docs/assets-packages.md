# Asset packages

Part of the [design record](design.md). See also [asset aliases](assets-aliases.md) for how one
asset gets a name.

## Search paths

Package search paths (`basegame`, `mods`) are game policy, not engine policy. `EnginePlugins`
defaults to none; `ename`'s `main` passes them in with `with_content_search_paths`.

`ename_asset_package` sits above the alias scan and adds the one thing it has no opinion about: an
order. It calls the scan once per package root and folds the results into load order.
`ename_asset_content` composes both crates: it adds `AliasSourcePlugin` alone, never
`AliasScanPlugin`, so a project that wants its own scan order can compose one instead of switching
a default off.

## Manifest and versions

A package version is a `semver` version. A `requires` entry is a `semver` requirement, written as
one string: `requires = ["core >= 2.0"]`. That's Cargo's syntax, which authors already know; a
syntax of our own would look almost the same while behaving differently in ways nobody would
notice until it broke.

A package id ends at the first whitespace, so an id containing whitespace has no single reading and
the manifest is rejected.

`manifest.toml` accepts unknown keys, unlike `.alias` and `_alias_rules.toml`. A mod is written by
a third party against whatever version of the game they had, and one carrying a key from a later
version must still load.

## Ordering constraints

`requires` is a dependency; `after` and `before` are not.

- An unsatisfied `requires` disables the package. It records which requirement failed and what
  version it found instead, and cascades to anything that required the disabled package, because
  loading a package whose dependency is missing is exactly what `requires` exists to prevent.
- An `after` or `before` naming a package that isn't installed is ignored, silently. It's an
  ordering hint about something that isn't there; warning about it would fire on every machine
  that lacks some popular optional package a mod happened to mention.

A cycle in `after`/`before` disables every package in it and reports each one by id. Panicking
would let one broken mod take down the whole session; breaking the cycle with a tiebreaker would
pick an order nobody asked for. A package that only depends on something in a cycle is disabled
too, under its own reason, so its author isn't sent looking at a cycle their package isn't part of.
`ename_check` (phase 4) turns the same problem into a non-zero exit, the hard error the spec asks
for.

## User overrides

`load_order.toml` overrules a manifest that contradicts it, and the overrule is recorded as a
problem. "Why did my mod load in that order" is the question the report exists to answer, so the
manifest's edge can't lose silently.

The file holds `[[constraint]]` entries with the same `after` and `before` an author writes, never
a full sequence, because a total order over the installed set goes stale the moment a package is
added.

It lives outside the asset tree. `ename_asset_package` reads whatever path it's handed;
`ename`'s `main` computes the default from `dirs::config_dir()`, because platform config policy
belongs to the target that owns platform policy. Most players never write the file, so a missing
one isn't a problem.

## Load order

Load order is Kahn's algorithm over those edges. The ready-set tiebreaker is the phase 2 order:
search path as the target listed it, then directory name. An unconstrained set comes out exactly as
it went in; the baseline settles every tie the constraints leave open.

Every alias two packages both claim produces a record saying why the winner won: which constraint
ordered them, and whether that ordering was direct, or `UNORDERED` plus the tiebreak that settled
it. `UNORDERED` is the line worth acting on: nothing relates those two packages, so the winner came
from an order neither author chose, and a `[[constraint]]` in the user's file is how to pin it
down.

## Error tolerance

Nothing fails a scan. An unreadable directory, an unparseable manifest, a `.alias` naming a file
that isn't there, an alias `AssetPath` would misread: each is recorded as a problem and skipped, so
one broken mod costs that mod and nothing else. A missing search path is different: a target may
list a `mods` directory a fresh install hasn't created, so that case is logged and passed over
without being anyone's fault.

`AliasScanPlugin` mirrors what it found into `Res<AliasScan>`. `ename_asset_content` mirrors its
own package-ordered version into `Res<ContentIndex>` and `Res<ContentReport>`, for the editor and
the log. Both are copies, for inspection; the reader resolves through the `OnceCell` it was built
with, because an `AssetReader` can't reach a resource.

## Further reading

The full design, including the `ename_xtask` tooling phase 4 adds -- `ename_check`, `ename_fix`,
`ename_list`, `ename_mv`, and the stubbed `ename_content_build` -- is in
`scratch/content-addressing-design.md`.
