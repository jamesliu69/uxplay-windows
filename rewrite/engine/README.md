# Rust engine: airplayd (AirPlay 2 receiver)

Pure-Rust AirPlay receiver engine. The runtime uses `shairplay` for AirPlay
discovery, pairing/FairPlay, RTSP, stream setup, and screen-mirroring transport.
H.264 mirror frames are decoded with `openh264` and rendered in a native
`minifb` window. No Bonjour service, Qt, GStreamer, or FFmpeg install is needed.

## Layout
- `crates/airplay-ipc` — NDJSON JSON-RPC server over Windows named pipe (control plane for the GUI).
- `crates/airplayd` — receiver daemon, persistent pairing identity, AirPlay runtime, and H.264 render window.

## Build / test
```
cargo test --workspace
cargo build --release -p airplayd
```

## License
GPL-3.0-or-later (protocol logic informed by GPL uxplay sources).
