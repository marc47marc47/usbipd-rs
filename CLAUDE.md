# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A single-binary Rust CLI that lists connected USB devices (in `usbipd list`
format), flags "probable boards" by VID:PID, and probes each with a
chip-specific external tool (espflash / avrdude / picotool / DAPLink / pyocd).

## Commands

```bash
cargo build --release                 # build
cargo run --release -- --probe        # run with args after `--`
cargo run --release -- --list-tools   # e.g. inspect installer status
```

There is no test suite — the crate ships zero `#[test]`/`#[cfg(test)]` code.
CI builds with `RUSTFLAGS: "-D warnings"`, so **`cargo build` must be
warning-clean** or the GitHub Actions build fails.

`Cargo.lock` is gitignored; CI resolves dependencies fresh per run.

## Architecture

All logic lives in one file: `src/bin/usbipd-rs.rs` (no `lib.rs`, no
`src/main.rs`; the binary is declared via `[[bin]]` in `Cargo.toml`). It is
two largely independent subsystems.

### 1. USB listing + board probing (default command, `--probe`)

- `run_usbipd_list()` shells out to `usbipd.exe list` and `parse_usbipd()`
  turns its text columns into `Vec<Entry>`. `nusb_by_vidpid()` enriches each
  row with USB speed via the `nusb` crate.
- `KNOWN_BOARDS: &[KnownBoard]` is a static VID:PID → `(name, &[ProbeKind])`
  table; `lookup_board(vid, pid)` matches a device against it.
- With `--probe`, `probe_boards()` runs each matched board's `ProbeKind`
  pipeline. Each `ProbeKind` variant (`Espflash`, `Avrdude`, `Picotool`,
  `Daplink`, `Pyocd`) shells out to an external CLI and has a paired
  `run_*` function (invoke + parse output into a `HashMap`) and `print_*`
  function (format). A board's `probes` is a slice, so one device can chain
  probes — e.g. the DAPLink entry uses `[Daplink, Pyocd]`.

### 2. Tool installer (`--install`, `--list-tools`)

- `TOOLS: &[ToolSpec]` lists installable dependencies. Each `ToolSpec.resolve`
  maps the current `Os` to an `InstallStep`:
  - `Command` — run a package manager (`cargo install …`, `brew`, `apt`).
  - `Download` — fetch a URL, then a `DownloadAction` (extract to bundle,
    extract + prompt, or save + prompt).
  - `Manual` — print a URL + steps and fetch nothing; for vendors whose
    server blocks automated downloads (e.g. FTDI returns HTTP 403).
- `cmd_install()` executes the step; downloads cache under `windows-driver/`
  on Windows or `tools/` elsewhere.

### Cross-cutting facts

- Probe tools are invoked by bare name (`Command::new("avrdude")`) so they
  must be on `PATH`. `picotool` is the exception: `find_picotool()` searches
  bundled paths and the `PICOTOOL_PATH` env var.
- **Probing resets the target chip** (toggles DTR/RTS or asserts SWD reset).
  Never probe a port with live firmware on it.
- Listing source is OS-dispatched in `list_entries()`: Windows parses
  `usbipd.exe list` (for the share/attach STATE column); macOS/Linux enumerate
  natively via `nusb` in `nusb_entries()` (BUSID synthesized as
  `bus-address`, STATE blank). All sub-commands (`--probe`, `--install`) work
  on every OS.
- avrdude probes run with `-F` (override the signature check, so a mismatched
  MCU still reports its true signature) and a list of candidate baud rates
  tried in order.

## Extending

- **New board:** add a `KnownBoard` entry to `KNOWN_BOARDS`. Reuse an existing
  `ProbeKind` if possible; a new probe type needs a `ProbeKind` variant plus
  its `run_*`/`print_*` functions and the match arm in `probe_boards()`.
- **New installable tool:** add a `ToolSpec` to `TOOLS` (it then appears
  automatically in `--list-tools`).
- The `--help` text in `print_help()` and the VID:PID / installer tables in
  `README.md` are hand-maintained — update them whenever you change
  `KNOWN_BOARDS` or `TOOLS`.

## Releasing

Bump `version` in `Cargo.toml`, commit as `Release vX.Y.Z`, and push a
`vX.Y.Z` tag. `.github/workflows/release.yml` builds four targets
(windows-x86_64, macos-aarch64, linux-x86_64, linux-aarch64) on `v*` tags;
pushes to `main` trigger a plain build.

`windows-driver/` holds ~30 MB of committed driver bundles plus the
`picotool`/`avrdude` binaries the program locates at runtime.
