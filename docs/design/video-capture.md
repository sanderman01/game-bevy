# Video capture

Part of the [design record](../design.md). How to use it is in
[../agent-tools.md](../agent-tools.md#video). The decisions:

**NV12 frames go over stdin to an `ffmpeg` child process.** The engine converts each frame to NV12
on the GPU, reads it back into a reused buffer, and writes the mapped buffer straight into
`ffmpeg`, which only uploads (VAAPI) or encodes it. ffmpeg already knows VAAPI, NVENC, AMF, QSV
and VideoToolbox, so hardware encoding costs nothing to support. At 1080p on an RX 9060 XT,
`h264_vaapi` encoded 600 piped frames in 2.95 s and `libx264 -preset veryfast` in 5.4 s, so the
pipe is far from the bottleneck. A crash in a driver or encoder kills ffmpeg, not the game. The runtime dependency is `ffmpeg` on `PATH`;
without it a recording fails with `ffmpeg_missing` and nothing else is affected.

Rejected:

- `ffmpeg-next` in process. It builds against FFmpeg 9, but hardware upload needs raw FFI (the
  workspace denies `unsafe`). It adds a C build coupled to the system FFmpeg version, and an
  encoder crash takes the game down.
- An image sequence encoded afterwards. Encoding a PNG per frame costs more than the H.264 encode,
  and the temporary files run to gigabytes.
- `bevy_capture` 0.6 records headless apps only, with no window capture. `bevy_image_export` writes
  image sequences. `bevy_tape` needs a newer wgpu than Bevy 0.19.
- GPU zero-copy (DMA-BUF into VAAPI or Vulkan Video). Weeks of `unsafe`, platform-specific interop
  to save a 1.5-byte-per-pixel readback.
- Bevy's `Screenshot` per frame, the first version. Every capture allocated a texture and a
  readback buffer, the CPU copied each 14 MB frame several times, and ffmpeg's RGB-to-YUV
  conversion cost about 49 ms of CPU per 2560×1382 frame against 8 ms for NV12 input. In a
  GPU-bound scene that took a 37 fps game to 20 fps at 60 fps output.

**Encoders are probed with a test encode.** `ffmpeg -encoders` lists `h264_amf` on Linux, where it
fails for want of the AMF runtime, so a listing proves nothing. Only a successful probe is cached,
so installing ffmpeg needs no restart. `ENAME_VIDEO_ENCODER` forces one by name.

**The conversion is on the GPU.** A standalone WGSL compute shader (no Bevy shader imports, built
with wgpu directly) applies the BT.709 matrix at TV range to the sRGB-encoded values, with the crop,
and writes tightly packed NV12: one `u32` per invocation, a 2D dispatch of 256-wide groups. Chroma
is left-sited, the H.264 default, through a `[1,2,1]/4` horizontal filter averaged over two rows;
a plain 2×2 average would be centre-sited and shift colour by half a pixel in players that honour
the position. Tools that ignore it (ffmpeg's default swscale treats 4:2:0 as centre-sited) show a
sub-pixel colour shift at sharp edges in extracted stills; that is the tool, not the file.

**The file is tagged as sRGB rather than converted.** The samples are sRGB-encoded. Converting the
transfer to BT.709 would change every pixel. Tagging the truth (BT.709 primaries and matrix,
`iec61966-2-1` transfer) is correct for any player that reads the tag, and close for one that does
not. The tags are set with `setparams`.

**A duration names its unit.** `{"frames": n}`, `{"seconds": x}` or `{"timecode": "hh:mm:ss"}`.
JSON has one number type, JavaScript writes `10.0` as `10`, and an agent drops the `.0` freely, so
an integer-means-frames rule would turn ten seconds into a sixth of a second without an error.

**Time is real time.** Frames are stamped with real time when they are requested, so pausing or
slowing virtual time does not stop or stretch a recording. Seconds and timecodes capture only the
first frame that rounds into a new output tick, so readback and encoding run at the lower of the
game's and the output's rate; at a high render rate the chosen frame can be up to half a tick
early. The file is resampled to a constant rate, each frame taking the tick nearest its stamp, so
a game running at the output rate gets exactly one tick per frame despite jitter. A frame count
captures every frame until the writer has `n`. The default output rate is 30 fps for every source.

**A change of source ends the recording and keeps it.** A resize, a camera that goes inactive or
disappears, five seconds without a frame, or a stop each end the recording early and report
success with the reason. Scaling or padding every frame would hide the change and cost CPU on
every frame. Failing would throw away what was recorded.

**Video reads frames in its own render-world pass.** It runs in `RenderGraphSystems::Finish` as an
exclusive system: after every camera's commands are submitted, before present. An image target
(the Game View) is read where the camera drew it. A window cannot be read, because Bevy configures
its swapchain as a render attachment only, so a recorded window renders into a persistent texture
of ours, installed through `ViewTargetAttachments` (the override Bevy's screenshots use), and is
blitted to the swapchain in the same pass. The redirect replaces an attachment only when it is the
window's own swapchain view, compared by identity, and each capture re-checks in `Finish` that the
attachment is still the one it expects; a screenshot of the same target in the same frame wins and
the video frame is skipped. Screenshots still go through Bevy's `Screenshot`, one per target per
frame, fanned out to every screenshot request.

**Four buffers per recording, freed on every path.** A slot is reserved before rendering and
either reaches the writer (whose dropped frame unmaps and frees it) or is freed where the request
ends: no free slot, a conflict, no swapchain, a size or format mismatch, a map error, or a writer
that has gone. Each request ends in exactly one of `delivered` or `failed`, so draining a finished
recording cannot hang. Map callbacks may fire on any polling thread and out of order; the writer
orders frames by sequence number, and the pass polls the device itself.

**The game never waits on ffmpeg.** A frame with no free buffer is dropped and counted. ffmpeg runs
at nice 10 on Linux (`setpriority` in `pre_exec`, the one `unsafe` function, because glibc documents
it as async-signal-safe; other Unix systems are left alone) and at below-normal priority on Windows,
and `libx264` uses at most 4 threads. A watchdog fails a recording whose writer has been stuck in
one pipe write for 10 seconds, or that has not finished within 60 seconds of its last frame.

**Bevy items this depends on**, the checklist for an upgrade: `RenderGraph` and
`RenderGraphSystems::Finish`; `RenderSystems::PrepareViews`, `prepare_view_attachments` and
`prepare_view_targets`; `ViewTargetAttachments` and `OutputColorAttachment`; `ExtractedWindows`
(a per-window component from Bevy 0.20); `RenderAssets<GpuImage>`; `RenderDevice`, `RenderQueue`;
`ExtractSchedule` and `Extract`; and wgpu's `map_async`/`get_mapped_range` (a `Result` from wgpu
30).

**Cancelling is one atomic flag and a kill.** The writer blocks only in a timed channel receive, a
pipe write, or a polled wait on the child, and each wakes on cancel: the receive times out every
100 ms, the kill breaks the pipe, and the poll checks the flag. The child is locked only to
register, poll and kill it. Publishing swaps the same flag to "publishing" before the rename, so a
cancel either wins and the file never appears, or loses and the finished file stays. Despawning a
request or exiting the app cancels; exit waits at most two seconds for the writer threads. A hard
kill of the game can leave a `.ename-video-*` temp file beside the target.
