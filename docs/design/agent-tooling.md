# Agent tooling

Part of the [design record](../design.md). How to use the tools is in
[../agent-tools.md](../agent-tools.md); the full plan is in `scratch/mcp-server-plan.md`. The
decisions:

**Two processes.** The game runs a `bevy_remote` BRP server; `ename_mcp` is a standalone binary
speaking MCP over stdio and forwarding to BRP over HTTP. Unreal puts its MCP server inside the
editor; this does not. The game binary gains one optional dependency instead of linking an MCP
implementation and its async runtime. The agent's client launches the sidecar, so there is no port
to allocate and no client config to regenerate. The sidecar survives a game crash: a dead game is
one tool error rather than a dead MCP server, and after a panic the agent can still read the logs.
The cost is one hop and a reserialization per call, irrelevant at this call volume.

**`ename_remote` must never ship.** BRP is unauthenticated read and write access to the running
world, and any process running as the same user can connect. Localhost is not a trust boundary.
Only `example_game_editor_bin`, the development binary, links it; `example_game_bin` is the
shipping composition and never names it. `scripts/check-layers.sh` asserts the crate is absent
from that binary's graph, the same mechanism that keeps the editor out.

**Every tool result carries a run id, named `pid`.** The sidecar holds no connection
and no cached state, so a call after the game restarts just works. That is right, and it is also the
trap: nothing otherwise tells the agent the world it is editing is not the one it planned against.
Tagging every response needs no extra call and no memory of having asked. It is deliberately not
part of `time_get`, because a signal only visible on request is one the agent will not think to
look for. It is eight letters: short enough to ride on every response, long enough that two runs essentially
never share one, which matters now that job ids and refused calls depend on it. A command or job
result carries the run from the same answer; only the world tools still read it with a separate
call.

**Every tool that names an entity takes a name or an id, and every result carries both.** Neither
works alone. An `Entity` is a generation-and-index bit pattern that changes every run, so an id
cannot be written into a plan or quoted back to the user. Names can, but an entity need not have a
`Name` and names are not unique: the starting stage has three entities named `VirtualCamera`. An
ambiguous name is an error listing the candidates, never a silent pick of the first match.

**An entity id crosses the boundary as the raw `Entity::to_bits` u64.** A readable `606v0` form,
index and generation written out, was built and reverted. An `Entity` nested inside a component
value serialises as a bare integer with nothing in the JSON marking it as one, so keeping the
readable form honest meant a type-registry-driven rewrite of every entity leaf on both the read and
the write path. The accepted cost: an id no longer matches what the editor's hierarchy panel shows
beside the same entity.

**Positions cross the boundary as absolute double-precision metres.** Under big_space a position is
a `CellCoord` plus a `Transform` relative to a floating origin that moves with the camera, so a bare
`Transform.translation` is correct for one frame in one cell. The conversion happens engine-side in
`game.position.get` / `.set`, not in the sidecar, because the write has to set both halves at once
and finding the entity's grid means walking its ancestors.

**Freezing the world is an engine capability, not an agent one.** `TimeControl` -- the step
countdown that sits alongside `Time<Virtual>`'s paused flag -- lives in `ename_engine::time`, and
`game.time.get` / `.set` is only a BRP surface over it. A pause menu wants the same thing,
and the agent must not be the only way to reach it. The method reports the clock and nothing else:
it used to report `GameState` too, which was the one reason `ename_remote` depended on
`example_game_lib`. Dropping the field removed that arrow from the
[layer graph](crate-layout.md#layer-graph), so the side channel now sits directly on the engine.

**Functionality reaches the agent as commands.** A feature crate registers a command in
`ename_engine::command` (see [commands.md](commands.md)); `ename_remote` exposes the registry
through five fixed BRP methods, and the sidecar lists the commands and offers each as a tool, with
the schema the game describes, re-listing every two seconds and sending `tools/list_changed` when
the set changes. Neither side-channel crate knows any feature, and neither changes when one is
added. This replaced per-feature BRP methods (`game.screenshot.*`, `game.video.*`, the editor's
`editor.*` methods inserted into `RemoteMethods`) and the hand-written tools over them, which
resolved paths, polled, and worded hints in the sidecar. `command_list` and `command_call` are
always present, so a session that started before the game, or a client that does not re-list,
can still reach every command.

**Slow work is a job, and the client does the waiting.** A screenshot finishes frames after it is
asked for and a recording runs for minutes, but a BRP handler answers in the frame it runs. A job
command returns a job id; the sidecar polls `game.job.status` and returns within 50 seconds,
either with the result or with the job still running and its id for `job_wait`. That keeps one
call under the 60 s timeout many MCP clients inherit, and the common short capture is still one
call. Every tool call runs under one hard deadline covering every request it makes, because the
HTTP client has none; the 50 s cap is the poll loop choosing to return, and the hard deadline only
catches a game that stopped answering.

**Failures are tool results, not protocol errors.** Claude Code shows the model only the message
of a JSON-RPC error and drops its `data`, where the error's kind and hint would be. So every tool
failure, a game that cannot be reached included, comes back as a tool result marked `isError`,
with the message, the kind and the hint in its text.

**The launcher lives in the sidecar binary.** `game_start`, `game_stop`, `game_restart` and
`game_status`, and the same verbs as `cargo game`, are a module of `ename_mcp`, not a crate or a
script. The sidecar already has the async runtime and the BRP client that readiness needs, a CLI
mode of the same binary adds no crate for one user, and agents without MCP get the same code.
Hand-written restart scripts were the evidence against a script: pattern kills, unbounded polls,
no check of who held the port.

**A detached supervisor per run, and a state directory.** Each launch spawns `ename_mcp supervise`
from its own copy of the binary, in its own session (Unix) or detached process group (Windows). It
builds the editor, runs it under `cargo run` in a process group or job object, and records how it
exited. Callers only edit `state.json` under a short lock and wait, always for a bounded time. So
a caller that times out, a `/mcp reconnect` or a closed client changes nothing, and any later
sidecar or CLI call can report or stop the game. The game running as the sidecar's child was
rejected because a reconnect would kill it or lose its exit status.

**Build before stop.** `game_restart` builds while the old game runs and swaps only after the
build succeeds, so a type error costs nothing but the build. Windows will not let the linker
replace a running executable, so there the old game is stopped first.

**Ready means the recorded process answers.** A launch is `ready` when the game process is alive,
`game.pid.get` answers on its port with that process's `os_pid`, and the main schedule has run
two frames. An answer from any other process is a port conflict. This exists because a stale
editor holding 15702 used to make a new one's failed bind invisible while the agent talked to the
old build. The game now also binds both its ports itself before Bevy does, and exits with code 3
when either is taken.

**No kill by name or pattern.** `pkill -f` matched the agent's own shell often enough to be a
known failure, and `pkill -x` silently matched nothing for names over 15 characters. The
supervisor stops a game with `game.exit` over BRP, then kills only its own child's group or job.
A game the launcher did not start is killed by pid only when the user asked (`take_over`), and
only after its start time, executable and working directory were checked again.

**Relative paths mean the agent's directory.** The sidecar sends its working directory with every
command call, and the game resolves relative paths against it. The game's own working directory
is wherever it was launched from, which under airshipgame is not the directory the agent works in.

**`ename_dep_src` reads `cargo metadata`.** Agents kept reading the wrong copy of a dependency: an
older registry version, or the registry `big_space` instead of the git checkout. `cargo metadata
--locked --offline` already names each resolved package's `manifest_path`, registry and git alike,
so `cargo xtask ename_dep_src <crate>` prints that directory. Parsing `Cargo.lock` would mean
reimplementing cargo's checkout path hashing. It is parsed with `serde_json` into three-field
structs; the `cargo_metadata` crate would add dependencies for no gain. It passes
`--all-features`, because without it metadata leaves out lock entries that only optional
features reach, and retries without it if an offline run fails on a crate never downloaded.

**Read-only reviewers do not run tests.** Cargo cannot take its build lock in a read-only
sandbox. Running the implementer's prebuilt test binaries instead was rejected: they are only as
fresh as the last build, so a reviewer could test old code without knowing it. The implementer
runs `cargo dev-test --no-fail-fast` after its last change and puts the result in the reviewer's
brief; a reviewer that needs another run asks the parent.

**A camera pose is position plus yaw and pitch in degrees.** Agents hand-computed quaternions
for every pose and kept helper scripts to do it. Degrees can be read, typed and diffed, and roll
is meaningless for the editor camera, which strips it every frame. Yaw 0 looks along -Z, 90 along
+X, pitch positive up: the mapping the editor's orbit already used. At the poles yaw comes from
the up vector, so a straight-down pose keeps its heading.

**Saved poses are viewpoints, not views.** "View" already names Scene View and Game View
(`editor_set_view`); `viewpoint_goto` next to it cannot be mistaken for it.

**Camera commands live in the editor; the pieces a game needs live in the engine.** The commands
need the editor camera and the shown view, and the viewpoints file is development configuration,
so `ename_editor` registers them and no shipping binary links them. The engine holds pose
conversion, absolute pose reads and writes, the ground probe registry and the facet registry,
which a game crate fills.

**Ground is optional and the game says where it is.** Not every game has open terrain: indoor
levels, multi-storey buildings, caves and space have no heightfield. So the guard only acts on
what the game has: `CameraGroundGuard` (default `both`, 2 m) picks which probes a placement asks,
and each call can override it with `ground`: `none`, `colliders`, `meshes` or `both`. Where no
probe finds ground, nothing is lifted or waited for.

- **Probes.** The engine always has two: one over static colliders and one over render meshes the
  game marks as terrain (`GroundSurface`), for terrain without a collider. A game can add its own,
  asked in every mode but `none`. A probe answers the support surface below a 3D point, with "no
  ground" and "not loaded" kept apart, so only data that is still streaming makes a placement wait.
  A probe may also answer rays (walls, ceilings, overhangs); probes that answer heights only are
  ray-marched, refining only between a known-above and a known-below sample.
- **Rules.** The built-in probes answer only what one ray each way says reliably, plus
  containment in convex solids. Being under terrain is opt-in through the marker, so a camera
  under an unmarked roof stays under it.
- **Lift.** The guard lifts instead of refusing, since a lift is what the agent would do next, and
  never lifts through a ceiling.

**Viewpoints are a versioned RON file in the repo, read on every call.** One `viewpoints.ron` at
the workspace root, path set by the editor binary. Not under `assets/`, which is outside the
repo's history. Re-read on every call so a hand edit or another agent's save takes effect without
a restart. A newer version is refused, never reinterpreted. Saves drop comments, so each viewpoint
has a `note`.

**A viewpoint can carry facets.** Typed state a crate registers with a capture and an apply
function, stored as raw RON and shown as JSON. `viewpoint_save` captures only the facets it is
told to, so a viewpoint never silently resets state a comparison depends on. An unregistered
facet fails before anything is applied.

**Capture sequences run in the game; image work stays outside.** `capture.viewpoints` and
`capture.fly_through` are editor job commands on one driver: show the view, then per step place,
settle if asked, capture, and finally write a manifest and restore what the run changed. In the
game because each step needs frame-exact order (the pose a frame before its capture, the next pose
after the readback), and the job model already gives progress, `job_wait` past 50 s and cancel. A
sidecar loop would cost a round trip per step and could not order them. In the editor, because the
viewpoints file, the editor camera and switching views are editor state. One sequence at a time,
since two would save and restore each other's state. Capture size is recorded, not pinned: a fixed
render size needs the Scene View drawn offscreen, which was rejected. Cropping, diffing and contact
sheets are external tools (ImageMagick, PIL, ffmpeg) on the files, which work everywhere and cost
this project no code.

## Debug switches

**One registry, in the engine.** A debug switch is a toggle or mode for debugging, performance
analysis or modding, never a gameplay control. A crate registers it in its plugin's `build`
(`ename_engine::debug_switch::DebugSwitchAppExt`): a category, a `namespace.field` name, a
description, and a reflected resource, a field of one, or a get/set pair. The registry lives in
`ename_engine`, so release builds have it too, for performance analysis and modders; release
builds have no remote, so there the keys are the front-end until a console exists. Four `Safe`
commands, `debug.list`, `debug.get`, `debug.set` and `debug.reset`, take flat params that a
console line maps onto. Rejected: a typed command per crate (a serde mirror of every enum, no
single listing), raw BRP resource writes (no discovery, no allowed values, no log line), env vars
read at runtime (invisible, need a restart to change), and a registry in the editor (absent from
release builds).

**Values go through `bevy_reflect`.** Debug resources already derive `Reflect`; few have a serde
and schemars mirror. Supported: `bool` and enums of unit variants, written as snake_case strings
like every other command. A key toggles a bool or cycles an enum in declaration order. Anything
else, a bad path, a bad name or category, two variants with one snake_case spelling, two switches
with one name, or two on one chord in one build target panics at startup, in
`DebugSwitchPlugin::finish`, so registration order does not matter.

**Keys belong to the registry.** One system in `PreUpdate` after `InputSystems` applies chords,
so `Update` sees a change in its frame. Chords match modifiers exactly, left and right alike; a
held Super blocks them all. That replaces hand-written "F5 but not Shift+F5" rules. Each switch
has an editor chord and a release chord (`.key()` sets both); `DebugSwitchTarget` picks the live
one, `Release` by default and `Editor` under `EditorPlugins`, and a game can move another crate's
chords with `rebind_debug_switch`. The editor gates the keys while egui has the keyboard.

**Writes compare first, read back, and log.** An equal value is not written, so change detection
stays precise. A write that did not take (a callback whose target is gone) is an error, not a
log line about a change that never happened. Every change logs `debug switch <name> = <value>
(<source>)`, the source being `command`, `menu` or the chord. Launch values are recorded the
first time a switch can be read, not at startup, because some apply only later (the editor camera
joins a grid after startup).

**Diagnostics logging is a switch.** `EnginePlugins` adds frame-time, entity-count and log
diagnostics when absent; `diagnostics.log` (`off`, `frame_times`, `all`) sets the log filter, off
by default. `EditorPlugins` adds `RenderDiagnosticsPlugin`, so only editor builds pay for GPU
timestamp queries.

**Experiment toggles are switches, not env vars.** The next `NB_OFF`-style toggle is a `bool`
field on the crate's debug resource registered as a debug switch: about as many lines, listed,
and flipped without a restart.

## Early failure detection

**Errors ride on every result.** The game sends a cumulative log tally (ERROR and WARN counts,
the newest of each, the newest sequence number) beside every run id: in `game.pid.get`, in every
command and job envelope, and in a command error's `data`. The sidecar keeps the highest tally
each run has reported to the agent and adds a `log` notice to a result only when something is
new, failed results included. Cumulative counts keep the game stateless per client, add no BRP
parameter, and degrade both ways: an older sidecar ignores the field, a newer sidecar against an
older game prints nothing. Tool calls run concurrently, so the baseline is a high-water mark per
run (the last four runs); a late, lower tally adds nothing and never lowers it. The mark lives in
memory, so a `/mcp reconnect` repeats a run's tally once. A signal the agent has to ask for is one
it will not ask for, which is why `pid` is on every result too. Rejected: failing screenshots
when a pipeline is in error, which is narrower than the log and would block captures that are
fine.

**Panics go into the log buffer.** Std prints panics to stderr only, which a BRP client cannot
see. `LogBufferPlugin` installs a chained panic hook that pushes one ERROR entry under target
`panic`, then calls the hook it replaced. It must be outermost, because Bevy's
`error_panic_hook` skips the inner hook for its own error handler's panics; building after
`DefaultPlugins` gives that.

**A dead game says how it died.** `ename_remote` writes a run record to
`<runtime dir>/ename/brp-<port>.json` (`$XDG_RUNTIME_DIR` on Linux, else the temp dir): run id,
OS pid, executable, start time, the latest panic plus a count, and the exit code on `AppExit`.
Written atomically at plugin build (after the port check, so a second game that fails to bind
leaves the record alone), from a panic hook, and from a `Last` system. Keyed by port, not by the
launcher's state directory, because a game the user started by hand has no instance, and the
port is the one fact the game and the sidecar share. When a call gets no answer (refused, body
lost, timed out), the sidecar reads the record and says, in this order: it exited (with its
`AppExit`), it is running but not answering, it panicked (and where), or it ended with neither
(killed, or a native crash). Liveness checks pid, executable and start time through `sysinfo`,
so a recycled pid never counts. A panic followed by a clean exit was survived and is not the
cause of death. The launcher's `crash` block takes the panic from the same record. The sidecar
names no first-party crate, so both sides read the format on their own and test against
`ename_mcp/tests/fixtures/run_record.json`.

**Known crashes are a markdown file.** `docs/known-crashes.md` lists each crash as a `##`
heading and a `Signature:` line holding a literal substring of the panic message or location.
The sidecar matches a panic or the last error against it and links the section. A path list in
`ENAME_KNOWN_CRASHES` lets a downstream repo list its own file and the engine's.

## What the agent can actually see

Type registration is a correctness requirement for MCP, not a convenience for the inspector. An
unregistered component is one the agent cannot see, and it fails silently. A component type or
subtype missing `bevy_reflect::Reflect` cannot be inspected or mutated by the agent. So register
component types and derive `Reflect` wherever possible.
