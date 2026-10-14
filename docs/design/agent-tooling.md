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

**Relative paths mean the agent's directory.** The sidecar sends its working directory with every
command call, and the game resolves relative paths against it. The game's own working directory
is wherever it was launched from, which under airshipgame is not the directory the agent works in.

## What the agent can actually see

Type registration is a correctness requirement for MCP, not a convenience for the inspector. An
unregistered component is one the agent cannot see, and it fails silently. A component type or
subtype missing `bevy_reflect::Reflect` cannot be inspected or mutated by the agent. So register
component types and derive `Reflect` wherever possible.
