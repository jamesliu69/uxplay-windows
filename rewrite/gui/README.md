# UxPlayRs GUI (C# / WPF, .NET 8)

Windows GUI for the pure-Rust AirPlay 2 receiver engine (`../engine`,
`airplayd.exe`). Controls the engine over a named pipe using NDJSON
JSON-RPC 2.0 (`\\.\pipe\uxplay-rs-airplayd`).

## Projects

- `UxPlayRs.Gui` — WPF app (net8.0-windows): system-tray icon, main window
  (device name / resolution / FPS / audio-only / run-at-login), log viewer,
  settings persistence (`%APPDATA%\uxplay-rs\settings.json`), engine process
  management (finds `airplayd.exe` next to the GUI, else the dev-built engine
  under `../engine/target/{release,debug}`), and a first-run Windows Defender
  Firewall rule for `airplayd.exe` on Private networks.
- `UxPlayRs.Gui.Tests` — xUnit tests: IPC round-trip over TCP loopback,
  camelCase contract checks, settings save/load, log ring buffer.

IPC contract details live in `UxPlayRs.Gui/Ipc/EngineProtocol.cs` and must
match the engine's `airplay-ipc` crate (`EngineParams`, `StatusResponse`,
method names `status`/`start`/`stop`/`set_params`, notifications `state`/
`log`/`video`).

## Build / test

```powershell
Set-Location rewrite\gui
dotnet build UxPlayRs.slnx
dotnet test UxPlayRs.slnx
```

No real `airplayd.exe` is needed for tests (a TCP-loopback fake engine
stands in). For a live end-to-end check, start the engine first:

```powershell
cargo run -p airplayd --manifest-path ..\engine\Cargo.toml
```

then run the GUI (`dotnet run --project UxPlayRs.Gui`) and press Start.

## License

GPL-3.0-or-later (rewrite of GPL uxplay-windows).
