# Driving the game from a coding agent

The game can expose its running world to an MCP client. Two processes:

```
agent  --stdio MCP-->  ename_mcp  --HTTP JSON-RPC-->  game  (bevy_remote + custom methods + commands)
```

Why the design is shaped this way is in
[design/agent-tooling.md](design/agent-tooling.md); the plan it came from is
`scratch/mcp-server-plan.md`. Shell, process, disk, git and test rules for agents on this machine
are in [agent-environment.md](agent-environment.md).

## Running it

Start the editor with `game_start` over MCP, or `cargo game start` from a shell. Both build and
run the package `[workspace.metadata.ename.launch]` in `Cargo.toml` names
(`example_game_editor_bin`, the only target that links `ename_remote`) and wait until it answers.
`game_restart` rebuilds after a code change, `game_stop` stops it, `game_status` reports it. Never
start it with a hand-written `cargo dev-run &`, and never stop it with `pkill`: see "Lifecycle".

`example_game_bin` is the shipping composition and has no BRP server. The launcher cannot run it,
but a hand-started one makes every tool call fail as though the game were down.

Expect a long first build: the editor pulls in egui and the dev-only Bevy features, and this
workspace optimizes dependencies. `game_start` returns `building` after 50 seconds;
`game_status` with `wait_seconds: 50` follows it.

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

`ENAME_BRP_URL` pins the address. The session then never follows a started instance to another
port, and starting one on another port is an error.

The sidecar does not need the game to be running when it starts. A call made while the game is
down fails with a message saying so, and works again once the game is back.

The sidecar is not rebuilt by `game_start`. Whoever changes `ename_mcp` builds it
(`cargo build -p ename_mcp`) and asks the user for `/mcp reconnect`.

## Lifecycle

| Tool | CLI | Does |
| --- | --- | --- |
| `game_start` | `cargo game start` | Build and start the editor, or report the one running, and wait until it answers. |
| `game_restart` | `cargo game restart` | Build while the old game keeps running; on success stop it and start the new one. On Windows the old game is stopped before the build, because Windows will not let the build replace a running executable, so a failed build leaves nothing running. Params you leave out are the last launch's. |
| `game_stop` | `cargo game stop` | `game.exit` over BRP, then a kill after `grace_seconds` (default 5). |
| `game_status` | `cargo game status` | Report the state; `wait_seconds` waits out a build or startup. |

Launch params: `instance`, `port`, `env` (e.g. `{"AIRSHIP_DIAGNOSTICS": "1"}`), `features`,
`window_size`, `render_size`, `wait_seconds` (0 to 50, default 50). `settle` is accepted and
ignored until a settle wait exists.

**States.** `stopped`, `building`, `build_failed` (with `errors` in `path:line:col: error[...]`
form), `starting`, `ready`, `exited` (with `exit` and, for an error code or a crash, `crash` with
the cause and the last lines of the log), `external` and `port_in_use` (with `holder`). `ready`
means the game process is alive, `game.pid.get` answers on its port from that process, and the main
schedule has run two frames. It does not mean a frame was presented. A launch that ends in
`build_failed`, `port_in_use` or a crash is a tool error, and so is one whose supervisor died
(`error` says why) and a stop that ran out of time. A `restart` whose build fails says so under
`pending`; the old game is still running, except on Windows.

**Ownership.** Every launch records who started it: the MCP session's client process, the
`--owner` token or `ENAME_GAME_OWNER` on the CLI, or nobody, meaning the user. Only the owner may
stop or restart a game. Anything else is refused with `not_owner` unless `take_over` is passed,
and that is only for when the user asked. Subagents share their parent's MCP session, so they
share its games. An agent driving the CLI must pass a token: `cargo game owner --new` mints one,
and `game_status` reports the session's own as `caller`, to hand to a separate-process agent as
`ENAME_GAME_OWNER`. Before you finish, stop the games you started (`game_status` lists them under
`yours`), unless you are leaving one running for the user to inspect; then say so.

**Ports and instances.** The `default` instance uses 15702 and the render world's listener 15703.
Another `instance` gets the first free pair from 15704, and gives up a recorded pair another
running instance now uses. A started instance becomes the one this session's world tools talk to
as soon as its run is created, even while it builds, and stays so across `/mcp reconnect`. When it
stops, they fall back to 15702. Another owner's start never moves it. `game_start {instance}` on
a running instance selects it. A game of another instance on the port is reported as that
instance's and is never stopped as an external game.

**Games the launcher did not start.** An editor started from a terminal still works with every
world tool. `game_status` reports it as `external`, and `game_start` reports `port_in_use` with its
pid and executable. `game_stop` with `take_over` stops it after checking it is this workspace's
editor binary run from this workspace, and reports its exit as unknown.

**A taken port is loud.** The game itself exits with code 3 and `BRP port 15702 is in use:
another game is running` when either of its ports is taken, rather than running without a server.
`ENAME_BRP_PORT` sets the port pair for a game started by hand.

**Where things are.** `target/ename-launch/<workspace>-<hash>/<instance>/`: `game.log` (plain
text, `RUST_BACKTRACE=1`; the previous run's is `game.prev.log`), `build.jsonl` and `build.log`
from the last build, `exit.json`, `state.json` and `supervise.log`. Every result names the log
under `log`.

**When `cargo game` itself does not build.** It builds into `target/ename-launch-bin`, so it
answers while the editor builds. If an `ename_mcp` edit breaks it, the last good binary still works:
`target/ename-launch-bin/debug/ename_mcp game stop`. Run `cargo game` from the repo root: the
alias's paths are relative to the working directory.

**Windows under the Bash tool.** There cargo's job object may forbid the supervisor to break away,
so force-killing `cargo game start --wait N` (or `taskkill /T` on it) can end the launch. Start
with `--wait 0` and follow with `cargo game status --wait N`.

**A game in a worktree without assets.** A linked worktree lacks the main checkout's gitignored
assets. If the game starts with missing assets there, run the repo's worktree asset setup
(airshipgame: `scripts/worktree-assets.sh <worktree>`) and restart it.

CLI exit codes: 0 ready or stopped, 1 build failed or crashed, 2 still building or starting, 3
port in use or external game, 4 refused or invalid.

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
| `command_list` / `command_call` | The commands the game registers, and calling one by name. Always present. |
| `job_list` / `job_wait` / `job_cancel` | Follow, collect or end the work a command started. |
| one tool per game command | Present while the game runs; see "Commands and jobs". Today: |
| `editor_get_view` / `editor_set_view` | Which editor view is shown, Scene View or Game View; switch it as clicking its tab would. |
| `screenshot_capture` | Write the game window, the game camera or the editor camera to a PNG file. |
| `video_record` | Record the game window, the game camera or the editor camera to an `.mp4` or `.mkv` file. |

Every result also carries `pid`, eight letters naming the run of the game process that answered
it. See "Noticing a restart".

A failed call comes back as a tool result marked as an error, with the reason, its stable kind in
parentheses, and what to do about it when the game knows.

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

So every tool result carries `pid`, eight lowercase letters naming the run:

```json
{"pid": "kqvzmbta", "entity": 4294966691, "name": "Cube", "position": [0, 1, 0]}
```

If it differs from the previous call, the game restarted. Discard every id you were holding and
re-resolve by name. The value comes from the process id, the start time and an OS-seeded hash, so
two runs share one essentially never. Job ids start with it (`kqvzmbta-3`), so a job id from an
earlier run is recognisably stale. A command result's `pid` comes from the same answer as the
result, so it cannot belong to a different run than the result does.

A dead game is a different signal: the call fails with "cannot reach the game's remote server",
and `game_status` says why.
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

`editor_set_view` returns once the shown view's camera is active and one more frame has run (at
most ten frames, for a Game View with no camera), and its result is read back from the editor
rather than echoed. So a screenshot taken right after it shows the new view, and if something
overrode the switch, the result says so. The frame count is the main loop's, so this does not
prove the GPU presented the frame. It works while time is paused, because frames keep running.
Cancelling it with `job_cancel` only stops the wait: the view has already changed.

These are editor commands, so a binary without the editor has no `editor_*` tools, and
`command_call` reports `unknown_command`.

## Screenshots

`screenshot_capture` takes a `path` and a `source`, writes a PNG, and returns the absolute path
and its size in physical pixels. Open the file to look at it; the image is not returned inline.

| `source` | What is in the PNG |
| --- | --- |
| `window` | The whole game window, editor panels included. |
| `game_camera` | What the stage's `MainCamera` rendered. In the editor, the Game View. |
| `editor_camera` | The editor's Scene View, cropped from the window, gizmos and grid included. |

A camera only renders while its view is shown, so `game_camera` needs Game View and
`editor_camera` needs Scene View. The wrong one fails with `camera_inactive` and a hint naming
the `editor_set_view` call that fixes it. The tool never switches the view itself, because that
changes what the user is looking at. `source` takes whatever the game registers: the editor adds
`editor_camera`, and the tool's schema lists the names.

A camera that draws into the window, the Scene View's included, is captured as a crop of the
finished window, so anything drawn over it inside that rect is in the picture too.

A relative `path` resolves against the MCP server's working directory, normally the project
root, not against the game's: the sidecar sends its directory with every command, and the game
resolves against that. Missing directories are created, an existing file is replaced, and the
file appears whole or not at all. Only `.png` is accepted.

The capture is a GPU readback that finishes a frame or two later, and the tool waits for it. A
minimized window may never present a frame; the game gives up after 5 seconds. Capturing works
while time is paused, because frames keep running. `job_cancel` on a screenshot is best effort: a
PNG already being encoded is still written.

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
defaults to 30 and goes up to 240. The output is H.264, `yuv420p`, tagged as sRGB (BT.709
primaries and matrix, sRGB transfer) with left-sited chroma. Odd sizes lose their last row or
column. ffmpeg's default conversion treats 4:2:0 chroma as centre-sited, so a still extracted with
a plain `ffmpeg -i clip.mp4 frame.png` can show a sub-pixel colour shift at sharp edges.

The tool waits for the file for up to 50 seconds. A longer recording comes back with
`state: "running"`, its `job` id, its progress and a note; `job_wait` with `wait_seconds` (up to
50) collects it, and `job_cancel` ends it early, keeps what was recorded, and waits up to 50
seconds for the file. A recording cancelled before its first frame ends `cancelled`, with no file.
`wait_seconds: 0` returns at once.

The recording follows real time, so it continues while the world is paused. A camera source must
stay visible: switching the view, resizing the window or the tab, or five seconds without a frame
(a minimized window) ends the recording early. What was recorded is kept, and `ended` says why
(`stopped`, `source_resized`, `camera_inactive`, `source_lost`, `stalled`). A recording that ends
before its first frame is an error.

Encoding runs in an `ffmpeg` child process, which must be on the game's `PATH`. The game picks the
first encoder that passes a test encode: VAAPI, NVENC or QSV on Linux, then `libx264`.
`ENAME_VIDEO_ENCODER=libx264` (or another known encoder name) in the game's environment forces
one. The GPU converts frames to NV12 before ffmpeg sees them. ffmpeg runs at below-normal CPU
priority (nice 10 on Linux; a process manager such as ananicy-cpp may renice it by name afterwards)
and `libx264` uses at most 4 threads. When the encoder cannot keep up,
or a screenshot of the same target is taken in the same frame, frames are dropped rather than
slowing the game, and `dropped_frames` counts them. A timed recording reads back only one frame per
output tick. When a recording ends with drops, the game logs them by reason (`no_slot`,
`no_source`, `conflict`, `mismatch`, `unsupported`, `unused`, `map_error`, `send_failed`,
`out_of_order`, `screenshot`); read them with `log_get_entries` and `message_contains: "video:"`.

Video needs `ffmpeg` on PATH at runtime. Game code uses the same recorder through
`ename_engine::video`: spawn a `VideoRequest` and observe `VideoFinished`.

## Commands and jobs

Functionality a game crate exposes on purpose -- screenshots, video, the editor's views -- is a
command registered in the game, not a tool written into the sidecar. `command_list` lists what the
running build has; each command also becomes its own tool, named after it with the dot replaced
(`screenshot.capture` is `screenshot_capture`), with the schema the game describes. The sidecar
re-reads the list every two seconds and tells the client when the tools change, so a game started
after the session gains its tools without a reconnect. Until then, or if a client does not
re-list, `command_call` with the dotted name reaches any command.

A command either answers at once or starts a job. A job tool waits up to `wait_seconds` (default
and maximum 50) and then returns `{"job", "state": "running", "progress", "note"}`; `job_wait`
picks it up, `job_cancel` ends it early, and `job_list` shows running jobs and finished ones kept
for five minutes. What cancelling keeps depends on the job, as its section here says.

A tool built from one run of the game is refused by the next: if the game restarted since the
tools were listed, the call fails with "nothing was run", the tools are refreshed, and calling
again runs it against the new run. A call is never repeated automatically, because it may change
the world.

The BRP methods behind this, for a raw client: `game.command.list`, `game.command.call`,
`game.job.status`, `game.job.list` and `game.job.cancel`. See
[design/commands.md](design/commands.md) for why it is shaped this way.

## What the agent cannot see

Reflection registration decides visibility, and it fails silently. Colliders are invisible,
asset handles read as errors, and a component that is not `#[derive(Reflect)]` and registered
does not exist as far as these tools are concerned.
[design/agent-tooling.md](design/agent-tooling.md#what-the-agent-can-actually-see) has the
measured list.
