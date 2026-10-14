# Driving the game from a coding agent

The game can expose its running world to an MCP client. Two processes:

```
agent  --stdio MCP-->  ename_mcp  --HTTP JSON-RPC-->  game  (bevy_remote + custom methods)
```

Why the design is shaped this way is in
[design/agent-tooling.md](design/agent-tooling.md); the plan it came from is
`scratch/mcp-server-plan.md`.

## Running it

Run the editor binary. It is the only target that links `ename_remote`, so it is the only one
with a server to talk to:

```sh
cargo run -p example_game_editor_bin
```

It listens on `127.0.0.1:15702`. `example_game_bin` is the shipping composition and has no BRP
server at all -- it runs and renders normally, nothing binds the port, and every tool call fails
with "cannot reach the game's remote server" as though the game were down. See Security below for
why the split exists.

Expect a long first build: the editor pulls in egui and the dev-only Bevy features, and this
workspace optimizes dependencies, so a target directory warm for `example_game_bin` still
recompiles most of the render stack. Give it ten minutes before concluding anything is wrong.

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

`ename_remote` must never be in a shipping build. BRP is unauthenticated read and write access to
the world, and localhost is not a trust boundary. Any process running as the same user can connect.

It is linked into `example_game_editor_bin` because every build made from that binary is a
development build, the same reason it carries the editor and the dev-only Bevy features.
`example_game_bin` is the shipping composition, and `scripts/check-layers.sh` fails if
`ename_remote` or the editor reaches it. CI runs the check, so a crate that is convenient here
cannot become a crate that ships by accident.

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
| `time_get` / `time_set` | Read the virtual clock; pause, resume, or step N frames. |
| `log_get_entries` | Recent tracing events, filtered by level, target and message. |
| `editor_get_view` / `editor_set_view` | Which editor view is shown, Scene View or Game View; switch it as clicking its tab would. |
| `screenshot_capture` | Write the game window, the game camera or the editor camera to a PNG file. |
| `video_record` / `video_status` / `video_stop` | Record the game window, the game camera or the editor camera to an `.mp4` or `.mkv` file; follow or stop a long recording. |

Every result also carries `pid`, three letters naming the run of the game process that answered
it. See "Noticing a restart".

A few things about them are worth knowing before reading the schemas.

**Entities are addressed by `name` or by `entity`, and results carry both.** An entity id is a
generation-and-index bit pattern that changes every run, so it cannot go into a written plan or
be quoted to a user. Names can. But an entity need not have one, and names are not unique: the
starting stage has three called `VirtualCamera`. An ambiguous name is an error listing the
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

1. `time_set` `pause`
2. `world_get_components` -- the before state
3. `time_set` `step`, `frames: 30`. This returns only once the frames have run.
4. `world_get_components` -- the after state
5. `time_set` `resume`

`elapsed_seconds` is virtual time and does not advance while paused. `frame` counts real frames
and keeps climbing, because the renderer is still drawing.

## Switching the view

The editor shows either Scene View, its own free camera, or Game View, the loaded stage's
`MainCamera`. `editor_set_view` with `view: "game"` switches to Game View the same way clicking
its tab does: the tab becomes active and its dock leaf takes focus. Both tools return
`{"view": "scene" | "game", "game_camera": bool}`.

`game_camera` is true when exactly one entity has `MainCamera`, `Camera` and `RenderTarget`. That
is the camera Game View renders. Switching to Game View still works when it is false; the tab then
shows "The loaded stage has no Main Camera."

`editor_set_view` returns after the game has run three more frames, and its result is read back
from the editor rather than echoed. So a screenshot taken right after it shows the new view, and if
something overrode the switch, the result says so. The frame count is the main loop's, so this does
not prove the GPU presented the frame. It works while time is paused, because frames keep running.

These methods belong to the editor, so a binary without it reports that the build "does not expose
editor view switching". An editor built before the methods existed reports the same.

## Screenshots

`screenshot_capture` takes a `path` and a `source`, writes a PNG, and returns the absolute path
and its size in physical pixels. Open the file to look at it; the image is not returned inline.

| `source` | What is in the PNG |
| --- | --- |
| `window` | The whole game window, editor panels included. |
| `game_camera` | What the stage's `MainCamera` rendered. In the editor, the Game View. |
| `editor_camera` | The editor's Scene View, cropped from the window, gizmos and grid included. |

A camera only renders while its view is shown, so `game_camera` needs Game View and
`editor_camera` needs Scene View. The wrong one fails with a hint naming the `editor_set_view`
call that fixes it. The tool never switches the view itself, because that changes what the user
is looking at.

A camera that draws into the window, the Scene View's included, is captured as a crop of the
finished window, so anything drawn over it inside that rect is in the picture too.

A relative `path` resolves against the MCP server's working directory, normally the project
root, not against the game's. Missing directories are created, an existing file is replaced, and
the file appears whole or not at all. Only `.png` is accepted.

The capture is a GPU readback that finishes a frame or two later, and the tool waits for it. A
minimized window may never present a frame; the game gives up after 5 seconds and the tool after
10. Capturing works while time is paused, because frames keep running.

Game code uses the same capture through `ename_engine::screenshot`: spawn a `ScreenshotRequest`
and observe `ScreenshotFinished`.

## Video

`video_record` takes the same `path` and `source` as `screenshot_capture`, plus a `duration`
with its unit as the only key:

| `duration` | Records |
| --- | --- |
| `{"frames": 600}` | 600 rendered frames, one video frame each. Plays in real time only if the game ran at `frame_rate`. |
| `{"seconds": 10}` | 10 s of real time, resampled to `frame_rate`. |
| `{"timecode": "00:01:30"}` | The same, as `hh:mm:ss` or `hh:mm:ss.fff`. |

A bare number is refused, because JSON cannot tell `10` frames from `10.0` seconds. `frame_rate`
defaults to 60 and goes up to 240. The output is H.264, `yuv420p`, tagged as sRGB (BT.709
primaries and matrix, sRGB transfer). Odd sizes lose their last row or column.

The tool waits for the file for up to 50 seconds. A longer recording comes back with
`status: "recording"`, its `request` id and a note; `video_status` with `wait_seconds` (up to 50)
collects it, and `video_stop` ends it early and waits up to 50 seconds for the file. `wait: false` returns as soon
as the recording has started.

The recording follows real time, so it continues while the world is paused. A camera source must
stay visible: switching the view, resizing the window or the tab, or five seconds without a frame
(a minimized window) ends the recording early. What was recorded is kept, and `ended` says why
(`stopped`, `source_resized`, `camera_inactive`, `source_lost`, `stalled`). A recording that ends
before its first frame is an error.

Encoding runs in an `ffmpeg` child process, which must be on the game's `PATH`. The game picks the
first encoder that passes a test encode: VAAPI, NVENC or QSV on Linux, then `libx264`.
`ENAME_VIDEO_ENCODER=libx264` (or another known encoder name) in the game's environment forces
one. When ffmpeg cannot keep up, frames are dropped rather than slowing the game, and
`dropped_frames` counts them.

Video needs the `video` cargo feature, which `example_game_editor_bin` enables by default. Game code
uses the same recorder through `ename_engine::video`: spawn a `VideoRequest` and observe
`VideoFinished`.

## What the agent cannot see

Reflection registration decides visibility, and it fails silently. Colliders are invisible,
asset handles read as errors, and a component that is not `#[derive(Reflect)]` and registered
does not exist as far as these tools are concerned.
[design/agent-tooling.md](design/agent-tooling.md#what-the-agent-can-actually-see) has the
measured list.
