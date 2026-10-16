---
name: game-capture
description: Take a screenshot or record a video of the running example game or editor, or screenshot any other window, then crop and zoom it for reading. Use when you need to see what the game actually rendered - verifying a UI or rendering change, reading on-screen text, or checking motion and animation over time. Capture through the game's MCP `screenshot_capture` and `video_record` tools; spectacle is the fallback for anything else.
---

# Game capture (ename engine workspace)

## The game: use MCP

Start the editor with `game_start` (`cargo game start` from a shell). It is the only binary with
the BRP server the `game` MCP server talks to; `example_game_bin` has none.
`docs/agent-tools.md` has the full reference.

`screenshot_capture`, `video_record` and `editor_set_view` are commands the game registers, so
those tools appear once the game is running (the server re-lists every two seconds). If they are
missing although the game runs, call them through `command_call` with the dotted name:
`command_call { "command": "screenshot.capture", "params": { ... } }`. `command_list` shows what
the build has.

The game renders captures itself, so focus, window stacking, desktop scaling and other windows do
not matter, and nothing but the game is captured. None of the tools need window focus: never
activate the game window just to capture it, because that steals the user's focus. Write output
under `scratch/`, which git ignores.
Never write captures to `/tmp` or the session scratchpad: both are a small shared tmpfs.

```
screenshot_capture { "path": "scratch/shots/scene.png", "source": "editor_camera" }
```

| `source` | What is captured |
| --- | --- |
| `window` | The whole editor window, panels included. |
| `game_camera` | What the stage's `MainCamera` rendered: the Game View. |
| `editor_camera` | The Scene View, gizmos and grid included. |

- A camera source needs its view shown. If a call fails with a hint naming `editor_set_view`,
  switch with `editor_set_view` (`view: "game"` or `"scene"`) and capture again. Switching changes
  what the user sees, so switch back afterwards if they were looking at the other view.
- `path` must end in `.png`. A relative path resolves against the project root; the result gives
  the absolute path written and the size in physical pixels. Missing directories are created and
  an existing file is replaced.
- Read the PNG with the Read tool to look at it.
- It works while time is paused (`time_set pause`), which is how to get a frame of a known state.
- Pass `settle: {}` on the capture, never a sleep before it: the game waits until loading,
  pipeline compiles and streaming have finished, then captures in that frame. `video_record` takes
  it too. A capture that cannot settle in time fails `not_settled` and names what was still busy.
- After a file edit (a shader, a stage), wait for the `Reloaded` log line, then settle:
  `wait_until { "condition": { "log": { "message_contains": "Reloaded" } } }`. The watcher reports
  an edit about 300 ms late, and a settle before that has nothing to wait for.
- A camera source is often a better crop than `window`: it already leaves out the editor panels.

### Recording video

`video_record` takes the same `path` and `source`, plus a `duration` whose only key names the unit:

```
video_record { "path": "scratch/video/flight.mp4", "source": "game_camera", "duration": {"seconds": 10} }
```

| `duration` | Records |
| --- | --- |
| `{"seconds": 10}` | 10 s of real time, resampled to `frame_rate`. The usual choice. |
| `{"timecode": "00:01:30"}` | The same, as `hh:mm:ss` or `hh:mm:ss.fff`. |
| `{"frames": 600}` | 600 rendered frames; plays in real time only if the game ran at `frame_rate`. |

- A bare number is refused. `frame_rate` defaults to 30 (max 240). `path` ends in `.mp4` or
  `.mkv`; the output is H.264.
- The call waits up to 50 s. A longer recording returns `state: "running"` and a `job` id:
  collect it with `job_wait` (`wait_seconds` up to 50), or end it early with `job_cancel`, which
  keeps what was recorded.
- The result reports `frames`, `dropped_frames`, `encoder` and `ended`. `ended` other than
  `completed` means it stopped early but kept what it had: `stopped`, `source_resized`,
  `camera_inactive`, `source_lost` or `stalled`. Resizing the window or switching the editor view
  mid-recording ends it.
- Recording follows real time, so it runs while the world is paused.
- Seconds mode fills gaps with repeated frames, so a slow game or `dropped_frames` shows up as
  stutter, not a shorter clip. When it reports drops, `log_get_entries` with
  `message_contains: "video:"` lists them by reason.
- `ffmpeg` must be on the game's `PATH` (`ffmpeg_missing` otherwise). Video needs the editor
  binary's `video` cargo feature, on by default; without it there is no `video_record` tool and
  `command_call` reports `unknown_command`.
  `ENAME_VIDEO_ENCODER=libx264` in the game's environment forces the software encoder.

Something has to move for a clip to show anything. The Main Camera is a child of the
`examples::airship` rigid body, so giving the airship a velocity moves the Game View:

```
world_mutate_component { "name": "examples::airship",
  "component": "avian3d::dynamics::rigid_body::LinearVelocity", "path": "", "value": [0.0, 0.0, -60.0] }
```

To look at a clip, extract stills and read them, or tile several into one image:

```sh
for t in 0 3 6 9.9; do
  ffmpeg -v error -y -ss $t -i scratch/video/clip.mp4 -frames:v 1 \
    -vf scale=flags=bilinear+full_chroma_int+accurate_rnd:in_h_chr_pos=0:in_v_chr_pos=128,format=rgb24 scratch/shots/f_$t.png
done
s=scratch/shots
magick $s/f_0.png $s/f_3.png $s/f_6.png $s/f_9.9.png -resize 40% +append $s/strip.png
ffprobe -v error -count_frames -show_entries stream=width,height,nb_read_frames,r_frame_rate:format=duration scratch/video/clip.mp4
```

The `scale` options decode the left-sited chroma correctly; a plain `ffmpeg -i clip.mp4 f.png` can
show a sub-pixel colour fringe at sharp edges that is not in the file.

### Minimized windows

Rarely relevant, but worth knowing: a minimized game window runs at about half its frame rate
(measured 60 to 31 fps on KDE), because the compositor slows its frame pacing. Captures still show
real content. A seconds-mode recording then covers the gaps with repeated frames, so it holds about
half the real motion. Focus and being covered by other windows make no difference.

## Fallback: spectacle

Use this only when MCP capture cannot do the job, and say which reason applies:

- the target is not the game;
- the game runs without the MCP server (`example_game_bin`, or the `game` server is not connected
  and cannot be reconnected);
- `screenshot_capture` fails for a reason a retry or a view switch does not fix;
- what needs inspecting is outside the game's own rendering, such as window decorations, OS
  dialogs or the surrounding desktop.

This machine runs KDE Plasma 6 on Wayland, which decides the tool:

```sh
kdotool search --name 'My Bevy Game' --limit 1 windowactivate
sleep 1
kdotool getactivewindow getwindowname    # verify before capturing
spectacle -b -n -a -o /abs/path/shot.png
```

`-b` background, `-n` no notification, `-a` active window, `-o` output file (absolute path). Use
`-f` (whole desktop) only when explicitly requested. Search windows by title: `kdotool search --pid`
matched unrelated windows here, so always check the active window's name before capturing. This
path steals focus; give it back afterwards.

`grim` and `import -window root` are installed but fail on KWin; do not spend a turn on them.

## Reading the result

A large shot scaled into a message is unreadable. Crop to the region and scale it up:

```sh
magick scratch/shots/shot.png -crop 1290x750+1998+330 +repage -resize 200% scratch/shots/crop.png
```

`WxH+X+Y` is in source pixels. `+repage` discards the leftover canvas offset. Use `magick`, not the
deprecated `convert`. `magick identify -format '%wx%h' shot.png` gives true dimensions; multiply
coordinates read off a scaled preview by the scale factor the image result reports.
