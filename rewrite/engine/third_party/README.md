# Vendored dependencies

`shairplay/` is based on `shairplay` 0.10.0 (LGPL-3.0-or-later) and is kept
locally because the Windows receiver needs connection-lifecycle fixes that are
not present in the published crate.

Local changes support AirPlay screen-mirroring playback:

- video reads no longer treat 30 seconds without media payload as a dead TCP connection;
- the active video task belongs to its RTSP connection and is cancelled on teardown/drop;
- AP2 `TEARDOWN` honors stream types so stopping video does not tear down unrelated audio/control state.
- the async video callback preserves compressed-packet order with bounded backpressure;
- the application supplies the virtual display resolution and frame rate for `GET /info`;
- legacy type-96 audio negotiates ALAC or AAC-ELD instead of always decoding as ALAC;
- AAC-ELD decoding uses the Apache-2.0 `rusty_aac` crate; synthesized regression fixtures include provenance;
- ALAC RTP decoding preserves negotiated 16/24-bit sample width;
- unsupported realtime audio setup returns an RTSP error instead of an empty success response;
- stream SETUP preserves the live RTP control port and no longer advertises a
  temporary UDP socket that has already been closed.

The upstream license is preserved in `shairplay/LICENSE`.

## OpenH264 comparison tests only

`openh264-sys2/` is based on crate 0.9.8 (native OpenH264 2.6.0). It is patched
only for development/reference tests; the production engine uses FFmpeg and
has no normal OpenH264 dependency. The upstream license files are preserved.

The local patches record two defects found during playback investigation:

- `rec_mb.cpp`: upstream commit
  [94085e6](https://github.com/cisco/openh264/commit/94085e614baa78b3ad63151dce313ab36f7a6443),
  correcting reference handling for B-slice 16x8/8x16 partitions.
- `manage_dec_ref.cpp`: upstream
  [PR 3954](https://github.com/cisco/openh264/pull/3954), commit
  `70a825cf31faeb03a98d0d47436898f862da8c71`, comparing the maintained long-term
  frame index during reference-list reordering. This fixes the local captured
  AirPlay P-frame corruption in the comparison decoder. It is an upstream
  proposed patch, not a claim that the published crate includes the fix.
- `build.rs`: watches the vendored upstream directory and `OPENH264_NO_ASM`
  so Cargo rebuilds native objects after either changes.
