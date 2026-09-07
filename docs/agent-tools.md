# Driving the game from a coding agent

The game can expose its running world to an MCP client. Two processes:

```
agent  --stdio MCP-->  ename_mcp  --HTTP JSON-RPC-->  game  (bevy_remote + custom methods)
```

Why the design is shaped this way is in
[design/agent-tooling.md](design/agent-tooling.md); the plan it came from is
`scratch/mcp-server-plan.md`.

## Running it

The `agent` feature is on by default, so a plain run listens on `127.0.0.1:15702`.

```sh
cargo run -p ename
```

Build the sidecar once:

```sh
cargo build -p ename_mcp
```

Register it with the client. For Claude Code, in `.mcp.json` at the repo root:

```json
{
  "mcpServers": {
    "game": {
      "command": "./target/debug/ename_mcp"
    }
  }
}
```

`ENAME_BRP_URL` overrides the address if the game is not on the default port.

The sidecar does not need the game to be running when it starts. A call made while the game is
down fails with a message saying so, and works again once the game is back.

## Security

The `agent` feature must never be on in a shipping build. BRP is unauthenticated read and write
access to the world, and localhost is not a trust boundary. Any process running as the same user
can connect.

It is in `default` because every build made here is a development build, alongside `dev` and
`editor` which carry the same warning. A shipping build is `--no-default-features`, and
`scripts/check-layers.sh` fails if `ename_remote` reaches the binary through that graph. CI runs
both, so the feature being convenient by default cannot become the feature shipping by accident.

## The tools

| Tool | Does |
| --- | --- |
| `world_query` | Find entities by name substring, component, or parent. The discovery tool. |
| `world_list_components` | The component type paths on one or more entities, without their values. |
| `world_get_components` | The values of named components on one entity. |
| `world_get_position` | Where an entity is: absolute metres, its grid, and the `CellCoord`, `Transform` and `GlobalTransform` behind them. |
| `registry_schema` | The JSON schema of registered types: the fully-qualified type paths, what fields a component has and their shapes. |
| `world_spawn_entity` | Create an entity with a name, components and a position. |
| `world_despawn_entity` | Delete an entity and its children. |
| `world_insert_component` / `world_remove_component` | Add or drop components. |
| `world_mutate_component` | Set one field of one component. |
| `world_reparent_entity` | Move an entity in the hierarchy. |
| `world_set_position` | Move an entity to an absolute world position. |
| `run_get_state` / `run_set_state` | Read run state; pause, resume, or step N frames. |
| `log_get_entries` | Recent tracing events, filtered by level, target and message. |

Every result also carries `pid`, three letters naming the run of the game process that answered
it. See "Noticing a restart".

A few things about them are worth knowing before reading the schemas.

**Entities are addressed by `name` or by `entity`, and results carry both.** An entity id is a
generation-and-index bit pattern that changes every run, so it cannot go into a written plan or
be quoted to a user. Names can. But an entity need not have one, and names are not unique: the
starting scene has three called `VirtualCamera`. An ambiguous name is an error listing the
candidates.

**Discovery hides the ECS's own entities.** Bevy stores resources, observers and registered
systems as entities, and in this project that is over 500 of them against seventeen named ones.
`world_query` filters them out and sorts named entities first. Pass `include_internal` to see
them.

**Writing a component starts at `registry_schema`.** It takes a `contains` substring, so
"rigidbody" finds `avian3d::dynamics::rigid_body::RigidBody`, and it returns the field names and
shapes the value must have along with the path. Ask it for the types you actually need: the full
registry is 1330 types and about 775 KB, and the default `limit` of 100 only caps how much of an
unfiltered call comes back. A path it reports under `unregistered` is one nothing can read or
write, which is an answer rather than a failure.

**Reading a component means naming it.** `world_get_components` takes full type paths and
nothing else, because the alternative -- an entity dumped whole -- costs more context than it
usually pays back. `world_list_components` lists the paths one or more entities have, and
`registry_schema` turns a partial name into a full one. A requested path the entity does not
have comes back under `absent`, apart from the ones whose value would not serialize, which come
back under `unreadable`.

**Positions are absolute metres, Y up.** Under big_space a position is a grid cell plus a
`Transform` offset from an origin that moves with the camera, so a raw `Transform.translation`
means a different world point from one frame to the next. `world_mutate_component` refuses to
write `Transform.translation` or `CellCoord` and points at `world_set_position` instead.
`world_get_position` is the read side: it returns the absolute metres, the grid entity that frame
belongs to, and the three components underneath, so a `Transform` that looks wrong can be checked
against the position it actually produces.

## Noticing a restart

Nothing the agent holds between calls survives the game restarting, entity ids least of all. The
sidecar does not: it keeps no connection and no cached state, so a call after a restart just
works, with no reattach step. That is convenient and it is the hazard. A plan built against one
run keeps being applied to the next, and the failures read as unrelated bugs.

So every tool result carries `pid`, three lowercase letters naming the run:

```json
{"pid": "uuq", "entity": 4294966691, "name": "Cube", "position": [0, 1, 0]}
```

If it differs from the previous call, the game restarted. Discard every id you were holding and
re-resolve by name. The value comes from the process id mixed with the process start time, so
two runs differ whatever the scheduler does with pids. It is three letters because it rides on
every response; 17576 values means two successive runs collide about once in 17576, which costs
a missed warning and never gives a false one.

A dead game is a different signal: the call fails with "cannot reach the game's remote server".
"Connection refused" means the process is gone. "Connection closed before message completed"
means a handler panicked mid-request, and the game may still be running in an unknown state, so
read `log_get_entries` before trusting it.

## Working with a frozen world

Reading a component from a world that is still advancing answers a different question from the
one usually being asked. The loop that makes an experiment:

1. `run_set_state` `pause`
2. `world_get_components` -- the before state
3. `run_set_state` `step`, `frames: 30`. This returns only once the frames have run.
4. `world_get_components` -- the after state
5. `run_set_state` `resume`

`elapsed_seconds` is virtual time and does not advance while paused. `frame` counts real frames
and keeps climbing, because the renderer is still drawing.

## What the agent cannot see

Reflection registration decides visibility, and it fails silently. Colliders are invisible,
asset handles read as errors, and a component that is not `#[derive(Reflect)]` and registered
does not exist as far as these tools are concerned.
[design/agent-tooling.md](design/agent-tooling.md#what-the-agent-can-actually-see) has the
measured list.
