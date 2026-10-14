# Commands and jobs

Part of the [design record](../design.md). How an agent uses them is in
[../agent-tools.md](../agent-tools.md#commands-and-jobs). The decisions:

**A crate exposes functionality by registering a command, not by touching the side channel.**
`ename_engine::command` holds a registry of named commands, each with a description, a params
type, a result type and a scope. It is always compiled and has no network code. `ename_remote`
lists and calls whatever is registered through a fixed set of BRP methods, and `ename_mcp` turns
each command into an MCP tool at runtime. Before this, every feature added a module to
`ename_remote`, a hand-written tool to `ename_mcp`, and often a cargo feature to forward; the
sidecar resolved paths, polled, and wrote feature-specific hints. A later in-game console calls
the same registry.

Rejected:

- Each crate inserting its own methods into bevy_remote's `RemoteMethods`, as the editor used to.
  The sidecar still needs a tool per method (BRP has no per-method schema), every feature crate
  depends on a networking crate, and a console has no metadata to decide what a player may run.
- The registry in `ename_remote`. Engine features could not register without depending on a crate
  that must never ship.
- A separate crate. Every caller already depends on `ename_engine`.
- One BRP method per command. A fixed dispatch method avoids ordering against `RemotePlugin`.

**Params and results are serde types described by schemars.** The schema describes exactly the
JSON the handler parses (snake_case enums, `deny_unknown_fields`, one-key duration objects), doc
comments become descriptions, and rmcp generates the static tools' schemas the same way.
Reflection-based params were rejected: reflection's JSON shape differs from serde's, its schema
exporter lives in `bevy_remote`, and descriptions need the `documentation` feature on every type.

**Names are `namespace.action`, exactly one dot.** The MCP tool name replaces the dot with an
underscore, so `screenshot.capture` is `screenshot_capture` and `editor.set_view` is
`editor_set_view`, the names agents already used. Two names that would map to one tool name panic
at startup.

**A slow command starts a job.** Screenshots finish frames after they are asked for, recordings
run for minutes, and a BRP handler must answer in the frame it runs. Instead of a request, poll and
retention scheme per feature, a job command starts work on an entity and returns a job id. `Jobs`
records it as running, the feature finishes it, and the record is kept for five minutes. Removing
the work entity while the job runs cancels the job, so nothing can wait on a job whose work is
gone. Cancel either despawns the work or triggers `JobCancelRequested` and lets the feature
decide: a recording stops and keeps its file. Waiting is the client's business; the sidecar polls,
which costs nothing on loopback, rather than holding a BRP watching stream open.

**Every answer names the run that produced it.** A run id (eight letters, from the pid, the start
time and an OS-seeded hash) is part of every command and job answer, and job ids are
`<run>-<n>`. A call may name the run it expects and is refused if the game restarted, so a
catalogue fetched from one run is never silently applied to the next. A refused call is never
replayed automatically, because it may change the world.

**Errors carry a stable kind and a hint written by the crate that knows.** A camera that is not
rendering is `camera_inactive` wherever it comes from, and the editor, which knows how to make it
render, supplies "show the Game View first". The sidecar adds nothing but formatting.

**Relative paths resolve against the caller's working directory.** The sidecar sends its own,
because the agent's relative paths mean the agent's directory, and the game's working directory
is wherever it happened to be launched: under airshipgame the two differ.

**Capture sources are named and extensible.** `screenshot.capture` and `video.record` take a
`source` name. The engine registers `window` and `game_camera`, and the editor adds
`editor_camera`, so the editor needs no capture commands of its own.

**Scope is metadata the caller enforces.** Each command is `Safe` or `Development`; the BRP side
channel calls with `Development`, and a shipping console will call with `Safe`. Every command
migrated so far is `Development`, because each writes to an arbitrary path or drives the editor.
These commands are not the editor command layer that undo needs (see Known gaps in
[../design.md](../design.md#known-gaps)).
