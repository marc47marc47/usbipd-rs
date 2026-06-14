# usbipd-rs

USB device inspector with chip-level board probing. Lists connected USB
devices natively via `nusb`, then drills into each one with
the right protocol-specific tool — `espflash`, `stm32flash`, `avrdude`,
`picotool`, DAPLink `DETAILS.TXT`, or `pyocd`.

> **Status:** built and used daily on Windows 11. Windows, macOS, and Linux
> enumerate USB buses and physical port topology natively via `nusb`;
> `usbipd-win` and `usbipd.exe` are not required.

---

## Highlights

- One command to inspect every connected USB device — VID:PID, BUSID, COM port, and USB speed.
- Seven probe back-ends, dispatched by VID:PID:
  - **`espflash`** — ESP32 / ESP32-S2 / S3 / C3 / H2 chip ID, MAC, flash size, features.
  - **`stm32flash`** — STM32 / GD32 (and bootloader-compatible clones) Device ID, flash / RAM size, option bytes via UART system bootloader.
  - **`avrdude`** — ATmega328P / 328PB / 2560 / 32U4 signature, programmer type, bootloader version.
  - **`picotool`** — RP2040 / RP2350 chip rev, package, flash size, ROM gitrev, dual-arch ARM/RISC-V info.
  - **DAPLink** — read `DETAILS.TXT` from the mass-storage drive (zero deps), decode board variant + target.
  - **`pyocd`** — cross-check the SWD-side target chip via pyOCD's board database (no chip reset).
  - **ST-Link layer 1** — identify the USB-visible debug-controller MCU and architecture from the ST-Link VID:PID and cached USB descriptors. Does not open SWD/JTAG or touch the downstream target.
  - **ST-Link layer 2** — `--probe` automatically detects a downstream SWD/JTAG target and, when one answers, reads its STM32 identity, flash size, SRAM, UID, and RDP **read-only over native SWD** (`nusb`, no probe-rs/pyocd). It enters SWD and reads ID registers only — no halt, reset, erase, unlock, dump, or write. Models come from a built-in table that `etc/chips/*.chip` files extend/override.
- Boards can chain probes — micro:bit V2 runs both `DETAILS.TXT` *and* `pyocd` for cross-verification.
- Built-in installer for the ten external tools (downloads from upstream, extracts, prints UAC-aware instructions).
- Cross-platform builds via GitHub Actions: Windows x86_64, macOS aarch64, Linux x86_64, Linux aarch64.

## Example output

```
$ usbipd-rs --probe
BUSID  VID:PID    DEVICE                                SPEED
-------------------------------------------------------------
1-11   0d28:0204  USB Mass Storage Device, COM7, ...    Full

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

An ST-Link/V2-1 first reports the USB-visible debug-controller layer. The
downstream SWD/JTAG target is deliberately left untouched:

```
[4-3  ST-Link/V2-1 debug controller  0483:374b  via nusb (layer 1 only)]
  Architecture:
  Layer:                   1 - USB debug controller
  Generation:              ST-Link/V2-1
  Controller MCU:          STM32F103CBT6
  Core:                    Arm Cortex-M3
  Architecture:            Armv7-M, Thumb/Thumb-2
  Max clock:               72 MHz
  Flash:                   128 KB
  SRAM:                    20 KB
  Downstream:              SWD (Nucleo on-board target link)
  Firmware access:         Not attempted; read-protection status unknown
  Layer 2:                 Not probed; no SWD/JTAG command issued
```

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
| `0483:3748` / `374B` / `374E` / `374F` | ST-Link V2 / V2-1 / V3 controller | `nusb` layer-1 architecture only |

`usbipd-rs --probe` automatically performs the ST-Link layer-2 read when an
ST-Link is present: after the layer-1 controller info it opens SWD natively and,
**if a downstream target answers**, prints its identity; if nothing answers it
shows a concise one-layer result instead. This step is read-only (it never
halts, resets, erases, unlocks, or writes the target). On Windows it needs a
WinUSB binding on `MI_00` (`usbipd-rs --install-driver`); without it the
layer-2 line reports the missing binding rather than guessing.

The layer-2 read proceeds in stages, each shown in the output:

1. Open + claim the ST-Link `MI_00` debug interface.
2. Read the probe firmware version and target voltage.
3. Enter SWD and read the DP IDCODE (presence of a target).
4. Read CPUID and DBGMCU_IDCODE (core + STM32 family).
5. Read flash size, SRAM, unique ID, and read-out protection.

No erase or flash operation is ever performed.

At the end, `--probe` prints a **Suggested installs** section tailored to the
detected board(s): (1) the driver needed so the host can talk to the board
(e.g. `usbipd-rs --install-driver --confirm` for an ST-Link, or skipped when one
is already bound), and (2) the Rust flashing tool that fits it
(`cargo install probe-rs-tools` for SWD/JTAG parts, `espflash` for ESP,
`ravedude` for AVR).

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

USB listing has no external runtime prerequisite on any supported OS.

## Usage

```bash
usbipd-rs                      # list USB devices, flag probable boards
usbipd-rs --probe              # also pull chip-level info from each board
usbipd-rs --mcu-alive          # minimal ST-Link/SWD target-alive test
usbipd-rs --driver-status      # Windows: diagnose every USB function driver
usbipd-rs --install-driver     # Windows: dry-run the ST-Link MI_00 driver install
usbipd-rs --install-driver --confirm  # ...and apply it (Administrator shell)
usbipd-rs --list-tools         # show install status of all probe dependencies
usbipd-rs --install picotool   # auto-download a tool from upstream
usbipd-rs --help               # full reference
```

To test the exact information available directly through `nusb` without
claiming or sending commands to an ST-Link debug interface:

```bash
cargo run --release --example nusb_stlink_probe
```

On Windows, `nusb` can obtain cached device, configuration, interface, and
endpoint descriptors through the USB hub. Actual transfers through the
vendor-specific ST-Link debug interface still use the Windows WinUSB backend
and require a compatible driver for that interface.

`--driver-status` separates hardware enumeration from Windows function-driver
health. It reports each present USB interface's PnP status, problem code,
service, INF, provider, and version. For a failed known interface it also
prints the matching `nusb` descriptor evidence and the official driver/install
route when known. Each failed interface is also classified (missing function
driver vs. failed load vs. stopped node vs. signature/blocked/conflict) with a
safe next action, and — when this repo bundles the matching INF — whether that
INF is available locally and whether the package is already staged in the
Windows driver store. A successful USB descriptor read proves that the USB-facing
controller enumerates; it does not by itself prove every downstream MCU or
external circuit is healthy.

`--install-driver` is the narrow, opt-in counterpart to `--driver-status`. It
targets only the ST-Link `MI_00` debug interface — the one binding this repo
ships an INF for — and never touches the device's Mass Storage, COM, or
composite functions. With no argument it is a dry run: it prints the exact
instance that would be modified, its classified current state, the INF path,
and the precise `pnputil` command, writing nothing. Add `--confirm` (alias
`-y`) in an Administrator shell to apply it; afterwards it re-queries the same
instance and reports whether the node recovered without a reboot.

`--mcu-alive` performs the smallest practical ST-Link target test: prove that
the USB probe exists, verify that its debug interface can accept commands, and
run a 100 kHz SWD discovery scan. It does not request a target reset or halt,
and does not read/write flash, erase, or unlock the target. The discovery scan
can reset the SWD debug-link state. On Windows, an accessible function driver
for the ST-Link debug interface is still required before any command can reach
the probe firmware.

`--mcu-alive-native` reaches the same goal with **no external tool**: it speaks
the ST-Link bulk command protocol directly through `nusb` (GET_VERSION,
GET_TARGET_VOLTAGE, ENTER_SWD, READ_IDCODES, READDEBUGREG) and reads the
target's DP IDCODE, CPUID, DBGMCU IDCODE, flash size, unique ID, and read-out
protection — all read-only, with no halt, reset, erase, or memory write. No
probe-rs, pyocd, or vendor driver is needed; on Windows the only requirement is
a WinUSB binding on the `MI_00` debug interface (`--install-driver`). If that
binding is missing it reports the exact transfer boundary
(`claim_interface` fails) rather than guessing the hardware is dead.

```text
Step 6 - DP IDCODE (DPIDR): 0x2BA01477
Step 7 - target identity (read-only):
  Core:              Cortex-M4 r0p1 (CPUID 0x410FC241)
  Device ID:         0x421 — STM32F446
  Flash size:        512 KB
  SRAM:              128 KB
  Read protection:   Disabled — flash readable (RDP Level 0)
  Source:            etc/chips/F446.chip
```

STM32 models are recognized from a **built-in family table** (no files needed).
For portability the whole table is compiled in; to add or override a part
without recompiling, drop a [stlink-style](https://github.com/stlink-org/stlink)
`*.chip` file under `etc/chips/` — a model with a matching `chip_id` takes
priority over the built-in table. The release archive bundles a few templates
next to the binary; see `etc/chips/README.md` for the format.

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

- **Some probes reset the chip.** Serial bootloader probes and downstream
  SWD/JTAG probes can reset or halt a target. ST-Link layer-1 identification is
  descriptor-only and does not issue any SWD/JTAG command.
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

- [`nusb`](https://crates.io/crates/nusb) — native cross-platform USB enumeration
- [`serialport`](https://crates.io/crates/serialport) — COM port mapping
- [`ureq`](https://crates.io/crates/ureq) — HTTP client for installer downloads
- [`zip`](https://crates.io/crates/zip) — archive extraction
- [`unicode-width`](https://crates.io/crates/unicode-width) — East-Asian-aware table padding
- [`anyhow`](https://crates.io/crates/anyhow) — flexible error handling

## Acknowledgements

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
