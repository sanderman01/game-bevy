# Performance sampling

Part of the [design record](../design.md). How an agent uses it is in
[../agent-tools.md](../agent-tools.md#measuring-performance).

Agents used to measure by scraping `diagnostics.log` lines with awk and sleeping 20 to 45 s per
run. Pass names with spaces broke the fields, one-off hitches set the max, and nobody noticed a
cargo build running beside the benchmark. A round took 25 to 35 minutes. `ename_engine::perf`
replaces that with two job commands, `perf.sample` and `perf.compare`, and a statistics panel.

## Decisions

1. **Per-camera GPU time comes from our own timestamps, recorded with the camera.** Bevy's pass
   spans have no view in their path, reach the main world frames late through a one-slot mutex
   that can drop frames, and carry no frame id. Matching them to the cameras active "now" was
   tried in airshipgame (`b95098b`) and blamed the wrong camera after a camera set change. A span
   per camera does not work either: Bevy finds a span's parent per thread. So the render world
   swaps each `ExtractedCamera`'s schedule for a wrapper that writes a timestamp before and after
   the camera's own schedule, and the slot that resolves them carries the camera entity and
   label. A camera turned off later cannot be blamed for an earlier frame. Shadow passes run
   inside the camera schedule, so a view's time includes them although Bevy never times them.
2. **Timestamps are empty compute passes with `timestamp_writes`.** That needs only
   `TIMESTAMP_QUERY`, not `TIMESTAMP_QUERY_INSIDE_ENCODERS`, so more backends get GPU rows.
   Without the feature `gpu_ms` is `"unavailable"` and views report CPU time only.
3. **Frame brackets** sit at the end of `RenderGraphSystems::Begin` and before `Submit`, so
   `gpu_ms` includes egui and post-processing. Root `RenderGraph` work outside
   `RenderGraphSystems::Render` is unordered against the begin bracket; game crates must put such
   work in `Render`.
4. **Frame slots never stall rendering.** At most four slots, each `Free`, `Recording` or
   `Mapping`. When none is free the frame goes untimed and counts in `gpu_frames_skipped`.
5. **Our own frame id.** `PerfFrame` counts main frames and is extracted, so a window is a range
   of ids. The series (`frame_ms`, `cpu_main_ms`, `render_wall_ms`, `gpu_ms`, views) are each
   summarized and hitch-split on their own. `frame_ms` is `Time<Real>::delta`, which under
   pipelined rendering is not the render cost of the same frame, so joining them per frame would
   be wrong. The one per-frame join is `gpu_unattributed_ms`, inside one GPU frame.
6. **Per-pass times stay Bevy's**, summed per frame over every camera that ran the pass. Bevy's
   logged average moves only on the first camera of a frame, so it understated a pass run by
   several cameras. Passes cannot be split per camera without changing Bevy. No fork, no upstream
   work (user decision), so they stay unsplit. Pass groups are matched to a window by their sync
   instant between the first and last GPU frame arrival; exact enough for sums and medians while
   the camera set does not change.
7. **Noise is the range of run medians**, `repeats` windows back to back, `null` with one run.
   Medians over several samples were what worked in the history, within about 0.3 ms run to run.
8. **Hitches are split out per series** at twice the window median. The worst frames are listed
   and the statistics leave them out, because a 300 ms rebuild otherwise owns `max` and `p95`.
9. **Settle, then hold steady, then warm up.** AGENTDX_04's `SettleGate` first. A timeout samples
   anyway with `not_settled`: an unsettled measurement, labelled, beats none. Then the rolling
   frame median must stop drifting, then warm-up frames are dropped.
10. **Bad conditions warn, they do not fail.** Machine load (other processes' CPU through
    `sysinfo`, GPU through Linux DRM fdinfo, one per-OS `GpuSnapshot` with no trait), vsync, an
    occluded window, the panel on, a video recording. The agent decides. A part the platform
    cannot read is `null`, never 0. Windows has no GPU arm until a Windows machine can test the
    PDH counter.
11. **`vsync_capped` under any present mode.** A compositor caps a window under `Immediate` too,
    so the check looks only at the frame median against the refresh interval.
12. **A view change invalidates a run.** A change of window size, view size, camera set or label
    mid-window, or between windows, drops every run so far and restarts the sample once, so all
    runs share one layout; a second change returns them with `unstable_view`. `perf.compare`
    warns the same when an arm's steps, or the two arms' windows, differ in size. Nothing pins
    the render size, so results record every size.
13. **`perf.compare` runs `A B B A` per round**, so linear drift cancels. Arms are command calls,
    usually `debug.set`, so any switch or game command is an arm and the plan adds no toggle of
    its own. Each step samples one window by default; the steps are the runs. Verdicts are
    judged on the rounded numbers the result shows. `restore` starts its job commands and does
    not wait for them, since the compare is already ending.
14. **Timing runs only on demand**, derived each frame: a perf job's work entity holds
    `PerfDemandHolder`, or `overlay.stats` is on. Off, there is no wrapper, no query and no record.
15. **Two plugins.** `PerfPlugin` is in `EnginePlugins` (editor and release): timer, records,
    labels, counters and the panel. `PerfCommandsPlugin` is editor-only: the commands, pass
    grouping and load. Neither adds `RenderDiagnosticsPlugin`, which the editor owns (a second add
    panics); without it `passes` is `"unavailable"`.

## Statistics panel

A feathers pane per host camera, top right of its viewport, behind `overlay.stats` (`F3`). Bevy UI
rather than egui, so it works in release builds and lands in captures as the user sees it. It reads
the same `PerfRecords` as `perf.sample`.

- Rows exist only for values the engine measures; a row whose source is missing is hidden, never
  "n/a". Unity's batches, triangles and SetPass counts have no source here.
- One panel per host camera, looked up by camera entity, related by `StatsPanelOf` /
  `StatsPanels` with `linked_spawn`. Neither is reflected, so a saved stage never contains a panel.
- Every panel node is `Pickable::IGNORE`; bevy_ui treats a node without `Pickable` as blocking.
- The Game View image target uses the window's scale factor, so the panel has the same physical
  size in both views.
- `FeathersCorePlugin` is an `EnginePlugins` member so a game can replace it. Its cursor plugin
  rewrites `CursorIcon` from `DefaultCursor` every frame, so the editor copies egui's cursor into
  `DefaultCursor`. `TabNavigationPlugin` is left out: the panel is read only, and it warns on
  every Tab press in a window with no `TabGroup`.
- Panel colours are our own theme tokens, `ename.stats.bg` and `ename.stats.header.bg`, derived
  from the theme's pane colours, so the pane can be translucent.
- **Extensible content.** What the panel shows is entities: `StatsSection`s under one
  `StatsContent` root, `StatsRow`s under sections. Content outlives the panels (one per host,
  despawned with the switch), and other code adds or removes rows by spawning and despawning.
  The built-in rows use the same mechanism. A system row's system is boxed in the row and
  registered on first refresh as a tracked `SystemHandle`, so despawning the row unregisters it.
  Each refresh compares a sorted layout key against the one each panel was built from and
  rebuilds that panel's body on a difference; observers would miss field edits, reparenting and
  child swaps. Each section is one CSS grid, so value columns line up without padded strings.
  `title` and `header` are `Option` fields, not marker components: they are parts of one
  presentation value, not states of the entity. Built-in sections are found by marker
  components, not a resource of entity ids that could go stale. The refresh is exclusive, since
  row systems run through `run_system_with` and return their cells.

## Measured overhead

Measured 2026-10-18 on the example editor in the Game View (815 x 489 view, 1360 x 710 window),
`dev` build, `perf.sample` with `repeats: 5` and `uncapped: true`, since a vsync-capped frame time
hides changes this small. Noise is the range of the five run medians.

- **Always-on `RenderDiagnosticsPlugin`.** Two baselines with it, one build without it in
  between. GPU 0.93 and 0.94 ms against 0.86 ms per frame: +0.07 to 0.08 ms, noise 0.01, about
  8 % of this scene's GPU time. Frame 9.97 and 10.12 ms against 9.64 ms: +0.3 to 0.5 ms, noise
  0.2, all of it in `render_wall_ms`; `cpu_main_ms` did not move.
- **The statistics panel** (`perf.compare`, `overlay.stats` false against true, 2 rounds, one
  panel since only the Game View host is active): frame +0.25 ms (noise 0.13), `cpu_main_ms`
  +0.20 (0.08), `render_wall_ms` +0.21 (0.11), GPU +0.02 ms (0.00). Both arms hold timing
  demand, so this is the panel's UI and text cost alone, not the timer's.

The example scene is light: three cameras and seven timed passes. These numbers show the fixed
cost per frame and that it sits above the noise. They do not show the cost in a game with many
cameras and passes, such as landscape streaming or reflection probes, where Bevy's timestamp
queries grow with every pass run.

## Rejected

- **Main-world matching of Bevy's per-path measurements to cameras.** Frames late, droppable, no
  frame id. It cannot be made correct.
- **A diagnostic span per camera.** Span parentage is per thread.
- **Patching or forking Bevy** to put the view in span paths. A fork costs upkeep, and upstream
  work is ruled out.
- **A `Bound` panel row.** Over a 0.5 s mean it would flicker at its thresholds.
- **Pinning the render size for benchmarks.** No plan owns it; results record sizes instead.
