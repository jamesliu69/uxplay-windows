# UxPlayRs packaging (rewrite)

GPL-3.0-or-later rewrite: Rust engine (`airplayd`) + C# WPF GUI (`UxPlayRs.Gui`).

## Staged bundle layout

`rewrite\out\<arch>\bundle\` (where `<arch>` is `x64` or `arm64`):

```text
bundle\
  UxPlayRs.Gui.exe   # WPF GUI (self-contained win-x64 / win-arm64 publish output, all files kept)
  airplayd.exe       # Rust engine (cargo build --release -p airplayd), must sit NEXT TO the GUI exe
  VERSION.txt        # git SHA + UTC date written by stage.ps1
  resources\         # optional app resources staged alongside the exes (icons, etc.), if present
```

Rules:

- The GUI auto-launches `airplayd.exe` sitting next to it. Do not rename
  either exe, do not use single-file publish, and do not move `airplayd.exe`
  into a subfolder.
- On the first Start, Windows may show a UAC prompt so the GUI can add the
  `UxPlayRs AirPlay Receiver` inbound rule for Private networks. The rule is
  scoped to the bundled `airplayd.exe` path and is reused on later starts.
- Publish is self-contained (`win-x64` / `win-arm64`) so the bundle installs
  with zero extra dependencies (no .NET runtime install required).
- BLE beacon note: this bundle stages only the GUI + engine. Any BLE
  advertisement/beacon helper is out of scope here; pair over the same
  network via mDNS/AirPlay discovery instead.

## How staging works

1. `packaging\stage.ps1 -Architecture x64|arm64` builds both parts from source:
   - Engine: `cargo build --release -p airplayd` in `rewrite\engine\`
     (output: `rewrite\engine\target\release\airplayd.exe`).
   - GUI: `dotnet publish` of `rewrite\gui\UxPlayRs.Gui\` with
     `-c Release -r win-x64|--win-arm64 --self-contained true`
     (NOT single-file, so `airplayd.exe` can sit side by side).
2. It clears/creates `rewrite\out\<arch>\bundle\`, copies the full GUI
   publish output plus `airplayd.exe` into it, verifies both
   `UxPlayRs.Gui.exe` and `airplayd.exe` exist, and writes
   `bundle\VERSION.txt` (git SHA + UTC date).
3. `packaging\zip.ps1 -Architecture x64|arm64` compresses the bundle to
   `rewrite\out\<arch>\artifacts\uxplay-rs-<arch>.zip` for release/CI upload.
