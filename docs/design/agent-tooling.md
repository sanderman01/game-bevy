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

**`ename_remote` is behind an `agent` feature and must never ship.** BRP is unauthenticated read
and write access to the running world, and any process running as the same user can connect.
Localhost is not a trust boundary. The feature is in `default` because every build made here is a
development build; a shipping build is `--no-default-features`. `scripts/check-layers.sh` asserts
the crate is absent from that graph, the same mechanism that keeps the editor out.

**Every tool result carries a three-letter run id, named `pid`.** The sidecar holds no connection
and no cached state, so a call after the game restarts just works. That is right, and it is also the
trap: nothing otherwise tells the agent the world it is editing is not the one it planned against.
Tagging every response needs no extra call and no memory of having asked. It is deliberately not
part of `time_get`, because a signal only visible on request is one the agent will not think to
look for. Three letters keeps the per-response cost near zero.

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
`ename_game`. Dropping the field removed that arrow from the
[layer graph](crate-layout.md#layer-graph), so the side channel now sits directly on the engine.

## What the agent can actually see

Type registration is a correctness requirement for MCP, not a convenience for the inspector. An
unregistered component is one the agent cannot see, and it fails silently. A component type or
subtype missing `bevy_reflect::Reflect` cannot be inspected or mutated by the agent. So register
component types and derive `Reflect` wherever possible.
