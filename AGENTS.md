# Repository Guidelines

## Project Structure & Module Organization

This repository contains a single Rust CLI declared in `Cargo.toml`.

- `src/bin/usbipd-rs.rs`: USB enumeration, board detection, probing, driver diagnostics, installers, and unit tests.
- `examples/`: focused experiments, such as `nusb_stlink_probe.rs`.
- `windows-driver/`: bundled upstream drivers, archives, and helper binaries used by installer workflows.
- `.github/workflows/release.yml`: cross-platform build and tagged-release workflow.
- `README.md`: user-facing commands, supported boards, and safety notes.

Keep feature logic close to its existing subsystem. New boards normally require a `KnownBoard` entry; new probe types require a `ProbeKind` variant and matching `run_*`/`print_*` functions.

## Build, Test, and Development Commands

```powershell
cargo build
cargo build --release
cargo test
$env:RUSTFLAGS="-D warnings"; cargo build --release --all-targets
cargo run --release -- --driver-status
cargo run --release -- --mcu-alive
cargo run --release --example nusb_stlink_probe
```

Use `cargo test` for unit tests and the warning-clean release build before submitting. Hardware commands require connected devices; document the tested VID:PID and observed result.

## Coding Style & Naming Conventions

Follow standard Rust naming: `snake_case` functions/modules, `CamelCase` types, and `SCREAMING_SNAKE_CASE` constants. Use four-space indentation and keep comments focused on non-obvious protocol or safety behavior.

Run `cargo fmt --check` for new code, but avoid unrelated whole-file formatting churn. Prefer existing patterns and `anyhow::Context` for actionable errors.

## Testing Guidelines

Unit tests live in the `#[cfg(test)] mod tests` block in `src/bin/usbipd-rs.rs`. Name tests after observable behavior, for example `stlink_layer2_requires_request_and_successful_layer1`.

Add tests for parsers, decoders, classification, and probe gating. Never require destructive hardware actions in automated tests.

## Commit & Pull Request Guidelines

Use concise imperative commit subjects, matching history such as `Add native macOS USB listing`. Releases use `Release vX.Y.Z`.

Pull requests should describe behavior changes, commands run, OS tested, and connected hardware when relevant. Update `--help` and `README.md` when changing CLI flags, `KNOWN_BOARDS`, or installer entries. Do not commit generated logs, firmware dumps, or newly downloaded driver bundles unless intentionally required.

## Hardware Safety

Default diagnostics should remain read-only. Clearly label commands that halt, reset, erase, unlock, flash, or replace Windows drivers. Never assume USB enumeration proves the downstream MCU is healthy.
