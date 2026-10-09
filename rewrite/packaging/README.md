# UxPlayRs packaging (rewrite)

GPL-3.0-or-later rewrite: Rust engine (`airplayd`) + C# WPF GUI (`UxPlayRs.Gui`).

## Staged bundle layout

`rewrite\out\<arch>\bundle\` (where `<arch>` is `x64` or `arm64`):

```text
bundle\
  UxPlayRs.Gui.exe   # WPF GUI (self-contained win-x64 / win-arm64 publish output, all files kept)
  airplayd.exe       # Rust engine, built for the requested Windows MSVC target
  ffmpeg.exe        # Validated static FFmpeg build, same architecture as the engine
  FFmpeg-LICENSE.txt # License supplied for this exact FFmpeg build
  FFmpeg-SOURCE.txt  # Source/provider information supplied for this exact FFmpeg build
  FFmpeg-VERSION.txt # Actual binary SHA-256, architecture, version and configure output
  VERSION.txt        # git SHA + UTC date written by stage.ps1
  resources\         # optional app resources staged alongside the exes (icons, etc.), if present
```

Rules:

- The GUI auto-launches `airplayd.exe` sitting next to it. Do not rename
  either exe, do not use single-file publish, and do not move `airplayd.exe`
  into a subfolder.
- Keep `ffmpeg.exe` next to `airplayd.exe`. The distributed bundle does not
  depend on FFmpeg being installed elsewhere or present on the user's PATH.
- On the first Start, Windows may show a UAC prompt so the GUI can add the
  `UxPlayRs AirPlay Receiver` inbound rule for Private networks. The rule is
  scoped to the bundled `airplayd.exe` path and is reused on later starts.
- Publish is self-contained (`win-x64` / `win-arm64`) so the bundle installs
  with zero extra dependencies (no .NET runtime install required).
- BLE beacon note: this bundle stages the GUI, engine and FFmpeg. Any BLE
  advertisement/beacon helper is out of scope here; pair over the same
  network via mDNS/AirPlay discovery instead.

## How staging works

1. Run `packaging\stage.ps1` with a static FFmpeg executable and its matching
   license/source notices. For example, from `rewrite\`:

   ```powershell
   .\packaging\stage.ps1 -Architecture x64 `
     -FFmpegPath 'C:\Tools\ffmpeg\ffmpeg.exe' `
     -FFmpegLicensePath 'C:\Tools\ffmpeg\FFmpeg-LICENSE.txt' `
     -FFmpegSourcePath 'C:\Tools\ffmpeg\FFmpeg-SOURCE.txt'
   ```

   If `-FFmpegPath` is omitted, staging looks for an application named
   `ffmpeg` on PATH. If a notice path is omitted, it checks the executable's
   directory for `FFmpeg-LICENSE.txt` then `LICENSE.txt`, or
   `FFmpeg-SOURCE.txt` then `SOURCE.txt`, respectively. Both notices are
   required and must describe the selected build; the script does not infer
   a license or provider from a machine-specific installation path.

   Staging validates the executable's PE architecture and uses a bounded
   Windows OS DLL allowlist, including Windows API/UCRT sets. VC/MinGW
   redistributable runtimes, shared FFmpeg libraries and unknown DLLs are
   rejected even when installed in the staging machine's System32 directory.
   It requires the native `h264` decoder and `ppm` encoder before the builds:

   - Engine: `cargo build --release --locked -p airplayd --target <target>`,
     with `x86_64-pc-windows-msvc` for x64 or `aarch64-pc-windows-msvc` for
     ARM64. Output is `rewrite\engine\target\<target>\release\airplayd.exe`.
     Install the corresponding Rust target and MSVC build/link tools first.
     FFmpeg must run on the staging host, so ARM64 staging requires a host
     that can execute the selected ARM64 binary.
   - GUI: `dotnet publish` of `rewrite\gui\UxPlayRs.Gui\` with
     `-c Release -r win-x64|win-arm64 --self-contained true`
     (NOT single-file, so `airplayd.exe` can sit side by side).
2. It clears/creates `rewrite\out\<arch>\bundle\`, copies the full GUI
   publish output, `airplayd.exe`, `ffmpeg.exe` and the supplied notices into
   it. It checks the copied FFmpeg hash, executes its version command from
   the bundle, writes `FFmpeg-VERSION.txt`, and writes `bundle\VERSION.txt`
   (git SHA + UTC date).
3. `packaging\zip.ps1 -Architecture x64|arm64` compresses the bundle to
   `rewrite\out\<arch>\artifacts\uxplay-rs-<arch>.zip` for release/CI upload.
   It refuses a bundle missing FFmpeg or any of its three metadata files.

## Single-file Windows installer

After staging the current source with `stage.ps1`, build the MSI from `rewrite\`:

```powershell
.\packaging\installer.ps1 -Architecture x64 -Version 0.1.1
```

Output: `rewrite\out\x64\artifacts\UxPlayRs-0.1.1-x64.msi`. The MSI embeds
its cabinet data, so this one file is sufficient for installation. It contains
the complete self-contained GUI, `airplayd.exe`, FFmpeg and notices, excluding
debug PDB files. No separate .NET or FFmpeg installation is required.

The installer uses the repository's pinned WiX 7 tool and UI extension, restored
through .NET. It validates the staged checkout/architecture and FFmpeg receipt,
builds a Traditional Chinese setup wizard, and runs Windows Installer ICE
validation. It also writes `.msi.sha256` and `.msi.receipt.json` sidecars with
the installer signature state, source HEAD/dirty state and each payload file's
size and SHA-256. These sidecars are build evidence, not installation inputs.
Always restage after changing engine or GUI sources; an unchanged HEAD does not
identify changes in an uncommitted worktree.

The minimum OS check reads `CurrentBuildNumber` from the 64-bit Windows
registry, rather than the MSI compatibility `WindowsBuild` value, which can
be 9600 on Windows 10/11. `verify-launch.ps1` evaluates the packaged condition
for supported and unsupported builds, 32-bit/missing-build cases and installed
maintenance, then executes the real registry AppSearch. This verification runs
as part of installer.ps1; it is separate from a real msiexec installation test.

Installation requires administrator rights, defaults to `Program Files\UxPlayRs`,
and allows choosing the directory. Start-menu and desktop shortcuts launch the
GUI with that directory as their working directory. Standard repair, upgrade
and uninstall are provided through Windows Installer. UxPlayRs has a distinct
upgrade identity from the original Qt `uxplay-windows` product, so installing
the rewrite does not replace it. Runtime user settings/pairing state are not
part of the MSI and remain in AppData after uninstall.

The existing application handles the Private-network firewall rule when Start
is first used; the MSI does not open ports or start a receiver automatically.
Exit an existing UxPlayRs instance before starting the installed copy because
the GUI is single-instance. Installer/archive/payload validation is separate
from actually installing and testing the GUI or mirroring on a physical phone.

## FFmpeg provenance

Use a fixed provider release/asset and verify its published checksum before
staging. FFmpeg itself publishes source releases and links Windows binary
providers at <https://ffmpeg.org/download.html>; staging does not download
or silently update a binary.

Supply the license shipped for the chosen build. In `FFmpeg-SOURCE.txt`,
record the provider and download URL, exact FFmpeg revision/version, source
URL, build recipe/configuration, binary SHA-256, and available source and
license information for external libraries included by that provider.
Clearly state any provenance that has not been verified. `FFmpeg-VERSION.txt`
records the binary actually staged; copying notices does not establish
that their contents cover all included external dependencies. Preserve
corresponding sources when distributing a release. See
<https://ffmpeg.org/legal.html> for the project's source/license guidance.
