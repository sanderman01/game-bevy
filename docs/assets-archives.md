# Asset archives

Part of the [design record](design.md). See also [asset aliases](assets-aliases.md) and
[asset packages](assets-packages.md), which read archives without knowing about them.

## What a mount is

A `.zip` or `.tar` file in the asset tree reads as a directory named without its extension.
`assets/basegame/core.zip` serves `basegame/core/abc.txt` exactly as a loose
`assets/basegame/core/abc.txt` would. Aliases, packages, folder rules, sidecars and `.meta` are
unaware of the difference.

- Formats: `.zip` (Stored and Deflate members) and plain `.tar`, each behind its own cargo
  feature (`zip`, `tar`). No `.tar.gz` or `.tar.zst`.
- A mount may sit at any depth. A `.zip` or `.tar` inside an archive is an ordinary file.
- Only the logical path is valid. `basegame/core.zip/abc.txt` is not a second spelling of
  `basegame/core/abc.txt`; two spellings would give two handles.
- Member paths map directly under the mount. A zip whose single top-level folder is `core/`
  yields `core/core/...`; that gets a hint in the log, not a problem.
- Archives are read-only and immutable while the game runs. Changing one needs a restart.
  Loose-file hot reload is unchanged.

## Crate and features

The archive code lives in `ename_asset_vfs`, a leaf crate that owns reading the asset tree as a
filesystem: the `Vfs` trait, `StdVfs`, `AssetReaderVfs`, and the mounts. Aliases and archives each
work without the other, and nobody compiles archive dependencies they don't use.

| Feature | Enables |
| --- | --- |
| `bevy` | `AssetReaderVfs`. With `zip` or `tar` also on: `ArchiveAssetReader`, the guarded writer and `ArchiveSourcePlugin`. |
| `zip` | `.zip` mounts (Deflate through pure-Rust `miniz_oxide`). |
| `tar` | `.tar` mounts. |

The mount code compiles when either format is on. `ename_asset_content` and `ename_engine` forward
`zip` and `tar`; the example binaries turn both on. `ename_xtask` always builds with both, so
`ename_check` and `ename_fix` understand archives. `ename_asset_alias` and `ename_asset_package`
never name a format.

## Where it hooks in

The archive layer sits below alias resolution, as a wrapper around the platform reader registered
as Bevy's `Default` source.

```text
plain path ──────────────────> Default source = ArchiveAssetReader ──> platform reader (loose)
alias://x ──> AliasReader ───> Default source                        └─> Mounts (members)
runtime scans ──> AssetReaderVfs ──> Default source
ename_xtask ──> ArchiveVfs ──> StdVfs + Mounts
```

- `ArchiveSourcePlugin` registers the `Default` source before `AssetPlugin`, whose
  `init_default_source` only fills an empty slot. It keeps the platform writer (guarded) and
  watcher, and inserts the shared `Mounts` as a resource. It supports unprocessed mode only.
- The alias reader and both scans read through `server.get_source(AssetSourceId::Default)`, not a
  private platform reader, so whatever sits under the default source is what they see. The
  alias watcher still needs the physical root, so `AliasSourcePlugin::with_asset_root` remains,
  for the watcher only.

## Reading

Only the index stays in memory, about 100 bytes per member. Members are read from the archive
file on demand.

- Zip: the index comes from the central directory alone, including Zip64 fields. A member's data
  start sits behind its local header, whose extra field often differs from the central one, so it
  is resolved on the first read and cached. Every read verifies the CRC32 and decoded length.
- Tar: the index is built with `entries_with_seek`, which reads each 512-byte header and seeks
  past the data. Only regular, continuous and directory entries are accepted. Tar has no payload
  checksum.
- A read is one positioned read of exactly the member's bytes (`read_exact_at` on Unix, a
  `seek_read` loop on Windows) on a blocking worker, then an inflate for Deflate members. There is
  no shared cursor and no lock. Open archive files are kept in an LRU of 64.
- The archive is located from `FileAssetReader::get_base_path().join(asset_root)`, the same root
  Bevy's file reader uses, so this is desktop only.
- Store already-compressed formats uncompressed, e.g. `zip -n .png:.jpg:.ktx2`.

## Validation and limits

An archive is unavailable, and reported, when any member has an absolute path, a `..`
component, a `:` (a drive prefix) or a backslash; when two members share a path; when a file and a
directory collide; for tar links, sparse and special entries; and for encrypted zip members or a
compression method other than Stored or Deflate. Limits on member count, path length and declared
member size are checked before allocating, and decoded bytes are capped while inflating.

`.` components are dropped, since `tar -C dir .` writes `./a.txt`, and `__MACOSX/` is skipped.
Listings hide dot-files and `.meta` exactly as loose listings do. The `zip` crate keeps only the
last of two members with the same name, so that duplicate is not detected in a zip; it is in a
tar.

## Conflicts and errors

- A real `core/` shadows `core.zip`, reported as a shadowed archive.
- `core.zip` together with `core.tar`, or a file `core` beside `core.zip`, makes the mount
  unavailable and is reported.
- A case-only clash such as `Core.zip` beside `core/` is a warning.
- A corrupt, unreadable or unavailable archive answers with an IO error, never `NotFound`. Three
  places treat `NotFound` as normal and would otherwise make a mod vanish silently: a missing
  directory in the walk, a missing manifest, and a missing `.meta`. The mount still lists as a
  directory, so the scan reports the failure on that package and its siblings are unaffected.

Mount problems carry the logical path, the physical archive and the member. Each is logged when
recorded. `ename_asset_content` merges them into the content report as `ProblemKind::Mount`, and
`ename_check` fails on them.

## Writes

Nothing writes into a mount.

- The default source's writer refuses any path inside a mount, on all its methods, both ends of
  a rename included. That covers Bevy writing a default `.meta`.
- `ename_fix` skips archive-backed assets and counts them.
- `ename_mv` refuses moves into or out of a mount.
- The editor's stage save calls a guard in `ename_asset_content`, compiled in always and a no-op
  without mounts, before serialising anything.

Filesystem paths are made relative to the physical root, with symlinks resolved, before they are
classified. A destination that doesn't exist yet but lies under a mount is refused.

## Hot reload

Watcher events for an archive file are dropped with a "restart to apply" log line.

## Out of scope

Compressed tar, nested archives, archive hot reload, writing into archives, per-file overlay,
wasm and Android, the asset processor, and streaming large Stored members.
