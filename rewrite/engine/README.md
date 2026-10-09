# Rust engine: airplayd (AirPlay 2 receiver)

Pure-Rust AirPlay 2 receiver engine. Replaces libuxplay/GStreamer/Bonjour.

## Layout
- `crates/airplay-mdns` — mDNS advertisement (`_airplay._tcp`, `_raop._tcp`) via `mdns-sd`. No Bonjour service needed.
- `crates/airplay-protocol` — RTSP/HTTP message parsing, pairing handshake state machine, session/stream setup types.
- `crates/airplay-crypto` — pair-verify (Ed25519/X25519), SRP-6a, stream key derivation, AES-CTR / ChaCha20 decryption.
- `crates/airplay-video` — RTP depacketization (H.264 FU-A/STAP-A), frame assembly, decode + D3D11 render (feature-gated backends).
- `crates/airplay-audio` — audio depacketization (AAC-ELD/ALAC), decrypt, WASAPI output (feature-gated backend).
- `crates/airplay-ipc` — NDJSON JSON-RPC server over Windows named pipe (control plane for the GUI).
- `crates/airplayd` — binary that wires everything together.

## Build / test
```
cargo test --workspace
cargo build --release
```

Backends requiring system libraries (FFmpeg, D3D11, WASAPI) are behind cargo features; default features keep tests dependency-free.

## License
GPL-3.0-or-later (protocol logic informed by GPL uxplay sources).
