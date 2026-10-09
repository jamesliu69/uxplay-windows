# uxplay-rs rewrite (two-language AirPlay 2 receiver)

Full rewrite of uxplay-windows with zero shared code with the original
(C++/Qt/libuxplay). License: GPL-3.0-or-later.

## Layout rule: one language, one folder

Each language project lives **entirely in its own top-level folder** and must
stay independently buildable. Cross-language contact happens only through
versioned wire/file contracts, never through shared source.

| Folder | Language | Builds with | Contract surface |
|---|---|---|---|
| `engine/` | Rust (workspace) | `cargo test --workspace` / `cargo build -p airplayd` | Named-pipe NDJSON JSON-RPC (`airplay-ipc` crate = contract owner); mDNS TXT values |
| `gui/` | C# / WPF (.NET 8) | `dotnet build` / `dotnet test` | `Ipc/EngineProtocol.cs` (must mirror `airplay-ipc`) |
| `packaging/` | PowerShell + WiX/YAML | `stage.ps1` / `zip.ps1` | Bundle layout only (exes side-by-side); never imports language sources |

Rules:

1. No `.rs` outside `engine/`, no `.cs`/`.xaml` outside `gui/` (CI can grep this).
2. `engine/` builds with only `cargo`; `gui/` builds with only `dotnet`.
   Neither build reads the other's directory (the GUI locates `airplayd.exe`
   at *runtime*, not at build time).
3. If the two projects ever move to separate repositories, the only files
   that must be duplicated/ versioned together are the IPC contract
   (`airplay-ipc/src/lib.rs` ↔ `Ipc/EngineProtocol.cs`) and this README.
4. Build outputs go to `out/<arch>/` (git-ignored); never commit binaries.

## Status

See `../.copilot` session plan (or ask the agent). Milestones: engine crates
→ IPC → GUI → packaging → engine wiring → A/V tuning → iPhone e2e.
