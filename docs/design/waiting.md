# Waiting for the running game

Part of the [design record](../design.md). How an agent uses it is in
[../agent-tools.md](../agent-tools.md#waiting).

Agents used to wait with blind sleeps before captures, `until grep` loops on log lines and marker
files around physics. The harness blocks leading sleeps and kills long loops, and a fixed sleep
is either too short (an unsettled capture) or too long. `ename_engine::wait` answers two
questions as job commands: "is the world quiet?" (`wait.settled`) and "has this happened yet?"
(`wait.until`), both bounded by a timeout and cancellable. Capture commands can settle first.

## Decisions

1. **Probes are registered read-only systems that return named counters.** A crate calls
   `register_settle_probe(name, system)`; the system returns an `Activity` such as
   `tiles_loading: 3`. The crate that owns the work writes the probe, because only it knows what
   "pending" means. Counters are named rather than a bool, so a timeout says what is still busy
   and progress shows it draining. A system rather than a trait object, so a probe takes queries
   and resources the normal way and gets Bevy's access checks. The engine ships `assets`,
   `pipelines` and `stage`; games add their own.
2. **Probes run only while someone waits.** Sampling happens in `Last`, in
   `WaitSystems::Sample`, under a run condition (some `SettleWaiter` exists). A build where nobody
   waits pays nothing. `wait.activity` runs them once on demand.
3. **"Settled" means every probe reports zero for `stable_frames` consecutive frames** (default
   5). Work often starts a frame after its cause: a camera move set over BRP is seen by LOD
   selection in the next `Update`, a pipeline is queued a frame behind in the render world, and a
   finished task is polled a frame later. Frames are real frames, which keep running while paused.
4. **Settle covers loading, not motion.** Probes report work that finishes. A flying airship or an
   ocean never stops, and a settle that can never succeed is useless; motion is a `wait.until`
   condition on a named entity (`at_rest`). A settle sees only work the engine knows about: a file
   edit is invisible until the asset watcher reports it (300 ms debounce), so the agent reads the
   newest log sequence before the edit, then waits for the `Reloaded <path>` line after that
   sequence, then settles. The sequence is needed because the line usually lands before the
   agent's next call, and a log wait by default looks only at lines newer than its start. A
   minimum settle time covering the debounce was rejected: it slows every settle to cover one
   case.
5. **`wait.until` takes exactly one condition as a one-key object**, like `video.record`'s
   `duration`, so the shape says the kind. No `all`/`any` combinators: an agent chains two calls,
   and one condition per call keeps the result and the timeout message unambiguous.
6. **Predicates run in the game on reflected values serialized to the JSON `world_get_components`
   returns.** One comparison path for every type, and the `value` an agent writes has the shape it
   already reads. Paths use `world_mutate_component`'s syntax.
7. **Entities are addressed by name or id inside the game.** A wait re-resolves its entity every
   frame without the sidecar, so the sidecar's selector rules have an engine twin,
   `ename_engine::command::EntityTarget`: exactly one of the two, exact names, an ambiguous name is
   an error listing the candidates.
8. **Absolute positions live in the engine** (`bigspace::grid::{absolute_position,
   set_absolute_position}`), and `ename_remote` calls them. They compose a non-grid parent chain
   up to the grid child holding the `CellCoord`, so a camera parented to an airship reads and
   writes right; the write goes through the inverse of the parent's affine and never puts a
   `CellCoord` under a non-grid parent. They stop at the nearest grid, correct while there is one
   root grid. A raw `Transform.translation` predicate is wrong under big_space, so `component`
   refuses it and `CellCoord`, pointing at `position`.
9. **Timeouts are real seconds and fail the job** (default 30 s, at most 600). `not_settled` for a
   settle, `timed_out` for a condition, with the last observation in the message. A wait never
   succeeds on timeout, because the bug agents hit was a capture of an unsettled world.
10. **Waits never change time unless asked.** `time: "run"` on `wait.until` resumes the world when
    the job starts and pauses it in the frame the job ends, by any path: an `on_remove` hook
    covers cancel and despawn. "Drop it, run until it lands, freeze there" is then one call with
    no race between resuming and the wait starting. It is the only wait with a side effect, so
    `wait.until` is a `Development` command; `wait.settled` and `wait.activity` are `Safe`.
11. **Settling before a capture is a gate on the work entity, not a second code path.** A command
    that takes `settle` spawns its work entity with a `SettleGate`; the gate triggers `Settled` or
    `SettleTimedOut` there, and the feature's observer starts the real work or decides. The gate
    never fails a job itself: `screenshot.capture` and `video.record` fail `not_settled`, a batch
    capture may proceed and label the result. `WaitSystems::Resolve` runs before
    `ReadbackSystems::Request`, so a screenshot is requested in the frame that settled.

Conditions that need virtual time (`virtual_seconds`, `at_rest`) fail `world_paused` at once in a
paused world with no step in flight; the check runs after the condition, so one met in the last
stepped frame still succeeds. `at_rest` holds for `hold_seconds` of virtual time because speed
passes through zero at every bounce, and trusts avian's `Sleeping` only after a physics step has
run since the wait began: a body moved while asleep keeps `Sleeping` until the next step wakes it.

## Rejected

- **Reading `DiagnosticsStore` paths as the probe.** Sampled every 0.5 s with a 10-sample history,
  and a diagnostic exists only when its crate chose to publish it. Probes are exact per frame.
- **A generic "pending async tasks" count.** `bevy_tasks` exposes no queue length, and the asset
  server's in-flight load table is private. Owners of tasks report them.
- **Waiting in the sidecar** by polling components. A round trip per check, per-frame conditions
  are missed between polls, and pausing in the exact frame a condition holds is impossible.
- **A BRP watching method.** The job model already gives bounded waits without a stream.
- **A real-seconds condition.** It is the blind sleep again. `frames` and `virtual_seconds` cover
  the honest cases.
- **A `touching` / contact condition.** avian fills `CollidingEntities` only on entities that
  carry it, so the wait would have to insert a component. `at_rest` plus `position` covers "it
  landed".
- **A `camera_blend` probe.** No camera uses `CameraDriver`'s damping blend today; add one when
  one does.
