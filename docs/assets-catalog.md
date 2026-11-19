# Content catalog

Part of the [design record](design.md). See [asset aliases](assets-aliases.md) for how a file gets
an alias and [asset packages](assets-packages.md) for load order and overrides.

The catalog answers "which maps exist": every content descriptor of every registered kind, after
load order, `overrides` and `removes`.

## Kinds

A game registers a kind with the file extension that marks it:

```rust
app.register_content_kind::<Map>("map", "map.ron");
```

- The kind name is an opaque string to the engine.
- `Map` is the asset type the descriptor loads as; it needs a loader for the extension.
- Register before `Startup`, when the scan runs. A kind or extension registered twice panics.
- Packages list nothing. Nothing is added to `package-info.toml`.

A file is of a kind when its name ends in `.` plus the extension, matched like Bevy matches a
loader: case-sensitive, the longest registered extension first.

## Entries

An entry is `{ kind, id, package }`, keyed `(kind, id)`. `id` is the alias, so `alias://<id>`
loads it; `package` is the package whose file won the alias.

Entries come from the resolved alias index, never from a second walk of the files.
`ename_asset_package::build_index_with_catalog` builds the index and the catalog in one fold, with
the kind table passed in as plain data. The browser's baked discovery goes through the same fold,
so it gives the same entries.

| Case | Result |
| --- | --- |
| A later package claims the alias, named in `overrides` | Its file replaces the entry. |
| The same, not named in `overrides` | The same, plus `UndeclaredOverride`, as for any alias. |
| A `removes` names the alias | The entry is gone. No report line. |
| The winning file has another extension | The entry moves to the new kind, or out of the catalog; `ContentKindChanged`. |
| Two files of one package claim the alias, one of them content | `DuplicateContentAlias` instead of `DuplicateAlias`. |
| A descriptor fails to load | Left out; `UnloadableContent`. |

The alias rule cuts the stem at the first dot, so `fujix.map.ron` and `fujix.landscape.ron` are
both `<package>::fujix`. Give content files distinct stems.

API on `ContentCatalog`:

| Call | Returns |
| --- | --- |
| `entries(kind)` | The entries of one kind, sorted by id. |
| `get(kind, id)` | One entry. |
| `kinds()` | The registered kinds and their extensions. |
| `iter()` | Every entry, by kind then id. |

## Readiness

`ename_asset_content` loads the descriptor of every entry, by alias, as the type its kind was
registered with, and holds the handles. Once each has loaded or failed:

- `Res<ContentCatalog>` exists. Before that it does not.
- `ContentCatalogState` goes from `Loading` to `Ready`. The state exists where `StatesPlugin` is
  added, which `DefaultPlugins` does. It is set up in `Plugin::finish`, after every plugin has
  built, so plugin order does not matter; a test that calls `app.update()` itself must call
  `app.finish()` first, as `App::run` does.
- The `content_catalog` settle probe goes idle (`catalog_loading` until then).

The catalog holds package content only, fixed at startup. New files are seen after a restart, as
for the alias index.

## Unknown fields

A loader that reads RON leniently accepts fields it does not know and reports them:

1. `ron_from_bytes_lenient::<T>(bytes, &options)` returns the value and the path of each skipped
   field, such as `parts.0.colour`. `T` must not use `deny_unknown_fields`.
2. The loader takes a clone of `UnknownFieldSink` in its `FromWorld` and calls
   `record(asset_path, fields)` after every load, with an empty list too, so a fixed file clears
   its warning on reload.
3. The fields land in `ContentReport::unknown_fields`, per asset.

## Tooling

| Tool | Shows |
| --- | --- |
| `content.report` | `catalog` (`loading` or `ready`), each kind with its entries, `unknown_fields`. |
| `content.list { kind }` | The entries of one kind, with package and file. |
| `cargo xtask ename_check` | The catalog per kind; fails on any problem, the ones above included. |

`ename_xtask` links no Bevy, so it cannot see the game's `register_content_kind` calls. It reads
the kinds from the workspace's root `Cargo.toml`, which must list the same ones:

```toml
[workspace.metadata.ename.content_kinds]
map = "map.ron"
landscape = "landscape.ron"
```

Whether a descriptor loads is checked only by the game.
