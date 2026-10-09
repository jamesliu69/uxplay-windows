# Mirroring playback improvements

Historical checkpoints follow in chronological order; earlier pending/status
statements describe those checkpoints, not the latest state. See the
[complete development and issue history](../../development/2026-10-09-uxplay-rs-development-history.md)
for the consolidated record and the subsequent MSI 0.1.1 OS-check correction.

User-approved design (2026-10-09): select Small, Medium, Large, or Fit to screen
inside the playback window. Default to Medium. Preserve aspect ratio and the
selection through phone rotation. Resizing must not reconnect the receiver.

## Implementation and verification

- [x] Add the playback size menu and shortcuts (Ctrl+1 through Ctrl+4).
  Fit client dimensions into the current monitor's work area, accounting for
  window borders and the menu. Resize the existing HWND when video dimensions
  change. Keep displaying the last frame when the phone sends no new frames.
- [x] Separate the bounded, lossless compressed-packet decoder queue from the
  render thread. An async video callback applies TCP backpressure without
  blocking a Tokio worker. Only already-decoded display frames may be replaced.
  Session generations reject stale frames and session drop clears the window.
  Regression: a slow consumer receives every compressed packet in order.
- [x] Pass GUI resolution and FPS to the advertised virtual display. Reject
  malformed display dimensions. Regression: GET /info reports the selected
  width, height, and maxFPS rather than fixed 1080p/60 values.
- [x] Honor legacy type-96 compression type (ALAC=2, AAC-ELD=8). Add an ELD
  decoder and test a valid stereo ELD access unit through the RTP buffer.
  Log negotiated codec and decode failures without logging session keys.
- [x] Map mono/stereo decoded PCM to the system output channel layout so an
  output device with more than two channels does not play at the wrong speed.
  Verify sample ordering and clearing buffered audio on stream replacement.
- [x] Preserve the live RTP control port in SETUP. Regression: legacy ALAC and
  AAC-ELD report the same bound control port owned by RTP; other stream types
  do not advertise a closed dummy UDP listener.
- [x] Run Rust workspace tests, vendored protocol tests with video enabled,
  WPF tests, release build, and a synthetic native-window playback check.
  Physical iPhone reconnect, sustained playback, and audible output remain
  separate device checks; do not infer them from compilation or replay.

## Native window verification

`cargo test -p airplayd native_window_size_smoke -- --ignored --nocapture`
passed with actual Windows UI interaction. The generated H.264 test stream
switches from 1920x1080 to 1080x1920 after 30 seconds. Verified Medium default,
Small after maximizing, preserved Small on rotation, Large constrained to the
work area, and Fit to screen. The same HWND survives rotation. The test also
exited normally with the native size menu left open during runtime shutdown.
This is a synthetic local stream, not a physical iPhone acceptance test.

## Initial local verification (before the quality follow-up)

- `cargo test --workspace`: 231 passed, 1 intentionally ignored native-window
  test (run separately above). Includes real TCP GET /info and unsupported
  audio SETUP responses, real AAC-ELD fixture decoding, RTP control socket
  ownership, packet backpressure, and session replacement regressions.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings -A clippy::cargo_common_metadata`:
  passed. The exception is for existing missing Cargo package metadata in the
  application/IPC crates; strict Clippy without this exception reports those
  repository/readme/keywords/categories fields.
- `cargo build --release -p airplayd`: passed.
- `dotnet build UxPlayRs.slnx --configuration Release --nologo`: passed,
  zero warnings and errors.
- `dotnet test UxPlayRs.slnx --configuration Release --no-build --nologo`:
  13 passed.
- `git diff --check`: passed. Deterministic Sonar secrets scanning reported
  no secrets in changed text files (with server-authentication warnings).

Launched the Release GUI and started its Release engine. The native GUI showed
Running (advertising), with `uxplay-rs` on TCP 7100 and successful Windows audio
initialization at 48000 Hz / 2 channels. A GET /info against this running engine
returned 1280x720 / maxFPS 30, matching the GUI's selected values.

Initial release engine: `rewrite/engine/target/release/airplayd.exe`, 7202816 bytes,
SHA-256 `009727E671A7ECF2DF1FDEAE9F7B8B7EA2726795B3E473F132F39A2ED0DFEA49`.
Release GUI: `rewrite/gui/UxPlayRs.Gui/bin/Release/net8.0-windows/UxPlayRs.Gui.exe`,
151552 bytes, SHA-256
`E7A8A6568A596E9BA63954642570ED6BF0396BEB726C693652E95AFDB342FD18`.

Physical iPhone playback, sound at the speakers, sustained wireless stability,
and reconnect acceptance are pending user/device verification. This change
does not establish hardware-accelerated decode or performance parity with the
upstream GStreamer player. No commit, push, or installer packaging was performed.

## Quality follow-up: color corruption while downscaling

The user's screenshots showed damaged text and color blocks. A decoded-pixel
test checked the generated gradient at 320x192, 1920x1080 and 1080x1920: mean
RGB error was 1.11/1.20/1.39 and maximum error 5/8/8, so the decoded data was
intact. The native window still displayed dark lines when downscaling it.
The original UI check covered sizing and lifetime, but did not verify image
fidelity; it was insufficient to catch this defect.

Root cause: minifb's Windows renderer uses StretchDIBits without setting a
stretch mode. The actual window DC reported mode 1 (BLACKONWHITE), which
performs bitwise AND on combined pixels. A native regression reproduced this
with gray 127/128: shrinking 2x2 to 1x1 returned black 0x000000. After setting
HALFTONE and the required brush origin on the persistent CS_OWNDC, the same
test returned correct gray 0x808080 (mode 4).
Reference: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-setstretchbltmode

Verification after the fix:

- `cargo test --workspace`: 232 passed, 2 intentionally ignored native tests.
- `cargo test -p airplayd native_downscaling_preserves_colors -- --ignored --nocapture`:
  RED before the fix, GREEN after the fix.
- `cargo test -p airplayd native_window_size_smoke -- --ignored --nocapture`:
  passed; the actual horizontal and vertical downscaled gradients no longer
  showed the dark lines seen before the fix.
- Rust formatting, Clippy with the existing package-metadata exception, and
  Release build passed. C# source is unchanged by this follow-up.

Stopped the old engine through the GUI, rebuilt Release, changed its resolution
selection from 720p to 1080p, and restarted. GET /info against the running
Release engine returned 1920x1080 / maxFPS 30. Window size remains independent
of stream resolution. Physical iPhone image quality after reconnect is pending
user/device verification.

Current engine: `rewrite/engine/target/release/airplayd.exe`, 7208960 bytes,
SHA-256 `DEA066695F8953D80168A8111E013E0CA2E486959A2614E32D8B9A3BFDC5A694`.

Baseline: 8426624. Earlier uncommitted audio/vendoring work was committed by
another session during investigation; preserve that baseline and unrelated work.

## Follow-up: confirmed decoder defects and FFmpeg production backend

The user retracted the earlier accidental positive quality response. The
original UxPlay was confirmed smooth with correct colors on the same phone and
Wi-Fi. The rewrite still had reference-picture artifacts even after continuous
decode and GDI downscaling were repaired; a stable FPS counter did not establish
image correctness.

With explicit permission, a one-shot local TEMP capture recorded ten seconds
of mirror video: 297 records (one AvcC and 296 payloads), 4,565,510 bytes. No
audio, pairing/session keys or setup data were recorded. The same input showed
mouth/face corruption with OpenH264 and correct pixels with FFmpeg's native
H.264 decoder. FFmpeg's optional libopenh264 decoder also reproduced the
corruption. This isolated the remaining artifact to the decoder rather than
Wi-Fi or the Rust packet converter.

Two upstream OpenH264 defects were reproduced in reference tests: B-slice
partition references (commit 94085e6) and long-term P-frame reference reordering
(proposed PR 3954, commit 70a825c). The latter corrected the captured AirPlay
comparison: ten sampled frames differed from native FFmpeg by at most three
RGB levels, with no pixels above the previous 20-level corruption threshold.
This is local replay evidence, not sustained device acceptance.

The user explicitly requested FFmpeg instead of OpenH264. Production now uses
FFmpeg's native H.264 decoder in a hidden child process, reads RGB PPM pictures,
and preserves the ordered bounded compressed queue. It emits an AUD after each
complete access unit to avoid waiting for the next phone packet. FFmpeg's PPM
encoder fixes its initial dimensions, so an SPS dimension change reopens only
the decoder process; AirPlay, audio, viewport selection and HWND remain alive.
OpenH264 and the manual color converter are test/reference-only dependencies.
The native render loop retains the prior pixel allocation through the next
message pump/update because minifb holds its raw pointer for WM_PAINT.

The Release engine was built and restarted with `ffmpeg.exe` beside it. The
running child executable was verified to come from that same directory. The
user then confirmed: "畫面流暢、顏色正常，也有聲音". This establishes the observed
playback result for this phone and session; it does not establish all-device,
long-duration or hardware-accelerated performance parity.

Verification at this checkpoint:

- `cargo test --workspace`: 242 passed, 3 intentionally ignored native/private
  diagnostic tests. The tests include real-FFmpeg first-frame delivery,
  360-frame High/B-frame order and BT.709 colors, dimensions across rotation
  within one mirror session, and prompt idle-process shutdown.
- Formatting and Clippy passed with the existing `cargo_common_metadata`
  package-field exception. `cargo tree -p airplayd --edges normal` contains no
  OpenH264 dependency.
- Full x64 `stage.ps1` completed using the validated local static FFmpeg build,
  a target-specific Release Rust engine, and a self-contained WPF publish.
  Staging verified native H.264/PPM support, architecture, copied hash and
  execution from the bundle. ARM64 and release ZIP creation were not run.
- Runtime capture is off; the one-shot marker has been consumed. The private
  capture and derived images remain only under local TEMP, outside Git.

Final focused review also reproduced a blocked-stdin teardown defect with a
non-reading child process. Session cleanup now shares a stream-tagged decoder
control, stops that child outside the frame-slot lock even when a replacement
session already owns the slot, and continues serving after the interrupted
old write. The spawn handshake rechecks session ownership before publishing
the control. The bounded headless regression passed both ordinary teardown
(8.219 ms) and overlap/reconnect: the old heartbeat stopped and the next real
FFmpeg decoder produced 64x64. No important findings remained on re-review.
The rotation test now uses separate AvcC then payload packets, like the phone.

Packaging's static-import check now uses a bounded Windows OS DLL allowlist;
a DLL installed in the developer's System32 is not sufficient. Windows
PowerShell 5.1 PE fixtures passed 18/18 checks, including rejection of VC,
MinGW, shared FFmpeg and unknown dependencies. The real static FFmpeg build
still passes. The full x64 staging command was rerun after these corrections
and completed successfully. Final workspace tests remain 242 passed / three
intentionally ignored; formatting and Clippy passed again. Current phone
playback was left running; the complete final build is in
`rewrite/out/x64/bundle`. No commit, push, installer or public release was made.

## Requested Windows installer

The user subsequently requested one installer file. Added a separate WiX MSI
manifest and `rewrite/packaging/installer.ps1` for the rewrite. Version 0.1.0,
x64, Traditional Chinese setup UI, per-machine configurable Program Files
directory, desktop/start-menu shortcuts, and standard repair/upgrade/uninstall.
Its stable UpgradeCode differs from the original Qt product. FFmpeg, notices
and the self-contained GUI runtime are embedded; no external cabinet is needed.

Final artifact: `rewrite/out/x64/artifacts/UxPlayRs-0.1.0-x64.msi`, 109,961,820
bytes, SHA-256
`22217479c456f6e5b45fc5c51251a2a9f951648e1576ff555d2bf820772a8465`.
Authenticode: NotSigned. Build HEAD remained
`8426624d68c410f4f7b62d6f25722e016f13e96f`; receipt explicitly marks the source
worktree dirty and records hashes for the actual packaged payload.

Verification: full WiX ICE validation with no suppression, 7-Zip integrity,
administrative extraction exit 0, all 470 non-PDB payload files matched size
and SHA-256 with no extras, and four FFmpeg integration tests passed using
the executable extracted from this MSI. Independent source review found no
important issue. An ICE38 menu component placement was corrected, and the
extraction comparison caught/fixed an initially ineffective PDB glob before
the final build. `.msi.receipt.json`, `.msi.sha256`, and
`.msi.verification.json` are saved beside the MSI.

Administrative extraction did not install/register the product. Actual
install/uninstall/upgrade and installed GUI/device playback were not run;
the existing user's live receiver was left running. This is a local unsigned
installer, not a signed public release. No commit or push was performed.

## Installer 0.1.1: actual Windows build check

The user subsequently reproduced an OS launch-condition failure on 64-bit
Windows 11 build 26300. Native msiexec reported compatibility WindowsBuild 9600
and VersionNT64 603. The 0.1.0 condition incorrectly rejected that environment;
administrative extraction and a PowerShell-hosted COM evaluation had not caught
the real installation behavior.

The manifest now reads CurrentBuildNumber from the 64-bit HKLM Windows version
key into secure UXPLAY_WINDOWS_BUILD before LaunchConditions. Seven condition
fixtures plus native Registry AppSearch passed; independent review confirmed
AppSearch precedes LaunchConditions in both UI and execute sequences.

Another session committed the accumulated changes as 28b360d during the build.
The HEAD guard stopped that build's receipt creation. Staging and packaging were
rerun against 28b360d3fa3b43bd9b97985e0d5e62f9ee2ca313 with a clean source tree.
Final MSI: rewrite/out/x64/artifacts/UxPlayRs-0.1.1-x64.msi, 109974220 bytes,
SHA-256 59735d99eb1250d992a308f8afd4caaaea786fa981f4741e85c8058033c70b62,
NotSigned. ICE, extraction, all 470 payload hashes and four extracted-FFmpeg
tests passed again.

Native msiexec /i /qn passed LaunchConditions with actual build 26300, then
failed with Error 1925 / exit 1603 because the non-elevated silent process could
not obtain administrator rights. Full installation was not completed. The
0.1.1 wizard was opened from Explorer, but computer-use policy blocked msiexec
window inspection. Installed GUI, upgrade/uninstall and installed-device
playback remain unverified; the existing receiver was left running.
