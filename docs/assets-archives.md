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
- Only a regular file whose name ends exactly in `.zip` or `.tar` mounts. `core.ZIP` is an
  ordinary file on every platform, and so is a directory named `x.zip`.
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
| `bevy` | `AssetReaderVfs`, `ArchiveSourcePlugin` with its reader, guarded writer and watcher, and `check_writable`. |
| `zip` | `.zip` mounts (Deflate through pure-Rust `miniz_oxide`). |
| `tar` | `.tar` mounts, parsed in the crate with no dependency. |

The mount code always compiles; with no format nothing ever mounts. That keeps the write guard
and problem reporting in the same crate as the formats, so they work whichever crate turned a
format on: a target can enable `ename_asset_vfs/zip` and add `ArchiveSourcePlugin` itself, before
`AssetContentPlugin`, and still get both. `ename_asset_content` and `ename_engine` forward
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
  watcher, and inserts the shared `Mounts` as the `ArchiveMounts` resource. It supports
  unprocessed mode only.
- The alias reader and both scans read through `server.get_source(AssetSourceId::Default)`, not a
  private platform reader, so whatever sits under the default source is what they see. The
  alias watcher still needs the physical root, so `AliasSourcePlugin::with_asset_root` remains,
  for the watcher only.

## Finding mounts

Mounts are found one loose directory at a time. The first question that passes through a
directory lists it once and records its subdirectories and archives; every later question about
anything in it is answered from memory. A loose tree costs one extra listing per directory and
nothing per file. Reading a file needs its ancestors listed, never its own directory.

The asset reader answers from memory on the async thread whenever it can: anything known to be
loose, and directory, listing and missing-`.meta` questions about an indexed archive. Only the
first listing of a directory, indexing an archive, and reading member bytes go to a blocking
thread.

## Reading

Only the index stays in memory, about 100 bytes per member. Members are read from the archive
file on demand.

- Zip: the index comes from the central directory alone, read through a buffer, including Zip64
  fields. A member's data start sits behind its local header, whose extra field often differs
  from the central one, so it is resolved on the first read and cached. Every read verifies the
  CRC32 and decoded length.
- Tar: parsed in the crate, one positioned 512-byte read per header, skipping the data. ustar and
  GNU headers, GNU long names and PAX `path` and `size` are understood. Tar has no payload
  checksum.
- A read is one positioned read of exactly the member's bytes (`read_exact_at` on Unix, a
  `seek_read` loop on Windows) on a blocking worker, then an inflate for Deflate members. Reads
  name their own offsets and never rely on a file cursor, so they don't serialise on one; only
  looking up the open file takes a short lock. Open archive files are kept in an LRU of 64.
- An archive is identified by length, modification time and (on Unix) inode when it is indexed. A
  file reopened after eviction that no longer matches fails with `InvalidData` rather than being
  read through a stale index.
- The archive is located from `FileAssetReader::get_base_path().join(asset_root)`, the same root
  Bevy's file reader uses, so this is desktop only.
- Store already-compressed formats uncompressed, e.g. `zip -n .png:.jpg:.ktx2`.

## Validation and limits

An archive is unavailable, and reported, when any member has an absolute path, a `..`
component, a `:` (a drive prefix) or a backslash; when two members share a path; when a file and a
directory collide; when zip members overlap (one member's data running into the next local
header, the overlapping-file zip bomb); for tar links, sparse files, global PAX headers and special
entries; and for encrypted zip members or a compression method other than Stored or Deflate.

Limits:

- Member count counts every record, directories and tar extension headers included. A zip's
  declared count and central directory size are read from its end record and checked before the
  `zip` crate parses or allocates anything for them; duplicate names are caught by comparing that
  count with the distinct names the crate keeps.
- Tar extension bodies (GNU long names, PAX) are capped at 64 KiB before they are read.
- Path length is capped at 4096 bytes and each name at 255.
- Member size (default 1 GiB) is checked against the declared size when indexing. Inflating
  reserves no more than 1032 times the compressed size and stops one byte past the declared size.

`.` components are dropped, since `tar -C dir .` writes `./a.txt`, and `__MACOSX/` is skipped.
Listings hide dot-files and `.meta` exactly as loose listings do.

## Conflicts and errors

- A real `core/` shadows `core.zip`, reported as a shadowed archive.
- `core.zip` together with `core.tar` makes the mount unavailable, and is reported.
- A file `core` beside `core.zip` wins, as the platform reader sees it; the archive does not mount
  and is reported.
- A case-only clash such as `core.zip` beside `Core/` is a warning, logged once when the directory
  is first listed.
- A corrupt, unreadable or unavailable archive answers with an IO error, never `NotFound`. Three
  places treat `NotFound` as normal and would otherwise make a mod vanish silently: a missing
  directory in the walk, a missing manifest, and a missing `.meta`. The mount still lists as a
  directory, so the scan reports the failure on that package and its siblings are unaffected.

Mount problems carry the logical mount path, the physical archive, and a detail that names the
member when one is at fault. Each is logged when recorded.

- `ename_asset_content` merges the problems found by the startup scan into the content report as
  `ProblemKind::Mount`, and `ename_check` fails on them.
- A problem found later, such as a member failing its CRC on a load, is only logged. Only bad
  bytes count; a read that fails with another IO error may be transient and is not recorded.

## Writes

Nothing writes into a mount.

- The default source's writer refuses any path inside a mount, a mounted archive file itself, and
  any path that is absolute or climbs above the asset root, on all its methods and both ends of a
  rename. Paths are normalized first, so `pkg/../pkg/x` is inside `pkg`. That covers Bevy writing
  a default `.meta`.
- `ename_fix` skips archive-backed assets and counts them.
- `ename_mv` refuses moves into or out of a mount.
- The editor's stage save calls `check_writable` (in `ename_asset_vfs`, re-exported by
  `ename_asset_content`) before serialising anything. It is a no-op without `ArchiveMounts`.

Filesystem paths are made relative to the physical root, with symlinks resolved, before they are
classified. A destination that doesn't exist yet but lies under a mount is refused.

## Hot reload

Watcher events for a mounted archive file are dropped with a "restart to apply" log line. The
check uses what is already known about the directory, so a directory merely named `x.zip` keeps
its events.

## Out of scope

Compressed tar, nested archives, archive hot reload, writing into archives, per-file overlay,
wasm and Android, the asset processor, and streaming large Stored members.
