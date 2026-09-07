# Agent tooling

Part of the [design record](../design.md). How to use the tools is in
[../agent-tools.md](../agent-tools.md); the full plan is in `scratch/mcp-server-plan.md`. The
decisions:

**Two processes.** The game runs a `bevy_remote` BRP server; `ename_mcp` is a standalone binary
speaking MCP over stdio and forwarding to BRP over HTTP. Unreal puts its MCP server inside the
editor; this does not. The game binary gains one optional dependency instead of linking an MCP
implementation and its async runtime. The agent's client launches the sidecar itself, so there is
no port to allocate and no client config to regenerate. The sidecar survives a game crash. A dead
game is one tool error rather than a dead MCP server, and a panic that leaves the process up still
lets the agent read the logs. The cost is one hop and a reserialization per call, which is
irrelevant at this call volume.

**`ename_remote` is behind an `agent` feature and must never ship.** BRP is unauthenticated read
and write access to the running world. Localhost is not a trust boundary. Any process running as
the same user can connect. The feature is in `default` because every build made here is a
development build; a shipping build is `--no-default-features`. `scripts/check-layers.sh` asserts
the crate is absent from that graph, which is the same mechanism that keeps the editor out.

**Every tool result carries a three-letter run id, named `pid`.** The sidecar holds no
connection and no cached state, so a call after the game restarts works with no reattach step.
That is the right behaviour and it is also the trap. Nothing tells the agent the world it is
editing is not the one it planned against. Tagging every response is the cheapest place to put
that signal, because it needs no extra call from the agent and no memory of having asked. It is
deliberately not part of `run_get_state`. Pausing and restarting are unrelated concerns, and a
signal only visible on request is one the agent will not think to look for. Three letters keeps
the cost of carrying it on every response near zero.

**Every tool that names an entity takes a name or an id, and every result carries both.** An
`Entity` is a generation-and-index bit pattern that changes every run, so an id cannot be written
into a plan or quoted back to the user. Names can. Neither works alone. An entity need not have a
`Name`, and names are not unique. The starting scene has three entities named `VirtualCamera`. An
ambiguous name is an error listing the candidates, never a silent pick of the first match. The id
crossing the boundary is the raw `Entity::to_bits` integer. A readable `606v0` form, index and
generation written out, was built and then reverted. An `Entity` nested inside a component value
serialises as a bare integer, with nothing in the JSON marking it as one. Keeping the readable
form honest would have meant a type-driven rewrite of every entity leaf inside a component value,
driven off the type registry, on the read path and the write path both. The accepted cost is that
an id no longer matches what the editor's hierarchy panel shows beside the same entity.

**Positions cross the boundary as absolute double-precision metres.** Under big_space a position
is a `CellCoord` plus a `Transform` relative to a floating origin that moves with the camera. A
bare `Transform.translation` is a number that is correct for one frame in one cell. The conversion
happens engine-side in `game.position.get` / `.set`, not in the sidecar, because the write has to
set both halves at once and finding the entity's grid means walking its ancestors.

`ename_remote` sits above `ename_game` rather than above `ename_engine` alone, because
`game.run_state.get` reports `GameState`, which is a gameplay concept. It adds no arrow the
[layer graph](crate-layout.md#layer-graph) forbids.

**Entities visible to the agent are serialized as u64 integer values.**
These are the entity values that BRP sends to the mcp server, in the form of 
bevy::prelude::Entity {to_bits, from_bits}
Bear this in mind whenever you try to compare entities from mcp and the in game editor hierarchy panel,

## What the agent can actually see

Types registration is important for mcp rather than merely a convenience for the inspector.
An unregistered component is one the agent cannot see, and it fails silently.

A component type or subtype which is missing the `bevy_reflect::Reflect` trait will not allow its
data to be inspected or mutated by the agent.

For these reasons we should register component types and add the Reflect trait wherever possible.

