# Rust engine: airplayd (AirPlay 2 receiver)

Pure-Rust AirPlay receiver engine. The runtime uses `shairplay` for AirPlay
discovery, pairing/FairPlay, RTSP, stream setup, and screen-mirroring transport.
H.264 mirror frames are decoded with FFmpeg's native H.264 decoder and rendered
in a native `minifb` window. The engine launches a hidden FFmpeg child process
and reads complete RGB pictures through a pipe. Keep a static `ffmpeg.exe`
beside `airplayd.exe`; the packaged application includes this runtime. No
Bonjour service, Qt, GStreamer, or separate system FFmpeg installation is needed.

## Playback controls

The playback window starts at Medium size (at most 960 pixels on its longest
edge), with the video aspect ratio preserved. Use **Window size / 視窗大小**
while connected, or **Ctrl+1 / Ctrl+2 / Ctrl+3 / Ctrl+4** for Small (640), Medium
(960), Large (1280), and Fit to screen. Sizes are constrained to the current
monitor's work area. Rotating the phone preserves the selected size; resizing
does not reconnect or reset the decoder. The selection lasts for the current
receiver run. Manual window resizing is also available.

On Windows, playback uses HALFTONE color scaling when shrinking the video.
The default GDI BLACKONWHITE mode combines colors with bitwise AND and causes
dark lines, color blocks and damaged text in a reduced playback window.

Resolution and Max FPS in the control GUI configure the virtual display
advertised to the phone. Window size controls only the local playback viewport.
Compressed H.264 packets are decoded in order on a separate thread; the display
uses the latest decoded frame so native menus and dragging cannot block decode.

FFmpeg handles reference pictures, B-frame ordering, color matrices and
full/limited range. Complete AirPlay access units receive a trailing AUD so
FFmpeg's elementary-stream parser can emit a frame without waiting for the
next phone packet. Slice threading avoids frame-thread buffering. A change in
SPS dimensions reopens the FFmpeg decoder because its PPM encoder fixes the
initial output dimensions; the AirPlay connection, audio, window and selected
viewport size survive this change. Stop/teardown closes and reaps the decoder
process, including when input is blocked behind pipe backpressure.

Five-second transport, FFmpeg input/decode and display statistics distinguish
network gaps, pipe backpressure and slow presentation without recording video.
OpenH264 is a development-only dependency for synthetic fixture generation and
comparison tests; it is not linked into the production engine.

Mirroring audio supports raw AAC-ELD (44.1/48 kHz, mono/stereo, 480/512 samples
per frame), alongside existing ALAC/PCM paths. AAC-ELD decoding uses `rusty_aac`;
low-delay SBR is not reconstructed. Unsupported negotiated formats return an
RTSP error and a log entry. Decoded PCM is mapped to the default Windows output
device's channel count. A physical iPhone is still required to verify sustained
wireless playback and audible output on the selected device.

AAC-ELD RTP packets are decrypted on arrival and decoded only when dequeued in
sequence order. This preserves decoder history when packets arrive out of order;
sorting already-decoded PCM cannot repair that history. Duplicate/stale packets
and packets discarded by a flush do not enter the decoder. An undecodable access
unit is consumed as one frame of silence so later packets can continue draining.

Audio output accumulates 60 ms of PCM before starting or recovering from an
underrun, adding approximately 60 ms of playback latency to absorb packet jitter.
Five-millisecond ramps soften startup, underrun recovery and overflow transitions.
The queue remains capped at 750 ms and discards only complete device frames when
full. Flush and session replacement reset the queue and its transition state.

When system audio is available, decoded video frames wait for the same 60 ms
before presentation to compensate for the added audio buffering. The renderer
selects the newest due frame after a stall, keeps at most eight pending decoded
frames, and discards pending frames on stream end or replacement. Decoding and
window event handling remain independent of this wait. Disabled audio adds no
video holdback. This compensates the software buffer only; it is not a shared
RTP/NTP presentation clock and does not measure transport or device latency.

## Layout
- `crates/airplay-ipc` — NDJSON JSON-RPC server over Windows named pipe (control plane for the GUI).
- `crates/airplayd` — receiver daemon, persistent pairing identity, AirPlay runtime, and H.264 render window.

## Build / test
```powershell
# Optional override for a development/test build. Packaged builds use the
# ffmpeg.exe sitting beside airplayd.exe.
$env:UXPLAY_FFMPEG_PATH = 'C:\Tools\ffmpeg\ffmpeg.exe'
cargo test --workspace
cargo build --release -p airplayd
```

FFmpeg must provide its native `h264` decoder and `ppm` encoder. See
[`../packaging/README.md`](../packaging/README.md) for architecture checks,
runtime staging and source/license notices. The 360-frame independent
libx264 fixture and its decoded YUV samples verify frame count, order and
BT.709 full-range color; first-frame, rotation and shutdown tests use real
FFmpeg processes. Hardware acceleration is not enabled by this backend.

Native display checks (open test windows; run on an interactive Windows desktop):
```powershell
cargo test -p airplayd native_downscaling_preserves_colors -- --ignored --nocapture
cargo test -p airplayd native_window_size_smoke -- --ignored --nocapture
```

## Local capture diagnostics

Capture is off by default. Only with explicit user authorization, create the
one-shot marker `$env:TEMP\uxplay-rs-capture-next-session` before a new mirror
session. It is consumed once and records at most about ten seconds/16 MiB of
decrypted H.264 video packets under TEMP, without audio or session keys. Keep
private captures and derived images out of the repository. The ignored
`replay_local_mirror_capture` test is an OpenH264 comparison tool, not the
production FFmpeg acceptance test.

## License
GPL-3.0-or-later (protocol logic informed by GPL uxplay sources).
