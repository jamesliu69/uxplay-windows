# Repository Guidelines

## Project Structure & Module Organization

`src/` contains the Qt 6/C++ application. `libuxplay/` is the upstream Git submodule; initialize it with `git submodule update --init --recursive` before building. Build helpers live in `scripts/`, packaging manifests in `packaging/`, Bonjour patches in `mdnsresponder-patches/`, documentation in `docs/`, and icons or other assets in `stuff/`. GitHub Actions workflows are in `.github/workflows/`. Generated build output is placed under `out/<arch>/`, with staging output in `release/` and the cached Bonjour SDK in `Bonjour SDK/`.

## Build, Test, and Development Commands

Run commands from PowerShell at the repository root. Replace `x64` with `arm64` to target the other supported architecture.

```powershell
.\build.ps1 bootstrap -Architecture x64  # install or update prerequisites
.\build.ps1 build -Architecture x64      # compile without packaging
.\build.ps1 package -Architecture x64    # build, verify, create ZIP and MSI
.\build.ps1 test -Architecture x64       # validate an existing release bundle
.\build.ps1 clean -Architecture x64      # remove that architecture's outputs
```

Use `package -SkipInstaller` to create a portable ZIP without the WiX MSI.

## Coding Style & Naming Conventions

Use C++17 and follow the surrounding Qt code: four-space indentation, PascalCase for classes, camelCase for methods and locals, and `m_` prefixes for member fields. PowerShell functions use PascalCase. No formatter or linter is configured, so keep changes consistent with nearby files and the existing CMake target structure.

## Testing Guidelines

There is no separate unit-test framework or test directory. `build.ps1 test` runs the isolated runtime validation against an existing staged bundle; `package` also performs runtime verification and is the main CI validation path. Run the relevant architecture command for changes that affect bundling or native dependencies.

## Commit & Pull Request Guidelines

Recent commits use short English imperative or fix-oriented subjects, without a strict prefix convention. Keep commits focused. Pull requests should explain the change, link a related issue when applicable, list affected architectures and the commands run, and include screenshots for visible UI changes. The build workflow runs for pull requests targeting `main` or `multi-arch`.

## Security & Configuration

Keep signing credentials in GitHub Actions secrets and out of source files. Avoid committing machine-specific paths or generated artifacts.
