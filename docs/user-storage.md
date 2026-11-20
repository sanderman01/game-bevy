# User storage

Part of the [design record](design.md). The crate is `ename_storage`; where it sits is in
[crate layout](design/crate-layout.md).

Saves, custom presets and anything else a player creates go through user storage. No game code
reads or writes user files directly. A consumer holds `UserStorage`, a handle to the one backend
the app installed, and never sees which backend that is.

## Records and keys

A record is a small set of named blobs (`meta`, `thumbnail`, `state`), addressed by a logical
key such as `saves/<session>/<save>` or `difficulty/<name>`. Keys are not paths; only the
filesystem backend maps them to files. The grammar is chosen so every backend can store any key:

| | Rule |
| --- | --- |
| Segment | 1 to 64 bytes of `a`-`z`, `0`-`9`, `-`, `_`; starts with a letter or digit |
| Key | 1 to 8 segments joined by `/`, at most 128 bytes |
| Key prefix | 0 to 8 whole segments; `saves` matches `saves/a/1`, never `saves-old/1` |
| Blob name | one segment of at most 32 bytes |
| Never | uppercase, `.`, spaces, non-ASCII, a Windows device name (`con`, `nul`, `com1`, ...) |

Lowercase only, so two keys never differ only by case on a case-insensitive filesystem. No `.`,
so `.` and `..` cannot occur and Windows never strips a trailing dot. A UUID is a valid segment.
A name the player types belongs in a blob, not in the key. Keys sort by segments.


## Operations

Every operation returns a `'static` future: it works on wasm and never blocks a frame.

| Call | Does |
| --- | --- |
| `list(prefix, Some(blob))` | Keys under the prefix in order, each with that blob |
| `read(key, blob)` | One blob, or `None` |
| `read_record(key, blobs)` | Several blobs of one record, all from the same commit |
| `begin()` | A `Transaction`: `write`, `delete_blob`, `delete` (whole record), then `commit` |
| `ready()` | Resolves once the backend has done what it can to be in sync |
| `status()` | `Syncing`, `Synced`, `Offline`, `Conflict`, `Volatile` or `Unavailable` |
| `idle()` | Resolves once every commit called before it has finished |

A save list is one `list` call with `meta`.

A transaction holds its changes until `commit`, which applies all of them or none, across
records. Later calls win for the same blob; `delete` then `write` starts the record afresh. A
record with no blobs left is removed. Dropping an uncommitted transaction discards it.

Commits apply in the order `commit` was called. A commit starts when `commit` is called and runs
whatever happens to its future, which only reports the outcome; so dropping the future, or the
task polling it, never loses a save. `StoragePlugin` waits up to 10 s for `idle()` when the app
exits, on desktop; a commit still running then is logged as an error. In the browser nothing can
block: the in-memory backend has nothing outstanding, and a browser backend flushes on its own.

Every operation waits for `ready()`. A save list or "continue" waits on it explicitly, and reads
`status()` to tell the player when saves are volatile, offline or in conflict.
`StorageError::is_storage_full()` tells a full disk or quota from other errors.

In a Bevy app, poll a future from a system with `StorageTask`:

```rust
let mut task = StorageTask::spawn(storage.list(&saves, Some(&META)));
// each frame:
if let Some(result) = task.poll() { /* ... */ }
```

## Backends

`EnginePlugins` adds `StoragePlugin::local` with the persistent data directory: the filesystem
backend with that directory as its root, or the in-memory backend where there is none (the
browser), so game code never branches. A target installs another backend with
`.set(StoragePlugin::with_backend(..))`.

A backend implements the object-safe `StorageBackend` trait in its own crate. Platform context,
such as the signed-in Steam or console user, is the backend's: the app constructs it with that
context, and no call or key carries a user.

The trait is `Send + Sync` on wasm too, because the handle is a Bevy resource. A browser backend
keeps its JS handles, which are `!Send`, in a `thread_local!` on the one thread wasm has, and is
itself a `Send + Sync` shell that reaches them from its futures; those need not be `Send` on
wasm.

## The filesystem backend

```
<persistent data>/        `~/.local/share/<project>/` on Linux
  lock                    held by the open store
  records/<key>/<blob>    one directory per record, `/` in the key written as `.`
  staging/<txn>/<i>/      new record versions being prepared
  journal.tmp, journal    the transaction being committed
  trash/<txn>-<i>/        old record versions on their way out
  load_order.toml, ...    the app's other files, which the store ignores
```

The store's files share the directory with the app's own, so those six names are reserved.

Records are siblings, so no record nests in another and a record directory is replaced whole.
Files that are not blobs (`.DS_Store`) are ignored, and a directory holding no blob is not a
record. A blob is written once and never changed, so a record's unchanged blobs are hard-linked
into its new version (copied where the filesystem has no hard links).

One worker thread per store opens it and runs the commits, one at a time in call order. Reads and
listings run on a blocking thread pool. Dropping the last handle waits for the queued commits,
then releases the store.

### Commit

Fault points (below) are in brackets.

1. Stage: for each changed record, build the new version in `staging/<txn>/<i>`. Flush every
   file and directory. *[staged]*
2. Write `journal.tmp` (the records to replace or remove) and flush it. *[journal_written]*
3. Rename it to `journal`: **the commit point**. Flush the root. *[committed]* If that flush
   fails, stop here: the commit is pending.
4. Under the write lock, so no reader in the process sees a record half swapped: for each
   record, move the old directory to `trash/` *[old_moved_aside]*, then move the staged one in
   *[new_moved_in]*. Release the lock.
5. Flush `records/` and `trash/` *[applied]*. Remove `journal`, flush the root
   *[journal_removed]*, then delete the trash and staging. Readers may see the new records
   before this; the next commit waits for it.

A record directory is never renamed onto an existing one, which Windows refuses; between the two
renames in step 4 the record is absent on disk, which the write lock hides here and the journal
finishes after a crash.

An error before step 3 removes the staging and commits nothing. An error from step 3's flush on
leaves the commit committed and pending: `commit` reports `Ok`, and the next read or commit runs
recovery first. Until that recovery succeeds, reads fail rather than show the old state.

### Recovery

On open, and for a pending commit:

1. Delete `journal.tmp` (never committed).
2. If `journal` exists: flush the root, so the journal is durable before any record moves; then
   steps 4 and 5 of the commit.
3. Empty `staging/` and `trash/`.

Each step is safe to repeat: a staged directory that still exists has not been moved in, so the
record beside it is the old version; one that is gone was moved in already. A crash during
recovery is recovered the same way. A journal that does not parse (it ends with a marker, so a
cut-off one never does) leaves the store untouched and `Unavailable`. The store is `Synced` once
recovery has run.

### The lock

`lock` is held with the OS's file lock (`flock`, `LockFileEx`) for as long as the store is open,
and a process-wide set of open roots (canonical paths) keeps a second handle in the same process
off it even where `flock` is emulated per process (NFS). A second process or handle gets
`Locked`, and the message says which. The store is then `Unavailable`, and the next operation or
`ready()` tries to open it again, so it works once the other one closes. The OS drops the lock
when the process ends, however it ends, so a lock file left behind is never stale. A filesystem
without file locks fails to open rather than open unguarded.

### What each platform guarantees

A crash of the process never leaves a commit half applied, on every platform. Power loss
depends on how the OS makes renames durable:

| | Files | Directories | After a power cut |
| --- | --- | --- | --- |
| Linux | `fsync` | `fsync` | the last commit is kept |
| macOS | `F_FULLFSYNC`, flushing the drive cache | the same | the last commit is kept |
| Windows | `FlushFileBuffers` | no flush exists | all or nothing; the last commit may be lost |

On Linux and macOS no record moves until the journal's rename is flushed, so after a power cut
either the journal is there and recovery finishes the commit, or none of its records moved.

Windows has no directory flush, so the journal's rename is not forced to disk. NTFS logs metadata
changes in order, so if a record rename survived a power cut, the journal rename before it did
too: all or nothing still holds, but the last commits may be lost.

A virus scanner, the search indexer or a backup tool may briefly hold a file open, which makes
renaming it or its directory fail; renames and removals retry for about a second on sharing and
access errors. Long paths are fine: Rust passes long absolute paths to Windows as `\\?\` paths.

## The in-memory backend

Records in a map behind one lock; its status is always `Volatile`, `ready()` resolves at once,
and each commit is done when `commit` returns. Used where there is no persistent path, and in
tests.

## Conformance suite

`ename_storage::conformance` checks any backend: reads, listing, transactions within and across
records, sync state, survival of a restart, readers that never see a torn record, commits in
call order, commits whose futures are dropped, and recovery. A backend's test implements
`conformance::Harness` (open, reset, whether it is persistent, which fault points it has) and
calls `run_all`.

A real crash cannot be tested, so a backend takes a `FaultSwitch` and calls `check(point)` as its
commit, or its recovery, passes each `FaultPoint`. The suite:

- stops the commit at each point and occurrence, reopens the store as a restart would, and
  expects the whole old state before the commit point and the whole new state after it;
- stops a commit past its commit point, then the recovery at each of its points, then recovers
  fully, and expects the new state;
- arms an io error instead at each point (`arm_io_error`): before the commit point the commit
  fails and changes nothing; after it the commit reports `Ok` and the next read sees it.

The filesystem backend implements every point; the in-memory one `staged`, the only one its
commit has.
