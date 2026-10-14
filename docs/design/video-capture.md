# Video capture

Part of the [design record](../design.md). How to use it is in
[../agent-tools.md](../agent-tools.md#video). The decisions:

**Raw frames go over stdin to an `ffmpeg` child process.** The engine reads each frame back from
the GPU, crops it on a writer thread, and writes the pixels to `ffmpeg`, which converts them to
YUV and encodes them. ffmpeg already knows VAAPI, NVENC, AMF, QSV and VideoToolbox, so hardware
encoding costs nothing to support. At 1080p on an RX 9060 XT, `h264_vaapi` encoded 600 piped frames
in 2.95 s and `libx264 -preset veryfast` in 5.4 s, so the pipe is far from the bottleneck. A crash
in a driver or encoder kills ffmpeg, not the game. The runtime dependency is `ffmpeg` on `PATH`;
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
  to save an 8 MB readback per frame. If readback ever shows up in a profile, the next step is an
  RGB-to-NV12 compute pass before readback, which needs no interop.

**Encoders are probed with a test encode.** `ffmpeg -encoders` lists `h264_amf` on Linux, where it
fails for want of the AMF runtime, so a listing proves nothing. Only a successful probe is cached,
so installing ffmpeg needs no restart. `ENAME_VIDEO_ENCODER` forces one by name.

**The file is tagged as sRGB rather than converted.** The pixels are sRGB-encoded. Converting the
transfer to BT.709 needs ffmpeg built with zimg and changes every pixel. Tagging the truth (BT.709
primaries and matrix, `iec61966-2-1` transfer) is correct for any player that reads the tag, and
close for one that does not. The tags are set with `setparams`, because output-side colour options
are lost after the `scale` filter.

**A duration names its unit.** `{"frames": n}`, `{"seconds": x}` or `{"timecode": "hh:mm:ss"}`.
JSON has one number type, JavaScript writes `10.0` as `10`, and an agent drops the `.0` freely, so
an integer-means-frames rule would turn ten seconds into a sixth of a second without an error.

**Time is real time.** Frames are stamped with real time when they are requested, so pausing or
slowing virtual time does not stop or stretch a recording. Seconds and timecodes resample to a
constant frame rate, repeating or dropping frames; each frame takes the tick nearest its stamp, so
a game running at the output rate gets exactly one tick per frame despite jitter. A frame count writes each accepted frame once.

**A change of source ends the recording and keeps it.** A resize, a camera that goes inactive or
disappears, five seconds without a frame, or a stop each end the recording early and report
success with the reason. Scaling or padding every frame would hide the change and cost CPU on
every frame. Failing would throw away what was recorded.

**Screenshots and video share one readback per target per frame.** Bevy captures a target at most
once per frame and silently drops a second `Screenshot`, so neither feature spawns its own: both
queue requests, and one `Screenshot` per target is fanned out to every requester with the crop it
asked with. At most four readbacks of a target are in flight for video, counted until Bevy
delivers them or drops their `Screenshot` entity, which bounds the GPU buffers and Bevy's unbounded
delivery channel. One that never arrives stops counting after 10 seconds, so lost readbacks cannot
block a target until restart.

**The game never waits on ffmpeg.** Frames go to the writer through an eight-frame queue. A full
queue drops the frame and counts it. A watchdog fails a recording whose writer has been stuck in
one pipe write for 10 seconds, or that has not finished within 60 seconds of its last frame.

**Cancelling is one atomic flag and a kill.** The writer blocks only in a timed channel receive, a
pipe write, or a polled wait on the child, and each wakes on cancel: the receive times out every
100 ms, the kill breaks the pipe, and the poll checks the flag. The child is locked only to
register, poll and kill it. Publishing swaps the same flag to "publishing" before the rename, so a
cancel either wins and the file never appears, or loses and the finished file stays. Despawning a
request or exiting the app cancels; exit waits at most two seconds for the writer threads. A hard
kill of the game can leave a `.ename-video-*` temp file beside the target.
