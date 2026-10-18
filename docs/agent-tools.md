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

Build the sidecar once, and again after pulling a change to its path or its sources, before
`/mcp reconnect`:

```sh
cargo mcp-build
```

The alias builds into its own target dir, `target/ename-mcp-bin`, so `cargo dev-build` and
`cargo game` never replace the binary a session runs. airshipgame's `cargo mcp-build` builds the
same binary. Register it with the client. For Claude Code, in `.mcp.json` at the repo root:

```json
{
  "mcpServers": {
    "game": {
      "command": "./target/ename-mcp-bin/debug/ename_mcp"
    }
  }
}
```

`ENAME_BRP_URL` pins the address. The session then never follows a started instance to another
port, and starting one on another port is an error.

The sidecar does not need the game to be running when it starts. A call made while the game is
down fails with a message saying so, and works again once the game is back.

### When ename_mcp is out of date

`game_start` does not rebuild the sidecar. The client starts it once per session and keeps that
process. Only the user can replace it, with `/mcp reconnect`; an agent cannot. A stale sidecar
lacks tools added since, or misreads answers from a newer game, and the symptoms read as game
bugs.

So every result says when the sidecar is out of date, under `warning` on a success and after the
message on a failure:

| Warning | Means | Do |
| --- | --- | --- |
| "its sources changed ..., after the last build" or "a source file the last build used is gone" | A source file of the sidecar binary, as cargo's `ename_mcp.d` beside it lists them, changed after that build started, or is gone. | Run the build command the warning names (here `cargo build -p ename_mcp --target-dir target/ename-mcp-bin`, the same as `cargo mcp-build`), then ask the user for `/mcp reconnect`. |
| "the build on disk ... is not the one this process started from" | The binary changed after this process started. | Ask the user for `/mcp reconnect`. |

The first result after the condition starts or changes, and every failure, carry the full text.
Other results carry a short form. Either way, do what the warning says, and tell the user in your
reply what it asks of them, and why; if you may not build, ask the user to. Treat surprising
results from before the reconnect with suspicion.

`cargo game` and `cargo dev-build` build their own copies, in `target/ename-launch-bin` and
`target/debug`, so they neither clear nor raise the warning. On Windows the connected sidecar
locks its `.exe` and the build fails: the user disables the `game` server in `/mcp`, you wait for
them to confirm, then build, then the user enables it again. The warning says so there.

## Lifecycle

| Tool | CLI | Does |
| --- | --- | --- |
| `game_start` | `cargo game start` | Build and start the editor, or report the one running, and wait until it answers. |
| `game_restart` | `cargo game restart` | Build while the old game keeps running; on success stop it and start the new one. On Windows the old game is stopped before the build, because Windows will not let the build replace a running executable, so a failed build leaves nothing running. Params you leave out are the last launch's. |
| `game_stop` | `cargo game stop` | `game.exit` over BRP, then a kill after `grace_seconds` (default 5). |
| `game_status` | `cargo game status` | Report the state; `wait_seconds` waits out a build or startup. |

Launch params: `instance`, `port`, `env` (e.g. `{"AIRSHIP_DIAGNOSTICS": "1"}`), `features`,
`window_size`, `wait_seconds` (0 to 50, default 50). `window_size` sets the editor window's size in
physical pixels (`ENAME_WINDOW_SIZE`); with the same dock layout it gives the same view sizes, so
captures from two launches compare. A window manager that refuses the size wins. `settle: true` (`--settle`)
runs `wait_settled` once the game is ready, within the same wait, and says how it went in the
`note`; a game that does not settle in time still counts as started.

**States.** `stopped`, `building`, `build_failed` (with `errors` in `path:line:col: error[...]`
form), `starting`, `ready`, `exited` (with `exit` and, for an error code or a crash, `crash` with
the cause, the panic that killed it from the run record (message, location, thread and how many
panics the run had), and the last lines of the log), `external` and `port_in_use` (with `holder`). `ready`
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
| `world_overview` | The named entities as a tree, with position and a few tags each. Start here. |
| `world_query` | Find entities by name substring, component, or parent. `matched_types` says what a component filter matched. |
| `world_list_components` | The component types on one or more entities, without their values; `not_reflected` lists the ones only removal can touch. |
| `world_get_components` | The values of named components on one entity, by short name; `Type.field` reads one field. |
| `world_get_position` | Where an entity is: absolute metres, its grid, and the `CellCoord`, `Transform` and `GlobalTransform` behind them. |
| `registry_schema` | How to write a type: a one-line wire shape and an example value the game reads back. |
| `world_spawn_entity` | Create an entity with a name, components and a position. |
| `world_despawn_entity` | Delete an entity and its children. |
| `world_insert_component` / `world_remove_component` | Add or drop components. Removal works on unreflected components. |
| `world_mutate_component` | Set one field of one component. |
| `world_get_resources` / `world_mutate_resource` | Read and set resources (global state) the same way. |
| `world_reparent_entity` | Move an entity in the hierarchy. |
| `world_set_position` | Move an entity to an absolute world position. |
| `time_get` / `time_set` | Read the virtual clock; pause, resume, or step N frames. |
| `log_get_entries` | Recent tracing events, filtered by level, target and message. |
| `command_list` / `command_call` | The commands the game registers, and calling one by name. Always present. |
| `job_list` / `job_wait` / `job_cancel` | Follow, collect or end the work a command started. |
| one tool per game command | Present while the game runs; see "Commands and jobs". Today: |
| `editor_get_view` / `editor_set_view` | Which editor view is shown, Scene View or Game View; switch it as clicking its tab would. |
| `camera_look_at` / `camera_get_pose` | Place a camera with a position and a point, an entity or yaw/pitch in degrees, kept above the ground; read a pose back in the same shape. See "Placing the camera". |
| `viewpoint_list` / `viewpoint_goto` / `viewpoint_save` | Named camera poses in the project's `viewpoints.ron`. |
| `screenshot_capture` | Write the game window, the game camera or the editor camera to a PNG file. |
| `video_record` | Record the game window, the game camera or the editor camera to an `.mp4` or `.mkv` file. |
| `capture_viewpoints` | Capture several named viewpoints into one directory with a `manifest.json`, as one job. See "Capture sequences". |
| `capture_fly_through` | Move a camera along a path in N steps and capture each step as a still or a video frame. See "Capture sequences". |
| `wait_settled` | Wait until loading, pipeline compiles and streaming have finished and stayed finished. See "Waiting". |
| `wait_until` | Wait until one condition holds: frames, virtual seconds, a log line, a component or resource value, a position, a body at rest. |
| `wait_activity` | What is still loading or building, right now. |
| `debug_list` / `debug_get` | Debug switches (overlays, diagnostics logging, the camera's frozen origin, a game's own): category, value, allowed values, keys. See "Debug switches". |
| `debug_set` / `debug_reset` | Set debug switches by name, or restore their launch values. No restart, no key press. |
| `input_send` | Send keys, text, pointer moves, clicks, drags and scrolls into the game as real input, with no focus and no OS injector. See "Driving input". |
| `input_state` | The pointer, held keys and buttons, egui's interest and what picking hovers. |
| `perf_sample` | Measure frame, CPU and GPU time per camera and per render pass over a few windows, with run-to-run noise and machine load. See "Measuring performance". |
| `perf_compare` | Sample two settings interleaved (A B B A) and report each metric's difference against the noise. |

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
them. It returns 50 rows by default, positions rounded to millimetres; a cut answer adds
`name_counts`, the ten most common names among all matches, so 400 rows of `Water surface`
become one line. A component filter that is a type's short name matches only that type
(`FloatingOrigin` no longer matches `FloatingOriginCandidate`), and `matched_types` tells "no
entity has it" apart from "no such type".

**The core tools load upfront.** Claude Code defers MCP tools behind tool search by default. The
fourteen most used (`world_overview`, `world_query`, `world_get_components`,
`world_mutate_component`, `world_set_position`, `registry_schema`, `log_get_entries`,
`command_call`, `game_start`, `game_status`, `wait_settled`, `screenshot_capture`,
`editor_set_view`, `camera_look_at`) carry `_meta["anthropic/alwaysLoad"]`. A command opts in
with `CommandSpec::always_load()`; keep the set small, since each costs context every turn.

**Types go by short name.** Every tool that takes a component or resource type accepts the full
path, a path suffix (`grid::Grid`) or the short name (`Transform`). An exact full path wins, then
an exact short name, then a case-insensitive one, then a suffix. For an entity, its own
components are tried first. A name two types share fails with `ambiguous_type` and lists both;
an unknown one fails with `unknown_type` and up to five close paths. Results always give the
full path.

**Writing a type starts at `registry_schema`.** For each type it returns `shape`, one line on how
the value is written (`unit struct: write {}`, `newtype over Vec3: write the Vec3 value itself,
not [value]`, `one of "Dynamic" | "Static" | "Kinematic" (bare string)`), and `example`, a value
the game's serializer wrote and its deserializer read back. The upstream JSON schema describes
reflection structure and is wrong for newtypes and glam types, so it comes only with
`full_schema: true`. A name under `unregistered` is one nothing can read or write; one under
`not_reflected` can be listed and removed but not read or written.

**Small mistakes are fixed and reported.** A value that parses as sent is used as sent. One that
fails gets one pass of fixes with a single right answer: `null` to `{}` for a unit struct, `"3.0"`
to `3.0`, `"\"Hidden\""` to `"Hidden"`, `"field_only"` to `"FieldOnly"`, `[[0,1,0]]` to
`[0,1,0]` for a newtype, `{"x":..,"y":..,"z":..,"w":..}` to an array for a glam type. Each fix
comes back under `coerced`, so the next call can use the right form. `strict: true` turns this
off. Anything else fails, naming the deepest failing field, its shape and an example:
`` `DirectionalLight` is invalid at `shadow_depth_bias`: invalid type: string "abc", expected f32.
Expected: a number. Example: 0.0 ``.

**Reading a component means naming it.** `world_get_components` takes the types to read, since
an entity dumped whole costs more context than it pays back. `"Transform.rotation"` reads one
field, with the path syntax `world_mutate_component` uses. A value over 8 KB comes back as its
size and field names, to read by path. A type the entity does not have comes back under
`absent`; one that cannot be read (not reflected, or an asset handle) under `unreadable` with the
reason. `Transform` is relative to a grid cell; `world_get_position` gives the world position.

**Immutable components are replaced whole.** avian's `RigidBody` is immutable: setting a field
fails with `immutable_component`, and `world_mutate_component` with an empty `path` replaces the
whole value.

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
and the failure says how it died. The game keeps a run record per port (`brp-<port>.json` in
`$XDG_RUNTIME_DIR/ename`, or the temp dir on macOS and Windows), so this works for a game you did
not start too. The sentence is one of:

- it exited, with `AppExit::Success` (a closed window, `game_stop`) or `AppExit::Error(n)`;
- its process is running but not answering on the port: still starting, or hung;
- it panicked, with the message and `file:line`, and a link into
  [known-crashes.md](known-crashes.md) when the panic is a known one;
- it ended with neither: killed, or a native crash, with the last error this session saw.

With no record (a game older than the record, or none ever ran on the port) it says only that
the game is unreachable, and `game_status` says why. A call that times out on a live game gets
the "not answering" sentence.

## Noticing errors

A failure that shows only in the game's log is one nobody looks for: a shader that did not
compile still produces screenshots. So a result carries `log` when the game logged new errors or
warnings since the last result that said so:

```json
{"pid": "kqvzmbta",
 "log": {"new_errors": 1, "new_warnings": 0,
         "last_error": "bevy_render::render_resource::pipeline_cache: failed to process shader ...",
         "after_sequence": 4180},
 "view": "game"}
```

`log_get_entries` with `min_level: "WARN"` and that `after_sequence` lists exactly the new lines;
no `after_sequence` means read from the start of the run. `last_warning` replaces `last_error`
when only warnings are new. The first call after a start reports everything since the start, so
it doubles as "did the launch log anything". A failed result carries the same notice as a
sentence after its message. Panics, including those the game survives (a task's, a handler's),
are in the log as ERROR entries with target `panic`.

`log` on a lifecycle result (`game_start`, `game_status`, ...) is something else: the path of
`game.log`.

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

## Waiting

Never sleep before a capture or a measurement, and never poll in a shell loop: the game answers
both questions itself, as job tools bounded by `timeout_seconds` (default 30, at most 600) and
ended early by `job_cancel`. A tool waits up to `wait_seconds` (50) like any job; `job_wait`
picks up a longer wait.

**`wait_settled` asks "is the world quiet?".** Crates report work that is still pending through
settle probes: `assets` (`meshes_loading`, `materials_loading`, `world_roots_loading`),
`pipelines` (`pipelines_waiting`) and `stage` (`stages_loading`) in every build, plus whatever a
game adds. The wait succeeds once every probe has been idle for `stable_frames` consecutive frames
(default 5), because work often starts a frame after its cause. `probes` limits it to some; `[]`
makes it a plain count of `stable_frames` frames. Progress shows the busy counters draining:
`{"frames": 41, "stable_for": 0, "busy": {"pipelines": {"pipelines_waiting": 3}}}`. A timeout fails
`not_settled` and names what was still busy. An unknown probe name is `unknown_probe`, listing the
registered ones, and an out-of-range `stable_frames` or `timeout_seconds` is `invalid_params`.
`wait_activity` reports the same counters at once, without waiting.

Settling covers loading, not motion: a flying airship never stops, so moving bodies are not a
probe. And it sees only work the game already knows about: the asset watcher reports an edit
300 to 400 ms late, as the log line `Reloaded <asset path>`, and a settle before that has
nothing to wait for. So for a file edit:

1. Before the edit, read the newest log `sequence`: `log_get_entries` with `limit: 1`.
2. Edit the file.
3. Wait for its reload line after that sequence, then settle:

```json
{"condition": {"log": {"message_contains": "Reloaded alias://examples::backdrop", "after_sequence": 9120}}}
```

The asset path is the one the game loaded it by: an aliased file such as
`examples-stage/backdrop.sky.ron` reloads as `alias://examples::backdrop`. When unsure, match
`"Reloaded"` alone. The `after_sequence` matters: by default the wait only looks at lines logged
after it starts, and the reload line often arrives before the next call does.

A file that fails to load (a syntax error) logs an error naming the asset path, and no
`Reloaded` line. To catch both, match the asset path alone (`"examples::backdrop"`) and check the
matched entry's `level`, or pass a short `timeout_seconds` such as 10 so a broken file does not
hold the wait for 30 s.

**`wait_until` asks "has this happened yet?".** `condition` has exactly one key:

| `condition` | Met when |
| --- | --- |
| `{"frames": 30}` | 30 real frames have run. Works while paused. |
| `{"virtual_seconds": 2.0}` | Virtual time advanced 2 s. Needs the world running. |
| `{"log": {"message_contains": "baked", "target_contains": "...", "min_level": "info", "after_sequence": 9120}}` | A log entry newer than `after_sequence` (default: the newest when the wait starts) matches, with `log_get_entries`' filters. |
| `{"component": {"name": "Crate", "component": "LinearVelocity", "path": ".0.y", "op": "gt", "value": -0.1}}` | The component's value, as `world_get_components` shows it, passes `op`. |
| `{"resource": {"resource": "...", "path": "...", "op": "eq", "value": ...}}` | The same, for a resource. |
| `{"position": {"name": "Crate", "axis": "y", "op": "lt", "value": 2.0}}` | Absolute metres, Y up. |
| `{"at_rest": {"name": "Crate", "speed_below": 0.05, "hold_seconds": 0.25}}` | A dynamic rigid body sleeps, or stays slower than `speed_below` m/s (and 0.05 rad/s) for `hold_seconds` of virtual time. Needs the world running. |

`op` is `eq`, `ne` (JSON equality, numbers within 1e-6), `lt`, `le`, `gt`, `ge`, `length_lt`,
`length_gt` (an array such as a `Vec3`), `changed` (differs from the value at the start, no
`value`), `exists` or `absent` (no `path`, no `value`). `position` takes the four comparisons.
Types are full paths or a unique short name. `Transform.translation` (also as `#0`), a whole
`Transform` compared by value, and `CellCoord` are refused, as `world_mutate_component` refuses
them; use `position`. A whole `Transform` may still be watched with `exists`, `absent` or
`changed`. `value: null` is a value, matching an `Option` field that is `None`. `hold_frames`
makes a predicate hold for that many frames. The result carries what met it under `met`, plus
`frames`, `seconds`, `virtual_seconds`, `frame` and `paused`. Failures name their kind:
`timed_out` (with the last value), `entity_not_found`, `entity_gone`, `ambiguous_entity`,
`component_absent`, `not_registered`, `bad_path`, `not_comparable`, `not_on_grid`,
`level_not_captured`, `invalid_params`, `world_paused`.

**The paused world.** Frames keep running while time is paused, so `frames`, `log` and
`wait_settled` progress in a paused world. A condition that needs virtual time fails
`world_paused` at once when the world is paused with no step in flight. `component`, `resource`
and `position` cannot tell whether their value needs time; they keep waiting and report
`paused: true`. A wait sees each frame of a `time_set step`. Waits never change time, except
`time: "run"` on `wait_until`: it resumes the world when the wait starts and pauses it in the
frame the wait ends, however it ends (met, timed out, cancelled). With two such waits, the first
to end pauses the world.

The deterministic capture loop is `time_set pause`, pose the camera, then `screenshot_capture`
with `settle: {}`.

**Physics.** The landing test, one call with no race between resuming and waiting:

1. `time_set pause`.
2. `world_set_position` the body above the ground (and set its velocity if needed).
3. `wait_until` `{"condition": {"at_rest": {"name": "Crate"}}, "time": "run"}`. The world runs,
   the body falls and comes to rest, and the world pauses in that frame.
4. `world_get_position` and `world_get_components` read the rest state.

"Falls below" is `position` with `op: "lt"`.

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

## Placing the camera

`camera_look_at` places a camera in one call and returns the pose read back from it:

```json
{"position": [90, 1505, 30], "look_at_entity": {"name": "examples::airship"}}
{"position": [0, 2400, 1400], "yaw": 0, "pitch": -40}
{"look_at": [0, 1200, 0]}
{"advance": 50}
```

**The pose.** `position` is absolute metres, Y up. Orientation is one of `look_at` (a point),
`look_at_entity` (`{"name"}` or `{"entity"}`, faced at its absolute position), `yaw`/`pitch`
(degrees; either alone keeps the other) or `rotation` (a quaternion copied from a `Transform`; its
roll is dropped and a note says so). None keeps the current orientation. Yaw 0 looks along -Z and
yaw 90 along +X; pitch is positive up. There is no roll. The editor camera holds at most ±89°
pitch, so -90 lands at -89 with a note. `advance` then moves the camera that many metres along its
view. Two orientation forms, a non-finite value or a point equal to the position fail
`invalid_pose`.

**Which camera.** `camera` is `editor_camera` or `game_camera`; left out, it is the one the shown
view renders, which is what the next `screenshot_capture` sees. `active` in the result says
whether that camera renders now; when false a note names the `editor_set_view` call. The game
camera needs exactly one `MainCamera` (`no_game_camera` otherwise). A game camera that follows a
virtual camera through an enabled `CameraDriver` fails `camera_driven`, because the driver would
overwrite the pose next frame; the hint names the virtual camera to move instead. The example
Main Camera hangs under `examples::airship`; it is placed at the absolute pose all the same.

**Motion is cleared.** Placing a camera zeroes its leftover fly velocity and moves the orbit pivot
to what it looks at, so an Alt+RMB orbit afterwards pivots there. `world_set_position` clears
nothing: a camera flown a moment ago keeps drifting for half a second, and while the world is
paused (`time_set pause`) it drifts forever, because the velocity decay is scaled by virtual time.

**The ground guard.** `ground` picks which ground keeps the camera clear of it:

| `ground` | Probes asked |
| --- | --- |
| `"none"` | None: no wait, no lift, `ground` and `center_hit` are `null` and not computed. A deliberate shot from below. |
| `"colliders"` | The built-in probe over static colliders, plus any probe the game registered. |
| `"meshes"` | The built-in probe over marked render meshes, plus any probe the game registered. |
| `"both"` | Both built-in probes, plus the game's. |

Left out, `ground` and `min_clearance` come from the game's `CameraGroundGuard` resource
(`ename_engine::camera::ground`), which defaults to `both` and 2 m. In a game without colliders
or marked meshes the probes find no ground, so nothing is lifted or waited for. Pass
`"ground": "none"` for a deliberate shot from below. The result's `ground` says which mode was
used (`mode`) and which probe gave the ground (`from`: `"colliders"`, `"meshes"` or `"custom"`).
The probes:

- The colliders probe answers from static colliders. With no physics in the app it answers
  nothing.
- The mesh probe answers from render meshes, for terrain that is drawn but has no collider. It
  sees only meshes marked `ename_engine::camera::ground::GroundSurface`, on the mesh entity or any
  ancestor (a glTF scene root covers its meshes). Everything else (sky, water, foliage, gizmos,
  characters) is ignored. In `both`, a marked mesh with a `Collider` on itself or an ancestor is
  left to the colliders probe, which also sees the unmarked colliders around it, such as a
  tunnel's ceiling under the terrain. In `meshes` the mesh probe answers for it.
  - The mesh must keep its data on the CPU (`RenderAssetUsages::MAIN_WORLD`). A marked mesh
    without it is ignored, with one warning naming the entity.
  - Terrain displaced on the GPU (in a shader) is not seen.
  - While a marked mesh's asset is still loading, the ground counts as not loaded.

The example game marks its map. Its map mesh is also its collider, so in `both` the colliders
probe answers for it, and in `meshes` the mesh probe does. When several probes are asked, the highest ground and the lowest ceiling win, and
`center_hit` is the nearest hit. With a probe:

- A camera less than `min_clearance` (default 2 m) above the ground is lifted to that clearance,
  and `ground.lifted_by` says by how much. For the physics probe the ground is the first static
  surface straight down from the camera, so a camera in a room stays in the room and one on an
  upper floor keeps to that floor.
- A lift never goes through a ceiling, the first static surface straight up: it stops 0.1 m
  below it, and a note says so. With less than 0.1 m free above the ground, the camera goes onto
  the ground and no further.
- A camera inside or exactly on a convex solid (a cuboid wall or slab, a sphere, a capsule, a
  cylinder, a cone, a convex hull, a half-space, or a convex part of a compound) is lifted out of
  it. Its ground is where a ray straight up leaves that part, or the top of a flush stack of
  such solids.
- Under the terrain: a game marks its terrain with `GroundSurface` (colliders or their bodies
  for the physics probe, meshes or their ancestors for the mesh probe). When the first surface
  straight up from the camera is marked terrain, the camera is under it and is lifted onto it.
  Unmarked geometry is a plain surface: a camera under a roof, a deck or an upper floor stays
  under it.
- Not detected: a camera inside a closed triangle mesh or a non-convex solid (it is treated as
  being in free space between that mesh's faces), and a camera under unmarked terrain (it stays
  under it; the note says a surface is above).
- "No ground here" (space, a void, off the map) is not waited for: `ground` is `null` with a short
  note. When a surface is above the camera but none below, the note says the camera may be under
  the ground.
- Ground a probe has not loaded yet is polled every frame for up to 10 s, then the call goes on.
- `settle` takes `wait_settled`'s params and settles after the lift; the guard then checks again
  (finer streamed ground can be higher) for up to 3 rounds. Without `settle`, `ground` carries
  `"provisional": true`.

`center_hit` is where the view centre meets a surface (`null` above the horizon), so "facing the
ground" or "facing the sky" shows without a screenshot. The job finishes once a frame has run with
the final pose, so a screenshot right after shows it. A newer placement of the same camera while
one still waits wins: the older job fails `superseded` and writes nothing more. `job_cancel` stops
only the wait; the pose is already written. Positions in results are rounded to 0.01 m and angles
to 0.01°.

`camera_get_pose` returns the same shape; its `position`, `yaw` and `pitch` paste into
`camera_look_at` as they are.

**Viewpoints.** Named poses live in `viewpoints.ron` at the workspace root, versioned in git and
read on every call, so a hand edit takes effect without a restart. `viewpoint_list` shows them,
`viewpoint_goto {"name": ...}` places a camera there as `camera_look_at` does (same
`camera`, `ground`, `min_clearance`, `settle`), and `viewpoint_save {"name", "note"}` adds
the current pose at the end of the file, or replaces one in place with `replace: true`
(`viewpoint_exists` otherwise). Names are ASCII letters, digits, space, `.`, `_` and `-`. A save
rewrites the file and drops comments, so describe a viewpoint in its `note`. A viewpoint can also
carry facets, state a game crate registers (a sun direction, say): `include` names the ones to
save, and `goto` applies them unless `facets: false`. The example game registers none. A file of
a newer version fails `unsupported_version`, a parse error names the line and column.

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

`settle` takes `wait_settled`'s params (`{}` for the defaults) and settles first: the capture is
requested in the frame the world settled, never earlier. While it settles, progress says
`"phase": "settling"`. If it does not settle in time the call fails `not_settled` and nothing is
written. Use it instead of a sleep before every capture.

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

`settle`, as on `screenshot_capture`, settles before recording starts. The encoder needs a few
frames to start after that, which is accepted: settled work does not restart on its own.
Cancelling during `"phase": "settling"` ends the job `cancelled` with no file; a timeout fails
`not_settled`.

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

## Capture sequences

Two commands capture several poses in one job, for A/B comparisons and for bugs that only show in
motion. The game sequences the steps itself: each pose is placed a frame before it is captured,
and the next waits for the capture, which a loop of tool calls cannot guarantee.

`capture_viewpoints` takes `viewpoints` (names from `viewpoint_list`), `dir` and `source`
(`editor_camera` or `game_camera`; a window has no pose to move). It writes `<name>.png` per
viewpoint (characters outside `[A-Za-z0-9._-]` become `_`) and `manifest.json`. `facets` (default
true) applies each viewpoint's facets, which are not put back. `ground` is the guard
`viewpoint_goto` uses.

`capture_fly_through` takes `source`, `steps` (1 to 10000, both ends included), a `path`, and
`record`:

| `path` | Moves |
| --- | --- |
| `{"from": vp, "to": vp}` | In a straight line; yaw turns the short way, pitch linearly. |
| `{"from": vp, "along": "forward", "distance": 400}` | Along the camera's `forward` or `right`, the world's `up`, or an `[x, y, z]` direction, orientation fixed. |
| `{"poses": [pose, pose, ...]}` | Through the poses, steps spaced evenly by distance. |

A `vp` is a viewpoint name or an inline pose `{"position": [x, y, z], "yaw": deg, "pitch": deg}`.
`record` is `{"stills": {"dir": ...}}` (`0.png` ... zero-padded to the last index) or
`{"video": {"path": "fly.mp4", "frame_rate": 30}}`, one frame per step, with the manifest beside
it as `fly.mp4.manifest.json`. Pose `i` is video frame `i` at any frame rate; a dropped frame shows
as a jump, and `dropped_frames` counts them. `ground` defaults to `none`, so the camera follows the
path exactly.

Both commands, for the run:

- show the source's view, and switch back after;
- pause virtual time (`freeze_time`, default true), so water and wind do not change between
  steps, and restore it. Two runs still freeze at different clock ages: `virtual_time` in the
  manifest says when they differ;
- hide the editor's debug overlays, transform gizmo and pick marker (`overlays: false`); egui
  drawn over the Scene View (a tooltip, a menu) still shows;
- disable a `CameraDriver` on the game camera;
- put the camera back where it was (`restore`, default true).

`settle` is `wait_settled`'s params, before each still, or once before a video starts. A step that
does not settle in time is not captured and is listed under `failed`, with what was still busy;
the run goes on. Left out, nothing waits, so streaming shows as a moving camera would see it. A
probe whose work runs on virtual time cannot finish while the world is paused; pass
`freeze_time: false` if settling times out with nothing busy.

One sequence runs at a time: a second one, `editor_set_view` and `viewpoint_goto` are `busy`
until it ends. `time_set` is not refused, but changing time during a run is not supported. A
`camera_look_at` on the same camera ends the run `superseded`, with the frames that landed in the
manifest, and leaves the camera where that call put it. `job_cancel` waits for a capture in
flight, keeps a partial video, writes the manifest and restores.

`dir` must be absent or empty. `replace: true` deletes the files the old `manifest.json` lists,
and the manifest, and nothing else; a file the new run would write that the old manifest does not
list is `file_exists`. The manifest is authoritative for its directory:

```json
{"run": "kqvzmbta", "source": "editor_camera", "width": 815, "height": 489, "virtual_time": 9.91,
 "frames": [{"file": "start.png", "viewpoint": "start", "position": [10, 5, 0], "yaw": 0,
             "pitch": 0, "frame": 585, "width": 815, "height": 489},
            {"viewpoint": "shore", "position": [50, 8, 20], "yaw": 90, "pitch": 0, "frame": 0,
             "error": "not_settled", "busy": {"assets": {"meshes_loading": 3}}}]}
```

The capture size is the size of the view's tab in physical pixels, which depends on the window
size, the dock layout and the display scale. Every result and manifest records it, and
`size_changed: true` says a frame differs from the first (a splitter dragged mid-run). Comparing
two runs is only valid at the same size: check both manifests first, and launch with the same
`window_size`.

### Visual check recipe

Image work is left to ImageMagick, PIL or ffmpeg on the files.

1. `capture_viewpoints` into `scratch/shots/base` (`settle: {}`).
2. Make the change, `game_restart` if it needs one, and capture into `scratch/shots/new`.
3. Check that `width` and `height` match in both manifests.
4. Per pair: `magick compare -metric AE -fuzz 2% base/x.png new/x.png diff-x.png` prints the
   count of changed pixels. Read the diff image of the worst pair only.
5. To know the noise floor, capture the baseline twice, once after a `game_restart`.

For motion: `magick montage scratch/shots/fly/*.png -tile 9x -geometry 300x+2+2 sheet.png` makes
a contact sheet, and `magick compare -metric AE` over consecutive stills finds a pop as a spike.

## Measuring performance

`perf_sample` measures the running editor and answers in one JSON object. Do not scrape
`diagnostics.log` lines or write sleep-and-grep scripts for this. Why it works the way it does is
in [design/perf-sampling.md](design/perf-sampling.md).

```
perf_sample { "duration": {"seconds": 5}, "repeats": 3, "settle": {}, "skip_settle": false,
              "warmup_frames": 30, "uncapped": false, "detail": "summary" }
```

Every param is optional; the values above are the defaults. `duration` is one window, written as
in `video_record` (`frames`, `seconds` or `timecode`, at most 10 minutes). `repeats` (1 to 20)
windows run back to back. `settle` takes `wait_settled`'s params; then the frame time must hold
steady (the median of the last 30 frames within 5 % of the 30 before, for at most 5 s), then
`warmup_frames` are dropped. `skip_settle: true` skips both. `detail: "full"` lists every pass and
every diagnostic instead of the top 10 passes by GPU time and the registered counters.

It is a job. Settle plus three 5 s windows can pass the 50 s tool wait; call `job_wait` again.
Progress says `phase` (`settling`, `sampling`, `waiting_gpu`), `run` and `of`, and while settling
the probes still `busy`. `job_cancel` returns the runs finished so far, or `cancelled` with none.
It fails `perf_busy` while another `perf.sample` or `perf.compare` runs, and `no_frames` when no
frame of a window rendered a camera or the window is 0x0 (a minimized window).

### The result

Times are milliseconds, rounded to 0.01. A series reports `median`, `p95` and `noise`, plus `max`
for `frame_ms`. `median` is the median of the run medians and `noise` is their range, `null` with
one run. A difference smaller than `noise` means nothing.

| Field | Is |
| --- | --- |
| `frame_ms` | The main loop's interval (`Time<Real>::delta`). With pipelined rendering this is not the cost of one frame's render. |
| `cpu_main_ms` | Wall time of the main world's schedules, `First` to `Last`. |
| `render_wall_ms` | Render-world wall time, first to last render set. It includes waiting for the swapchain and for present, so it is not CPU work. |
| `gpu_ms` | GPU time per frame, every camera, egui and post-processing included. `"unavailable"` without the `TIMESTAMP_QUERY` feature. |
| `gpu_unattributed_ms` | Per frame, `gpu_ms` minus the sum of its views. Near 0 in `game`. |
| `views` | Per label: `count` (cameras under it in one frame), `size` (largest physical viewport size, which for the Scene View camera is its tab, not the window), `gpu_ms` and `cpu_ms` (wall time of its render schedule), each `median` and `noise`. |
| `passes` | Bevy's render pass spans: `path`, `runs_per_frame`, `gpu_ms`, `cpu_ms`. `"unavailable"` without `RenderDiagnosticsPlugin`. |
| `counters` | `entity_count` and every registered counter. A counter that changed during the sample reads `{median, min, max}`: it was not settled. |
| `hitches` | `frame_ms` frames above twice their window's median: `count`, `threshold_ms`, `worst_ms`, `at_seconds`. Statistics leave them out. `hitch_counts` gives the other series' counts. |
| `bound` | What the frame time waits on, see below. |
| `load` | Other processes over the busiest window: `loadavg_1m`, `cpus`, `other_cpu_cores`, `top` (above 0.25 cores, children of the game marked `child`), `gpu_others_ms`, `gpu_others_measured_ms` and `gpu_top`. |
| `window`, `focused`, `occluded`, `present_mode`, `refresh_ms` | The primary window's physical size and state, and the monitor's refresh interval. |
| `gpu_frames_missing`, `gpu_frames_skipped` | Frames whose GPU times never came back, or were not recorded because every timer slot was in flight. |
| `reruns` | View changes (window size, view size, camera set or label) that dropped the runs so far and restarted the sample, during a window or between two. Every run in the result shares one layout. |
| `warnings` | `{kind, message}` each; see below. |

Other processes' GPU time comes from the kernel's DRM fdinfo on Linux. On Windows and macOS
`gpu_others_ms` is `null`, and `loadavg_1m` is `null` on Windows. On Linux it is also `null` when
some process's driver reports no usage (its `gpu_top` entry has `gpu_ms: null`); then
`gpu_others_measured_ms` holds the measured processes' time, a lower bound. A second editor left
running shows up in `gpu_top`.

`bound` is a heuristic, first match wins: `vsync` when `vsync_capped` is warned; `gpu` when the
`gpu_ms` median is at least 90 % of the frame median; `cpu_main` when `cpu_main_ms` is; `render`
when `render_wall_ms` is and `gpu_ms` is under half the frame; else `unclear`. Without GPU times
only `cpu_main` can be named.

| Warning | Means |
| --- | --- |
| `vsync_capped` | The frame median sits within 3 % of a multiple of the refresh interval, under any present mode (a compositor can cap a window too). Frame time cannot show a change; read the CPU and GPU series, or pass `uncapped: true`. |
| `machine_busy` | In some window, other processes used over one core on average, or GPU time above 5 % of the window. |
| `not_settled` | Settling timed out; sampled anyway, with what was busy. |
| `not_steady` | The frame time was still drifting after 5 s. |
| `hitchy` | Over 5 % of frames were hitches. The hitches are the finding. |
| `unstable_view` | The view changed again after a restart, or (in `perf_compare`) an arm's steps or the two arms ran at different sizes; the runs are kept anyway. |
| `window_occluded` | The window was covered or minimized; the compositor may throttle it. |
| `stats_panel_on` | The statistics panel was on, so its cost is in the numbers. |
| `video_recording` | A `video_record` ran during the sample, so readback and encoder cost is in the numbers. |
| `views_truncated` | Over 64 cameras in a frame; the rest were not timed. |
| `no_render_diagnostics` | No `RenderDiagnosticsPlugin` in this build; `passes` is unavailable. |
| `timestamp_budget` | Bevy ran out of its 256 timestamp queries in a frame; some passes lack `gpu_ms`. |
| `pass_gpu_unsupported` | The device cannot time inside passes; passes report `cpu_ms` only. |

`uncapped: true` switches the window to `AutoNoVsync` for the sample and back after, also on
cancel. It changes what the user sees, so it is off by default. The swapchain rebuild is a hitch
that settling absorbs. A compositor may still cap the window, and the warning then says so.

### Views, passes and how to measure

A camera's label is its `PerfViewLabel` if it has one, else `main_camera` for the camera
`MainCameraFilter` selects, else its `Name`, else `camera <order>`. Cameras that share a label are
summed per frame. A view's time covers its whole render schedule, shadow passes included, which
Bevy's own pass spans do not time.

A pass's time is summed per frame over every camera that ran it; `runs_per_frame` says how many.
Bevy logs a moving average that only the first camera of a frame moves, so the summed number is
the real cost. Passes cannot be split per camera.

- **Measure in the Game View** (`editor_set_view` `game`). The Scene View's numbers include the
  `Editor Camera` and differ from what the game renders. The editor deactivates the hidden view's
  camera, so only one of `Editor Camera` and `main_camera` appears.
- **Compare only samples with the same `window` and view `size`s.** Nothing pins the render size;
  the Game View renders at the tab's size. A change of a size, the camera set or a label
  mid-window drops that run and samples it once more.
- On a vsync-capped window frame time stays at the refresh interval. Compare `gpu_ms`,
  `cpu_main_ms` and the views.

A game crate hooks in through `ename_engine::perf`. Put `PerfViewLabel("mirror")` on cameras it
spawns in groups, such as cubemap faces, so they report as one view; it is reflected, so an agent
can insert one with `world_insert_component`. Call `app.register_perf_counter(path)` for a
diagnostic it already records, and the value appears in `counters` and as a panel row.

`gpu_ms` runs from the end of `RenderGraphSystems::Begin` to just before `Submit`. A game crate
that adds systems to the root `RenderGraph` schedule must put them in `RenderGraphSystems::Render`,
or their GPU work may fall outside the total. Work submitted outside the graph (screenshot and
video readback copies, a `queue.submit` in a prepare system) is not in it either.

### Comparing two settings

`perf_compare` runs two arms, each a list of command calls, and samples after each:

```
perf_compare {
  "a": [{"command": "debug.set", "params": {"values": {"overlay.physics": false}}}],
  "b": [{"command": "debug.set", "params": {"values": {"overlay.physics": true}}}],
  "restore": [{"command": "debug.reset", "params": {"names": ["overlay.physics"]}}],
  "rounds": 2,
  "sample": {"duration": {"seconds": 5}}
}
```

Each round runs `A B B A`, so linear drift (warming caches, a scene that gets slower after
visiting other views) cancels; `order` lists what ran. `rounds` is 1 to 10, default 2. `sample`
is `perf_sample`'s params, but each step samples one window unless `repeats` says otherwise. An
arm's steps are its runs, so with 2 rounds each arm has 4 and `noise` is their range. Job
commands in an arm are awaited before sampling. Each step settles again first.

The result has both arms' sample results under `a` and `b`, and `diff` with `frame_ms`,
`cpu_main_ms`, `render_wall_ms`, `gpu_ms`, `views` by label (GPU time, or CPU time without
timestamps) and arm A's top 10 `passes`. Each entry has `a`, `b`, `diff` (b minus a; negative is
faster), `noise` (the larger arm's) and `verdict`: `b_faster`, `b_slower` or `within_noise` when
`|diff| <= noise`. Verdicts use the rounded numbers shown, so a 0.00 difference is never called
faster.

`restore` runs once at the end, also when a step fails or the job is cancelled. Its job commands
are started, not awaited, and a failed one adds a `restore_failed` warning. A failing arm step
stops the job and the error names the step. Cancelling also cancels an arm's job still running
and waits for it to end (progress `phase: "cancelling"`) before `restore` runs; the compare stays
running, and `perf_busy` holds, until then.

### The statistics panel

The debug switch `overlay.stats` (key `F3` in editor and release builds, View > Statistics in
the editor) shows a panel in the top right corner of each view: the Scene View, the Game View, and
the window in a release build. One panel per host camera: the `MainCameraFilter` camera, plus any
camera with `StatsPanelHost` (the editor puts it on `Editor Camera`). The panel of a hidden view
draws nothing.

| Row | Shows |
| --- | --- |
| FPS, Frame | `1000 / frame` and `frame_ms` |
| CPU | `cpu_main_ms`, the main schedule's wall time |
| GPU | `gpu_ms`, the whole frame's GPU time |
| Entities | Entity count |
| View | This view's physical size |

The panel stays small on purpose; `perf_sample` reports the rest (render wall time, per-view
times, passes, counters).

Values are the mean of the last 0.5 s, refreshed four times a second. A row whose value has no
source, such as the GPU row without `TIMESTAMP_QUERY`, is hidden. Nothing like Unity's batches,
triangles or SetPass calls is measured here, so none is shown. Clicks pass through the panel.

The panel is real UI, so `screenshot_capture` and `video_record` show it. Capture sequences with
`overlays: false` hide it. It costs a little, and samples taken while it is on carry
`stats_panel_on`.

The panel is drawn with `bevy::feathers`. `EnginePlugins` adds `FeathersCorePlugin` as a group
member, and `PerfPlugin` inserts the dark theme only when `UiTheme` is empty; its colours are the theme tokens
`ename.stats.bg` and `ename.stats.header.bg`. Consequences for game code:

- A game that wants feathers controls adds `TabNavigationPlugin` itself. Adding `FeathersPlugins`
  after `EnginePlugins` panics on the duplicate plugin.
- feathers resets each window's `CursorIcon` to `DefaultCursor` every frame while no feathers
  widget is hovered, so set the idle cursor through `DefaultCursor`, not `CursorIcon`. The editor
  copies egui's cursor into it for this reason.
- `F1` and `F2` are taken by `bevy/dev`'s render debug overlay.

## Debug switches

A debug switch is a named toggle or mode for debugging, performance analysis or modding:
`overlay.physics`, `overlay.grids`, `overlay.stats` (the statistics panel, see "Measuring
performance"), `camera.freeze_origin`, `diagnostics.log`, and whatever a game crate registers. Set them by name; never with ydotool, a key press or `world_mutate_resources`.

- `debug_list` returns every switch with its `category`, `value`, `allowed` values,
  `launch_value`, key chords per build (`keys.editor`, `keys.release`) and description.
  `{"category": "overlay"}` narrows it; `categories` always lists them all.
- `debug_get {"names": [...]}` reads some, in the order given.
- `debug_set {"values": {"overlay.physics": true, "diagnostics.log": "frame_times"}}` checks
  every entry before writing any and returns what `changed`. Bools are JSON booleans; modes are
  snake_case strings from `allowed`.
- `debug_reset` restores launch values, all of them or `{"names": [...]}`.

A switch whose `value` is `null` does not apply now (`camera.freeze_origin` with no grid);
setting it fails `switch_unavailable`. Every change, from a command, the View menu or a key, logs
one line: `log_get_entries` with `message_contains: "debug switch"` shows them.

`diagnostics.log` logs Bevy diagnostics every 5 s: `frame_times` (fps, frame time, frame count)
or `all` (also entity count and, in the editor, render pass CPU and GPU timings).

## Driving input

`input_send` writes the same messages `bevy_winit` writes for real input, inside the game, so
`bevy_input`, egui, picking, selection, the editor camera and gameplay all react as to a real
device. It needs no window focus, never moves the user's pointer, and never raises the window.
Never use ydotool, `kdotool windowactivate` or another OS injector on the game window.

```
input_send { "steps": [
  {"action": "click", "x": 415, "y": 100, "in": "editor_camera"},
  {"action": "key", "key": "KeyF"},
  {"action": "wait", "frames": 30},
  {"action": "leave"}
]}
```

- Steps: `key` (with `modifiers`: `shift`, `ctrl`, `alt`, `super`), `key_down` / `key_up`,
  `text`, `move`, `click` (`button`, `count: 2` for a double click), `button_down` /
  `button_up`, `drag` (`from`, `to`, `frames`), `scroll` (`lines`, positive up), `motion` (raw
  mouse motion for mouse-look), `leave`, `wait` (`frames` or `seconds`). One frame per event.
- Keys are Bevy `KeyCode` names: `F10`, `KeyW`, `ShiftLeft`, `Equal`, `Space`.
- Points are pixels of a full-size screenshot: `window` (the default), `editor_camera` or
  `game_camera` via `"in"`. Pick a point from a capture of that source and pass it as is; the
  mapping to the window, egui zoom and scale factor included, is the game's job. A cropped or
  scaled capture must be mapped back to full size first.
- The script is checked before it starts (`unknown_key`, `out_of_source`, `camera_inactive`).
  If the view moves or resizes while it runs, the step fails `layout_changed` rather than
  clicking elsewhere.
- Keys and buttons the script pressed are always released, whether it ends (a frame after its
  last step), fails, times out (`timeout_seconds`, default its waits and frames at 20 fps plus 10 s, at most 3600) or
  is cancelled. Buttons are released where the script last put the pointer. If the window loses
  keyboard focus mid-script, the script stops with `focus_lost`. One step takes at most 100000
  frames or 3600 s. In `text`, `\n` and `\t` are Enter and Tab. Hold with `key_down`, `wait`,
  `key_up` in one script; holds never span calls. After `job_cancel`, `job_wait` for it before
  reading `input_state`: the release lands in the next frame.
- The result's `real_input` counts the user's own pointer and keyboard messages during the
  script. Non-zero means the run may have been disturbed. `already_held` lists keys that were
  already down, so pressing them fired nothing. A key counts as down when its twin is (the
  user's ShiftRight makes a scripted ShiftLeft a no-op), and a `text` character whose key is
  held is skipped. Twins pressed by both the user and a script share one logical state, as with
  two real keyboards: whichever releases first releases it for Bevy's `ButtonInput<Key>`, and
  egui, which falls back to the physical key, may see a release early.
- One script at a time (`input_busy`). The transform gizmo is off while a script runs.
- Send `leave` before a screenshot: it moves the pointer out, clearing hover highlights.
- Debug keys do not fire while egui has the keyboard; use `debug_set` for debug switches anyway.

`input_state` reads the window (`size`, `scale_factor`, `focused`), the mouse pointer
(`position` in window pixels and `over`: `scene_view`, `game_view`, `egui` or `outside`), held
keys and buttons, egui's `wants_pointer` / `wants_keyboard` / `popup_open`, and `picking`: what
each pointer hovers (entity, name, depth). The window itself always appears there as a depth-0
hit with no name. Use `world_get_position` on a hovered entity for where it is.

What cannot be driven:

- Transform gizmo drags and `bevy_ui` `Interaction`: both read `Window::cursor_position()`,
  which injection never writes (writing it would warp the user's pointer).
- OS file dialogs (File > Open): another process. Use the stage commands.
- egui panel contents cannot be read back as data: read them from screenshots.

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
