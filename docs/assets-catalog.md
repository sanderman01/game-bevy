# Content catalog

Part of the [design record](design.md). See [asset aliases](assets-aliases.md) for how a file gets
an alias and [asset packages](assets-packages.md) for load order and overrides.

The catalog answers "which maps exist": every content descriptor of every registered kind, after
load order, `overrides` and `removes`.

## Kinds

A game registers a kind with the file extension that marks it:

```rust
app.init_asset::<Map>()
    .register_ron_content_kind::<Map>("map", "map.ron");
```

- `register_ron_content_kind` also registers `RonContentLoader<Map>`, which reads RON and reports
  unknown fields (below). `Map` needs `Deserialize`.
- `register_content_kind::<A>(kind, extension)` registers the kind only, for an asset type with a
  loader of its own.
- The kind name is an opaque string to the engine.
- Register before `Startup`, when the scan runs. A kind or extension registered twice panics.
- Packages list nothing. Nothing is added to `package-info.toml`.

A file is of a kind when its name ends in `.` plus the extension, matched like Bevy matches a
loader: case-sensitive, the longest registered extension first. A hidden file (`.map.ron`) is of
no kind: the alias stem ends at the first dot, so it has none.

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
| A `removes` names the alias | The entry is gone. No report line. A later claim starts over. |
| The final winner has another kind, or none, than the first claimant | The entry moves to the new kind, or out of the catalog; one `ContentKindChanged`. A change a later package reverts reports nothing. |
| Two files of one package claim the alias, one of them content | `DuplicateContentAlias` instead of `DuplicateAlias`. |
| The package is disabled | Its files are not in the index, so not in the catalog. |
| A descriptor fails to load at startup | Left out; `UnloadableContent`. |

The alias rule cuts the stem at the first dot, so `fujix.map.ron` and `fujix.landscape.ron` are
both `<package>::fujix`. Give content files distinct stems.

API on `ContentCatalog`:

| Call | Returns |
| --- | --- |
| `entries(kind)` | The entries of one kind, sorted by id. |
| `get(kind, id)` | One entry. |
| `kinds()` | The registered kinds and their extensions, sorted by kind. |
| `iter()` | Every entry, by kind then id. |

The resource has no way to change it in place. `without(failed)` consumes a catalog and returns
it without some entries; `ename_asset_content` uses it once, before publishing.

## Readiness

`ename_asset_content` loads the descriptor of every entry, by alias, as the type its kind was
registered with, and holds the handles. Once each has loaded or failed:

- `Res<ContentCatalog>` exists. Before that it does not.
- `ContentCatalogState` goes from `Loading` to `Ready`. The state exists where `StatesPlugin` is
  added, which `DefaultPlugins` does. It is set up in `Plugin::finish`, after every plugin has
  built, so plugin order does not matter; a test that calls `app.update()` itself must call
  `app.finish()` first, as `App::run` does.
- The `content_catalog` settle probe goes idle (`catalog_loading` until then).

Without `StatesPlugin` there is no state and a warning says so at startup when kinds are
registered; `Res<ContentCatalog>` appearing is then the only signal.

The catalog holds package content only, fixed at startup. New files are seen after a restart, as
for the alias index. Load problems are not fixed: after `Ready`, a descriptor that fails to reload
gains an `UnloadableContent` line, and loses it when it loads again. Its entry stays.

## Unknown fields

Content loaders accept fields they do not know and report them, so a file written for a newer
version still loads and a typo is visible. `T` must not use `deny_unknown_fields`.

| Way in | For |
| --- | --- |
| `register_ron_content_kind::<T>` | A kind whose descriptor is plain RON. Nothing else to do. |
| `read_ron_lenient(reader, load_context, &sink, &options)` | A loader of its own. Reads, parses and records in one call. The loader takes a clone of the `UnknownFieldSink` resource in its `FromWorld`. |
| `ron_from_bytes_lenient(bytes, &options)` | Code outside a loader, such as part parameters read at spawn. Returns the value and the skipped field paths; the caller reports them. |

The first two record on every load, an empty list when the read or parse fails, so a warning never
outlives the file that caused it. The fields land in `ContentReport::unknown_fields()`, per asset.

`serde_ignored` does not see unknown fields inside a `#[serde(flatten)]` struct or an untagged
enum: serde buffers those and drops what it does not use.

## Tooling

| Tool | Shows |
| --- | --- |
| `content.report` | `catalog` (`loading` or `ready`), each kind with its entries, `unknown_fields`. |
| `content.list { kind }` | The entries of one kind, with package and file. |
| `cargo xtask ename_check` | The catalog per kind; fails on any of the scan's problems, `DuplicateContentAlias` and `ContentKindChanged` included. |

`ename_xtask` links no Bevy, so it cannot see the game's registrations. It reads the kinds from the
workspace's root `Cargo.toml`:

```toml
[workspace.metadata.ename.content_kinds]
map = "map.ron"
landscape = "landscape.ron"
```

The two lists must not drift apart, so a game keeps a test that compares them. Order does not
matter:

```rust
#[test]
fn declared_content_kinds_match_the_registered_ones() {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()));
    // Add the game's plugins that register content kinds here.
    let manifest: toml::Table = include_str!("../../Cargo.toml").parse().unwrap();
    assert_eq!(
        app.registered_content_kinds(),
        ContentKinds::from_workspace_manifest(&manifest).unwrap()
    );
}
```

Whether a descriptor loads, and `UnloadableContent`, are checked only by the game.
