# AAC-ELD regression fixture

`eld-44100-stereo-480.frames` contains 12 independently encoded, raw AAC-ELD
access units. Each is prefixed by its length as a big-endian unsigned 16-bit
integer. The fixture contains synthesized sine waves, not a phone recording or
third-party media.

Generator: temporary Rust program using `fdk-aac` 0.8.0 / `fdk-aac-sys` 0.5.0.
The encoder is used only to create this fixture; neither FDK crate is a runtime
or test dependency of the application.

Encoder parameters:

- Object type: 39 (`Mpeg4EnhancedLowDelay`).
- Transport: raw access units, no ADTS or LATM.
- Sample rate: 44100 Hz, stereo, bitrate: 128000 bps, SBR disabled.
- Granule length: 480 (set `AACENC_GRANULE_LENGTH` before encoder initialization).
- AudioSpecificConfig returned by FDK: `F8 E8 50 00`.
- Input: 12 successive blocks of 480 interleaved stereo frames, no encoder flush.
- Left: `sin(t * 440 * TAU) * 8000`, cast to signed 16-bit PCM.
- Right: `sin(t * 880 * TAU) * 4000`, cast to signed 16-bit PCM.
- `t = (block_index * 480 + sample_index) / 44100.0`, calculated as `f32`.

The regression feeds every access unit through `RaopBuffer` and requires 960
finite PCM samples per packet, nonzero signal energy, and the stronger left
channel amplitude. A second test encrypts each complete AES block of the first
access unit and leaves its trailing bytes clear, matching AirPlay RTP transport.

SHA-256: `7595606212e3c78d80c0792614d60832177952ca467b84006d0a20c4527d2fe4`.
