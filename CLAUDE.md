# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A single-binary Rust CLI that lists connected USB devices natively via
`nusb`, flags "probable boards" by VID:PID, and probes each with a
chip-specific external tool (espflash / avrdude / picotool / DAPLink / pyocd).

## Commands

```bash
cargo build --release                 # build
cargo run --release -- --probe        # run with args after `--`
cargo run --release -- --list-tools   # e.g. inspect installer status
cargo test                            # unit tests (pure decoders/parsers/classifiers)
```

The crate ships a `#[cfg(test)] mod tests` at the bottom of
`src/bin/usbipd-rs.rs` covering the pure logic: ST-Link controller/target
decoders, SWD failure classification, the driver-issue classifier
(`classify_driver_issue`), driver advice/INF lookup, and the `pnputil`
driver-store parser. Keep new pure helpers testable and add cases there.
CI builds with `RUSTFLAGS: "-D warnings"`, so **`cargo build` must be
warning-clean** or the GitHub Actions build fails.

`Cargo.lock` is gitignored; CI resolves dependencies fresh per run.

## Architecture

All logic lives in one file: `src/bin/usbipd-rs.rs` (no `lib.rs`, no
`src/main.rs`; the binary is declared via `[[bin]]` in `Cargo.toml`). It is
two largely independent subsystems.

### 1. USB listing + board probing (default command, `--probe`)

- `nusb_entries()` enumerates connected USB devices directly and turns them
  into `Vec<Entry>`. `nusb_by_vidpid()` enriches each row with USB speed.
- `KNOWN_BOARDS: &[KnownBoard]` is a static VID:PID → `(name, &[ProbeKind])`
  table; `lookup_board(vid, pid)` matches a device against it.
- With `--probe`, `probe_boards()` runs each matched board's `ProbeKind`
  pipeline. Most `ProbeKind` variants (`Espflash`, `Avrdude`, `Picotool`,
  `Daplink`, `Pyocd`) shell out to an external CLI and have a paired
  `run_*` function (invoke + parse output into a `HashMap`) and `print_*`
  function (format). A board's `probes` is a slice, so one device can chain
  probes — e.g. the DAPLink entry uses `[Daplink, Pyocd]`.
- The ST-Link entry uses `[Stlink, StlinkTarget]`. `Stlink` (layer 1) names the
  controller from VID:PID/descriptors. `StlinkTarget` (layer 2) is **not** an
  external tool: `run_stlink_target_query()` reuses the native `StlinkLink`
  reader to enter SWD and read the downstream MCU's identity read-only. It runs
  automatically under `--probe` (no separate flag) and auto-detects target
  presence — when nothing answers it returns a one-layer result. There is no
  `--probe-layer2`.

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
- **Serial-bootloader probing resets the target chip** (espflash / stm32flash /
  avrdude toggle DTR/RTS). Never run those on a port with live firmware. The
  ST-Link layer-2 step is the exception: it is read-only native SWD (enter +
  read ID registers, no halt/reset/erase/write).
- `list_entries()` uses `nusb_entries()` on every OS. BUSID is synthesized
  from the native bus and physical `port_chain()` topology; native enumeration
  does not depend on or expose usbip share/attach state. All sub-commands
  (`--probe`, `--install`) work on every OS.
- avrdude probes run with `-F` (override the signature check, so a mismatched
  MCU still reports its true signature) and a list of candidate baud rates
  tried in order.

### Native ST-Link SWD reader (`--mcu-alive-native`)

- `cmd_mcu_alive_native()` speaks the ST-Link bulk command protocol directly
  via `nusb` (no probe-rs / pyocd / stlink). `StlinkLink` wraps the two bulk
  endpoints (`0x01`/`0x81`, or `0x02` on the original V2) and implements
  GET_VERSION, GET_TARGET_VOLTAGE, ENTER_SWD, READ_IDCODES, READDEBUGREG. The
  sequence is read-only — no halt/reset/erase/write. Opcodes/byte-layouts were
  cross-checked against `../probe-rs`, `../stlink`, `../openocd`.
- Windows gotchas baked in: bulk IN length must be rounded up to the endpoint
  max packet size (else WinUSB returns `InvalidArgument`); `open()` does
  `clear_halt` + a short drain to recover from an interrupted prior command;
  the flash-size field is read at an aligned address and the correct half-word
  extracted (it sits at `…A22` on F4/F7). This path needs only WinUSB on
  `MI_00` (`--install-driver`); `claim_interface` failing is the no-driver
  boundary, reported explicitly.
- STM32 identification: `stm_family()` (+ `ADDRS_*`, `builtin_sram_kb`) is the
  compiled-in base table. `load_chip_db()` reads optional `etc/chips/*.chip`
  files (stlink subset format) and `resolve_chip()` prefers a matching `.chip`
  over the built-in table, keeping the verified UID/RDP addresses from built-in.
  `cortex_core()` / `decode_rdp()` are shared with the pyocd path's
  `decode_stlink_regs()`.

## Extending

- **New board:** add a `KnownBoard` entry to `KNOWN_BOARDS`. Reuse an existing
  `ProbeKind` if possible; a new probe type needs a `ProbeKind` variant plus
  its `run_*`/`print_*` functions and the match arm in `probe_boards()`.
- **New installable tool:** add a `ToolSpec` to `TOOLS` (it then appears
  automatically in `--list-tools`).
- **New STM32 model (native reader):** add a `dev_id` arm to `stm_family()`
  (and `builtin_sram_kb`) for a compiled-in entry, OR drop a `etc/chips/*.chip`
  file to add/override one without recompiling. Keep `etc/chips/*.chip`
  templates in sync with `pack-release.sh`, which bundles them in the archive.
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
