# usbipd-rs

USB device inspector with chip-level board probing. Lists connected USB
devices in the same format as `usbipd list`, then drills into each one with
the right protocol-specific tool — `espflash`, `stm32flash`, `avrdude`,
`picotool`, DAPLink `DETAILS.TXT`, or `pyocd`.

> **Status:** built and used daily on Windows 11 with `usbipd-win`.
> macOS / Linux are supported too: there the listing layer enumerates USB
> natively via `nusb` (no `usbipd.exe` needed), and the `--probe` / `--install`
> sub-commands work as on Windows. The usbip share/attach STATE column is
> Windows-only.

---

## Highlights

- One command to inspect every connected USB device — VID:PID, BUSID, COM port, USB speed, attach state.
- Six probe back-ends, dispatched by VID:PID:
  - **`espflash`** — ESP32 / ESP32-S2 / S3 / C3 / H2 chip ID, MAC, flash size, features.
  - **`stm32flash`** — STM32 / GD32 (and bootloader-compatible clones) Device ID, flash / RAM size, option bytes via UART system bootloader.
  - **`avrdude`** — ATmega328P / 328PB / 2560 / 32U4 signature, programmer type, bootloader version.
  - **`picotool`** — RP2040 / RP2350 chip rev, package, flash size, ROM gitrev, dual-arch ARM/RISC-V info.
  - **DAPLink** — read `DETAILS.TXT` from the mass-storage drive (zero deps), decode board variant + target.
  - **`pyocd`** — cross-check the SWD-side target chip via pyOCD's board database (no chip reset).
- Boards can chain probes — micro:bit V2 runs both `DETAILS.TXT` *and* `pyocd` for cross-verification.
- Built-in installer for the ten external tools (downloads from upstream, extracts, prints UAC-aware instructions).
- Cross-platform builds via GitHub Actions: Windows x86_64, macOS aarch64, Linux x86_64, Linux aarch64.

## Example output

```
$ usbipd-rs --probe
BUSID  VID:PID    DEVICE                                STATE       SPEED
-------------------------------------------------------------------------
1-11   0d28:0204  USB Mass Storage Device, COM7, ...    Not shared  Full

=== Board Probe ===

[1-11  DAPLink (mbed CMSIS-DAP)  via DETAILS.TXT]
  Board:               DAPLink (mbed CMSIS-DAP)
  Unique ID:           9905360200052833a247b5a43c0a3b04...
  HIC ID:              6e052820
  DAPLink mode:        Interface
  Interface FW:        0256
  Variant:             BBC micro:bit V2.21 (later)
  Target chip:         nRF52833 — Cortex-M4F, 128 KB SRAM, 512 KB flash, BLE 5.x

[1-11  DAPLink (mbed CMSIS-DAP)  via pyocd]
  Probe vendor:        Arm
  Board name:          BBC micro:bit V2
  Target chip:         nrf52833
```

(One micro:bit V2 inspected via two independent probes — filesystem
metadata and USB serial-number → board database lookup.)

## Boards detected (VID:PID → probe)

| VID:PID family            | Board                                | Probe                       |
| ------------------------- | ------------------------------------ | --------------------------- |
| `10C4:EA60` / `EA70` / `EA71` | Silabs CP210x bridge             | `espflash` → `stm32flash` → `avrdude` |
| `1A86:7523` / `55D4`      | WCH CH340 / CH9102                   | `espflash` → `stm32flash` → `avrdude` |
| `0403:6010` / `6014` / `6015` | FTDI FT2232 / FT232H / FT231X    | `espflash` → `stm32flash` → `avrdude` |
| `0403:6001`               | FTDI FT232R (Arduino Nano/Duemilanove) | `nusb` descriptors → `avrdude` |
| `303A:1001` / `4001`      | ESP32 native USB-Serial-JTAG / OTG   | `espflash`                  |
| `0483:DF11`               | STM32 USB DFU bootloader (DfuSe)     | `dfu-util` (memory map)     |
| `2341:0001` / `0043`      | Arduino Uno R1 / R3                  | `avrdude`                   |
| `2341:0010` / `0042` / `0044` | Arduino Mega 2560 / Mega ADK     | `avrdude`                   |
| `2341:8036` / `8037`      | Arduino Leonardo / Micro             | `avrdude`                   |
| `2E8A:0003`               | RP2040 BOOTSEL (Pi Pico)             | `picotool`                  |
| `2E8A:000F`               | RP2350 BOOTSEL (Pi Pico 2)           | `picotool`                  |
| `0D28:0204`               | mbed CMSIS-DAP (BBC micro:bit, FRDM) | `DETAILS.TXT` + `pyocd`     |

## Install

### From release

Pre-built binaries for v0.3.0+ are attached to GitHub
[Releases](https://github.com/marc47marc47/usbipd-rs/releases). Pick
the archive for your platform, extract, and put the binary somewhere on
your `PATH`.

### From source

```bash
git clone https://github.com/marc47marc47/usbipd-rs.git
cd usbipd-rs
cargo build --release
# Binary lands at target/release/usbipd-rs(.exe)
```

Linux build needs libudev:

```bash
sudo apt-get install libudev-dev pkg-config
```

### Windows: usbipd-win prerequisite

On Windows the list command shells out to `usbipd.exe list`. Install
[usbipd-win](https://github.com/dorssel/usbipd-win) first:

```powershell
winget install --interactive --exact dorssel.usbipd-win
```

On macOS / Linux this isn't needed — listing enumerates USB directly via
`nusb`.

## Usage

```bash
usbipd-rs                      # list USB devices, flag probable boards
usbipd-rs --probe              # also pull chip-level info from each board
usbipd-rs --list-tools         # show install status of all probe dependencies
usbipd-rs --install picotool   # auto-download a tool from upstream
usbipd-rs --help               # full reference
```

## Probe-tool installer

`--install <ID>` knows how to fetch / launch each tool on each platform:

| ID         | Provider                              | Use case                                  |
| ---------- | ------------------------------------- | ----------------------------------------- |
| `espflash`   | `cargo install espflash`              | ESP32 chip identification / flashing             |
| `pyocd`      | `pip install pyocd`                   | CMSIS-DAP / DAPLink target chip ID               |
| `picotool`   | GitHub release zip                    | Pi Pico (RP2040 / RP2350) inspection             |
| `avrdude`    | GitHub release zip / brew / apt       | AVR (Arduino) chip ID & flashing                 |
| `stm32flash` | bundled zip in `windows-driver/`      | STM32 / GD32 UART-bootloader chip ID & flashing (Win / Linux / macOS binaries) |
| `dfu-util`   | `brew` / `apt` / manual (Windows)     | STM32 USB DFU (`0483:DF11`) memory-map / chip-family ID & flashing |
| `ravedude`   | `cargo install ravedude`              | avr-hal `cargo run` runner (Rust AVR dev)        |
| `arduino-cli`| download (Win / Linux) / `brew` (macOS) | Arduino core manager — bundles avrdude (`core install arduino:avr`), esptool, etc. |
| `zadig`      | libwdi GitHub release                 | Win-only: replace USB driver → WinUSB            |
| `cp210x`     | silabs.com universal driver           | Win-only: CP2102/CP2104 VCP driver               |
| `ch340`      | wch-ic.com `CH341SER.EXE`             | Win-only: CH340/CH341 USB-Serial driver          |
| `ftdi`       | ftdichip.com CDM (manual download)    | Win-only: FTDI FT232R VCP driver                 |

Files cache under `windows-driver/` (Windows) or `tools/` (macOS / Linux);
re-running `--install <id>` reuses an existing download — delete it to
force a fresh fetch.

## Architecture

```
                       ┌─────────────────────────────┐
                       │  KNOWN_BOARDS lookup        │
                       │  (VID:PID → &[ProbeKind])   │
                       └──────────┬──────────────────┘
                                  ↓
              ┌─────────────┬─────────────┬─────────────┬─────────┬──────────┐
              ↓             ↓             ↓             ↓         ↓          ↓
      ┌──────────────┬─────────────┬─────────────┬───────────┬──────────┐
      │  Espflash    │  Avrdude    │  Picotool   │  Daplink  │  Pyocd   │
      ├──────────────┼─────────────┼─────────────┼───────────┼──────────┤
      │ COM port     │ COM port    │ libusb      │ MSD scan  │ libusb   │
      │ espflash bin │ avrdude bin │ picotool bin│ DETAILS.  │ pyocd    │
      │              │             │             │   TXT     │   bin    │
      └──────────────┴─────────────┴─────────────┴───────────┴──────────┘
```

A `KnownBoard.probes` is `&'static [ProbeKind]`, so a single device can
fan out into multiple probe back-ends. The micro:bit entry uses
`[Daplink, Pyocd]` for cross-checking; the Pi Pico entries use
`[Picotool]` only.

## Notes

- **Probing resets the chip.** All five back-ends will toggle DTR/RTS or
  assert SWD reset on the target. Don't run `--probe` against a
  3D-printer's Marlin board (or any other live firmware) on the same
  USB port.
- **Pi Pico BOOTSEL on Windows needs WinUSB.** A one-time driver swap
  is required for `picotool` to access the PICOBOOT vendor interface.
  Run `usbipd-rs --install zadig` and follow the printed steps.
- **`Cargo.lock` is currently gitignored.** CI resolves dependency
  versions fresh per run. For reproducible builds, commit `Cargo.lock`
  and add `--locked` to the workflow.
- **Bundled drivers ship with the repo** (~30 MB) so a fresh Windows
  setup can install everything offline. The release archives ship the
  binary only — pull tools on demand via `--install`.

## Dependencies

- [`nusb`](https://crates.io/crates/nusb) — pure-Rust USB enumeration (the
  native listing source on macOS / Linux)
- [`serialport`](https://crates.io/crates/serialport) — COM port mapping
- [`ureq`](https://crates.io/crates/ureq) — HTTP client for installer downloads
- [`zip`](https://crates.io/crates/zip) — archive extraction
- [`unicode-width`](https://crates.io/crates/unicode-width) — East-Asian-aware table padding
- [`anyhow`](https://crates.io/crates/anyhow) — flexible error handling

## Acknowledgements

- [`usbipd-win`](https://github.com/dorssel/usbipd-win) — supplies the
  Windows USB enumeration via `usbipd.exe list` (macOS / Linux use `nusb`).
- [`espflash`](https://github.com/esp-rs/espflash),
  [`avrdude`](https://github.com/avrdudes/avrdude),
  [`picotool`](https://github.com/raspberrypi/picotool),
  [`pyocd`](https://github.com/pyocd/pyOCD) — the upstream tools
  this wrapper orchestrates.
- [`Zadig`](https://zadig.akeo.ie/) by Pete Batard — the WinUSB driver
  swap for libusb-only USB devices.

## License

[GPL-3.0-only](LICENSE). You're free to use, modify, and redistribute,
provided derivative works are also released under GPL-3.0.
