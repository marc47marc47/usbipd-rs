// The Windows USB driver-binding diagnostics (--driver-status, --install-driver,
// stlink_driver_check, …) are compiled on every OS but only *used* behind
// #[cfg(windows)]. On other targets they are legitimately dead code, so relax
// the lint there; Windows (the primary platform) still enforces -D dead_code.
#![cfg_attr(not(windows), allow(dead_code))]

use anyhow::{Context, Result};
use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
use nusb::{Endpoint, MaybeFuture};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use unicode_width::UnicodeWidthStr;

const HEADERS: [&str; 4] = ["BUSID", "VID:PID", "DEVICE", "SPEED"];

/// One avrdude connection attempt: an MCU paired with the bootloader
/// programmer that drives it, plus the sync baud rates to try (in order).
/// FT232R-class boards don't reveal which baud the bootloader uses, so
/// `bauds` may hold several candidates.
#[derive(Clone, Copy)]
struct AvrTarget {
    mcu: &'static str,
    programmer: &'static str,
    bauds: &'static [u32],
}

#[derive(Clone, Copy)]
enum ProbeKind {
    Espflash,
    /// Try each `AvrTarget` in order; the first that syncs wins. A board on a
    /// dedicated Arduino VID:PID passes exactly one target; a board behind a
    /// generic USB-UART bridge passes several, since the bridge can't reveal
    /// whether a Uno-class (STK500v1) or Mega-class (STK500v2) MCU is wired
    /// to it.
    Avrdude { targets: &'static [AvrTarget] },
    /// STM32 / GD32 system-memory UART bootloader probe (`stm32flash`).
    /// Requires the chip to be in bootloader mode (BOOT0=HIGH at reset);
    /// fails fast (~3s) when no chip responds, so safe to chain.
    Stm32Flash,
    /// STM32 USB DFU (DfuSe) bootloader probe via `dfu-util -l`. The ROM
    /// bootloader presents as native USB (VID:PID 0483:DF11), not a COM port,
    /// so this is read-only descriptor listing — alt settings + per-region
    /// memory layout (flash base/size, option bytes) — with no chip reset.
    Dfu,
    /// Read FTDI device descriptors via nusb (manufacturer / product /
    /// serial / bcdDevice chip variant / Windows driver binding). Read-only,
    /// no COM port, no chip reset — safe to chain before any serial probe.
    Ftdi,
    Picotool,
    Daplink,
    Pyocd,
    /// Identify the USB-visible ST-Link debug controller itself (layer 1) from
    /// its VID:PID and cached USB descriptors. This is deliberately
    /// non-invasive: it does not open the debug interface, issue SWD/JTAG
    /// commands, halt a target, or reset either MCU.
    Stlink,
    /// Explicit opt-in downstream SWD target probe (layer 2). Reads identity,
    /// flash-size, UID, and read-protection registers, then resets the target
    /// to resume firmware. Never erases, unlocks, dumps, or writes flash.
    StlinkTarget,
    /// Identify a CMSIS-DAP debug probe itself (layer 1) — e.g. the RP2040-based
    /// Raspberry Pi Debug Probe / picoprobe — from VID:PID + USB descriptors.
    CmsisDap,
    /// Downstream SWD target read (layer 2) through a CMSIS-DAP probe, spoken
    /// natively over the CMSIS-DAP v2 bulk protocol via nusb. Read-only: brings
    /// up SWD and reads ID registers only; no halt, reset, erase, or write.
    CmsisDapTarget,
}

impl ProbeKind {
    fn needs_serial_port(&self) -> bool {
        matches!(
            self,
            ProbeKind::Espflash | ProbeKind::Avrdude { .. } | ProbeKind::Stm32Flash
        )
    }

    fn tool_name(&self) -> &'static str {
        match self {
            ProbeKind::Espflash => "espflash",
            ProbeKind::Avrdude { .. } => "avrdude",
            ProbeKind::Stm32Flash => "stm32flash",
            ProbeKind::Dfu => "dfu-util",
            ProbeKind::Ftdi => "nusb",
            ProbeKind::Picotool => "picotool",
            ProbeKind::Daplink => "DETAILS.TXT",
            ProbeKind::Pyocd => "pyocd",
            ProbeKind::Stlink => "nusb (layer 1 only)",
            ProbeKind::StlinkTarget => "native SWD (layer 2)",
            ProbeKind::CmsisDap => "nusb (layer 1, CMSIS-DAP)",
            ProbeKind::CmsisDapTarget => "native CMSIS-DAP SWD (layer 2)",
        }
    }
}

struct KnownBoard {
    vid: u16,
    pid: u16,
    name: &'static str,
    probes: &'static [ProbeKind],
}

// ── avrdude connection profiles, keyed by Arduino bootloader family ─────
const AVR_UNO:  AvrTarget = AvrTarget { mcu: "atmega328p", programmer: "arduino", bauds: &[115200] };
const AVR_MEGA: AvrTarget = AvrTarget { mcu: "atmega2560", programmer: "wiring",  bauds: &[115200] };
const AVR_32U4: AvrTarget = AvrTarget { mcu: "atmega32u4", programmer: "avr109",  bauds: &[57600] };

// Reusable probe pipelines
const ESP: &[ProbeKind] = &[ProbeKind::Espflash];
const PICO: &[ProbeKind] = &[ProbeKind::Picotool];
const DFU: &[ProbeKind] = &[ProbeKind::Dfu];
const DAP_PIPELINE: &[ProbeKind] = &[ProbeKind::Daplink, ProbeKind::Pyocd];
const STLINK: &[ProbeKind] = &[ProbeKind::Stlink, ProbeKind::StlinkTarget];
const CMSISDAP: &[ProbeKind] = &[ProbeKind::CmsisDap, ProbeKind::CmsisDapTarget];

// A generic USB-UART bridge (CH340, CP210x, FT2232/FT232H/FT231X) carries no
// information about which MCU is wired to its TX/RX lines — the same chip
// ships on ESP, STM32/GD32, and Arduino (Uno/Nano/Mega) clones alike. Probe
// in fail-fast order:
//   1. espflash — fast on real ESP via DTR/RTS auto-bootloader (~2s).
//   2. stm32flash — needs user to manually pull BOOT0=HIGH + RESET, but fails
//      in ~3s when no STM32/GD32 responds. Catches GD32F103 / STM32 dev
//      boards that don't auto-reset (most generic boards don't wire DTR/RTS
//      to BOOT0/RESET).
//   3. avrdude — slowest (multiple programmer+baud combos, ~10s when failing),
//      but the only way to detect Arduino-class boards. Tries both bootloader
//      dialects: STK500v1 ("arduino", Uno/Nano-class) and STK500v2 ("wiring",
//      Mega 2560).
// probe_boards() stops at the first serial probe that syncs, so a real ESP
// never reaches stm32flash; a real GD32 in bootloader never reaches avrdude.
const BRIDGE: &[ProbeKind] = &[
    ProbeKind::Espflash,
    ProbeKind::Stm32Flash,
    ProbeKind::Avrdude {
        targets: &[
            // Uno/Nano-class: bootloader baud varies by clone age → try the
            // modern 115200 first, then the legacy 57600.
            AvrTarget { mcu: "atmega328p", programmer: "arduino", bauds: &[115200, 57600] },
            AVR_MEGA,
        ],
    },
];

const KNOWN_BOARDS: &[KnownBoard] = &[
    // ── Arduino official boards (probe via avrdude) ──────────────────────
    KnownBoard { vid: 0x2341, pid: 0x0001, name: "Arduino Uno R1",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_UNO] }] },
    KnownBoard { vid: 0x2341, pid: 0x0043, name: "Arduino Uno R3",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_UNO] }] },
    KnownBoard { vid: 0x2341, pid: 0x0010, name: "Arduino Mega 2560",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_MEGA] }] },
    KnownBoard { vid: 0x2341, pid: 0x0042, name: "Arduino Mega 2560 R3",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_MEGA] }] },
    KnownBoard { vid: 0x2341, pid: 0x0044, name: "Arduino Mega ADK",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_MEGA] }] },
    KnownBoard { vid: 0x2341, pid: 0x8036, name: "Arduino Leonardo",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_32U4] }] },
    KnownBoard { vid: 0x2341, pid: 0x8037, name: "Arduino Micro",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_32U4] }] },

    // ── Classic Arduino behind a bare FTDI FT232R USB-UART bridge ────────
    // FT232R (0403:6001) is a bare USB-serial chip — it can't tell us what MCU
    // sits on its TX/RX lines. Classic Arduinos that use it (Nano, Duemilanove)
    // speak the STK500v1 bootloader protocol, so probe via avrdude. We request
    // atmega328p (the common case); avrdude runs with -F, so a 328PB / 168 /
    // LGT8F328P clone still connects and reports its true signature instead of
    // erroring. Bootloader baud varies by board age → try 57600 then 115200.
    //
    // Always run the read-only Ftdi descriptor probe first so the user gets
    // useful info (chip variant, serial, driver binding) even when no Arduino
    // MCU is wired downstream and avrdude fails to sync.
    KnownBoard { vid: 0x0403, pid: 0x6001, name: "FT232R Arduino (ATmega328-class)",
        probes: &[
            ProbeKind::Ftdi,
            ProbeKind::Avrdude {
                targets: &[AvrTarget { mcu: "atmega328p", programmer: "arduino", bauds: &[57600, 115200] }],
            },
        ] },

    // ── Generic USB-UART bridges: an ESP *or* an Arduino may sit behind ──
    // them, so probe espflash first and fall back to avrdude (see BRIDGE).
    KnownBoard { vid: 0x10c4, pid: 0xea60, name: "CP2102/CP2102N", probes: BRIDGE },
    KnownBoard { vid: 0x10c4, pid: 0xea70, name: "CP2105",         probes: BRIDGE },
    KnownBoard { vid: 0x10c4, pid: 0xea71, name: "CP2108",         probes: BRIDGE },
    KnownBoard { vid: 0x1a86, pid: 0x7523, name: "CH340",          probes: BRIDGE },
    KnownBoard { vid: 0x1a86, pid: 0x55d4, name: "CH9102",         probes: BRIDGE },
    KnownBoard { vid: 0x0403, pid: 0x6010, name: "FT2232",         probes: BRIDGE },
    KnownBoard { vid: 0x0403, pid: 0x6014, name: "FT232H",         probes: BRIDGE },
    KnownBoard { vid: 0x0403, pid: 0x6015, name: "FT231X",         probes: BRIDGE },

    // ── ESP32 native USB (the ESP silicon itself presents USB) ───────────
    KnownBoard { vid: 0x303a, pid: 0x1001, name: "ESP32 USB-Serial-JTAG", probes: ESP },
    KnownBoard { vid: 0x303a, pid: 0x4001, name: "ESP32 USB-OTG",  probes: ESP },

    // ── STM32 system ROM USB DFU bootloader (DfuSe) (probe via dfu-util) ──
    // VID:PID 0483:DF11 is the on-chip DFU bootloader every STM32 with USB
    // exposes (entered via BOOT0=HIGH at reset, or a firmware "jump to
    // bootloader"). It's native USB, not a UART/COM port, so stm32flash can't
    // reach it — dfu-util reads the alt-setting memory map (flash base/size,
    // option bytes) read-only. On Windows the DFU interface needs a WinUSB
    // driver (run `usbipd-rs --install zadig`).
    KnownBoard { vid: 0x0483, pid: 0xdf11, name: "STM32 DFU Bootloader (DfuSe)", probes: DFU },

    // ── Raspberry Pi Pico (RP2040 / RP2350) (probe via picotool) ─────────
    KnownBoard { vid: 0x2e8a, pid: 0x0003, name: "RP2040 BOOTSEL (Pi Pico)",   probes: PICO },
    KnownBoard { vid: 0x2e8a, pid: 0x000f, name: "RP2350 BOOTSEL (Pi Pico 2)", probes: PICO },

    // ── DAPLink-based boards (BBC micro:bit, NXP FRDM, etc.) ─────────────
    // Read DETAILS.TXT from MSD first, then ask pyocd what target chip is on
    // the other end of the SWD lines (board database lookup; no chip reset).
    KnownBoard { vid: 0x0d28, pid: 0x0204, name: "DAPLink (mbed CMSIS-DAP)", probes: DAP_PIPELINE },

    // ── RP2040-based CMSIS-DAP debug probes (layer 1 = RP2040; layer 2 = SWD) ──
    // Native CMSIS-DAP v2 read of the downstream target — no probe-rs/pyocd.
    KnownBoard { vid: 0x2e8a, pid: 0x000c, name: "Raspberry Pi Debug Probe (RP2040 CMSIS-DAP)", probes: CMSISDAP },
    KnownBoard { vid: 0x2e8a, pid: 0x0004, name: "Picoprobe (RP2040 CMSIS-DAP)",               probes: CMSISDAP },

    // ── ST-Link debug controllers (layer 1 only; downstream target disabled) ──
    KnownBoard { vid: 0x0483, pid: 0x3748, name: "ST-Link/V2 debug controller",   probes: STLINK },
    KnownBoard { vid: 0x0483, pid: 0x374b, name: "ST-Link/V2-1 debug controller", probes: STLINK },
    KnownBoard { vid: 0x0483, pid: 0x374e, name: "ST-Link/V3 debug controller",   probes: STLINK },
    KnownBoard { vid: 0x0483, pid: 0x374f, name: "ST-Link/V3 debug controller",   probes: STLINK },
];

fn lookup_board(vid: u16, pid: u16) -> Option<&'static KnownBoard> {
    KNOWN_BOARDS.iter().find(|b| b.vid == vid && b.pid == pid)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        print_help();
        return Ok(());
    }
    if args.iter().any(|a| matches!(a.as_str(), "-V" | "--version")) {
        println!("usbipd-rs {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--list-tools") {
        return cmd_list_tools();
    }
    if args.iter().any(|a| a == "--driver-status") {
        return cmd_driver_status();
    }
    if args.iter().any(|a| a == "--install-driver") {
        let confirm = args.iter().any(|a| matches!(a.as_str(), "--confirm" | "--yes" | "-y"));
        return cmd_install_driver(confirm);
    }
    if args.iter().any(|a| a == "--mcu-alive-native") {
        return cmd_mcu_alive_native();
    }
    if args.iter().any(|a| a == "--mcu-alive") {
        return cmd_mcu_alive();
    }
    if let Some(idx) = args.iter().position(|a| a == "--install") {
        let tool = args.get(idx + 1).map(String::as_str).unwrap_or("");
        if tool.is_empty() {
            anyhow::bail!("--install requires a tool name. Try `--list-tools`.");
        }
        return cmd_install(tool);
    }

    cmd_list_usb()
}

fn print_help() {
    let help = r#"usbipd-rs — USB device inspector with chip-level board probing

USAGE:
    usbipd-rs                       List connected USB devices and detect probable boards.
    usbipd-rs --probe               List, then probe each detected board (chip-level info).
                                    Bridge boards (CH340/CP210x/FT232) probe in fail-fast
                                    order: espflash → stm32flash → avrdude. For an ST-Link
                                    it also auto-detects a downstream SWD/JTAG target and
                                    shows a second layer when one is present (read-only).
    usbipd-rs --mcu-alive           Minimal read-only ST-Link/SWD target-alive test.
    usbipd-rs --mcu-alive-native    Same, but native nusb (no probe-rs/pyocd needed).
    usbipd-rs --driver-status       Diagnose Windows USB interface driver bindings.
    usbipd-rs --install-driver      Dry-run (or --confirm) bind the ST-Link MI_00 driver.
    usbipd-rs --list-tools          Show install status of all probe-tool dependencies.
    usbipd-rs --install <ID>        Download/install one tool by its ID.
    usbipd-rs --help                Show this help.
    usbipd-rs --version             Print the program version and exit.

OPTIONS:
    -p, --probe                Probe each detected board with the matching chip-level
                               tool (espflash, stm32flash, avrdude, picotool, DAPLink,
                               pyocd). Aliases: --probe-esp, --probe-arduino. For an
                               ST-Link it automatically adds a read-only second layer:
                               it enters SWD natively (nusb), and if a target MCU
                               answers it prints the target identity; if none answers
                               it reports a one-layer result. No chip reset/halt.
        --mcu-alive            Minimal ST-Link test: verify USB probe, open debug
                               interface, then perform a 100 kHz SWD discovery scan.
                               Does not request target reset, halt, flash read/write,
                               erase, or unlock.
        --mcu-alive-native     Same goal with NO external tool: speaks the ST-Link
                               bulk protocol directly via nusb (enter SWD, read
                               DPIDR / CPUID / DBGMCU IDCODE + flash/UID/RDP, all
                               read-only). On Windows needs only WinUSB on MI_00
                               (--install-driver) — no probe-rs, pyocd, or vendor
                               driver. If MI_00 has no driver it reports the exact
                               transfer boundary instead of guessing. STM32 models
                               are identified from a built-in family table, which
                               optional etc/chips/*.chip files extend or override
                               (a matching .chip wins).
        --driver-status        Windows: show every present USB interface's service,
                               INF/provider, problem code, classified issue, safe next
                               action, and local/driver-store INF availability.
        --install-driver       Windows: install the bundled WinUSB INF for the ST-Link
                               MI_00 debug interface ONLY. Dry-run by default (shows the
                               exact interface and pnputil command); pass --confirm
                               (alias -y) in an Administrator shell to apply, then it
                               re-checks the node recovered without a reboot.
        --list-tools           List installable tools, their OS provider (cargo / pip /
                               brew / apt / download / manual) and install status.
        --install <ID>         Install one tool by ID. See --list-tools for IDs.
    -h, --help                 Show this help.
    -V, --version              Print version (from Cargo.toml) and exit.

WHAT GETS DETECTED (VID:PID → board → probe):
    10C4:EA60 / EA70 / EA71      Silabs CP210x bridge       → ESP / STM32 / AVR (espflash → stm32flash → avrdude)
    1A86:7523 / 55D4              WCH CH340 / CH9102         → ESP / STM32 / AVR (espflash → stm32flash → avrdude)
    0403:6010 / 6014 / 6015       FTDI FT2232 / FT232H / X   → ESP / STM32 / AVR (espflash → stm32flash → avrdude)
    0403:6001                     FTDI FT232R (Arduino)      → FTDI+AVR (nusb descriptors → avrdude)
    303A:1001 / 4001              ESP32 native USB-Serial    → ESP32 (espflash)
    0483:DF11                     STM32 USB DFU bootloader   → STM32 (dfu-util, DfuSe memory map)
    2341:0001 / 0043              Arduino Uno R1 / R3        → AVR  (avrdude)
    2341:0010 / 0042 / 0044       Arduino Mega 2560 / ADK    → AVR  (avrdude)
    2341:8036 / 8037              Arduino Leonardo / Micro   → AVR  (avrdude)
    2E8A:0003                     RP2040 BOOTSEL (Pi Pico)   → RP2  (picotool)
    2E8A:000F                     RP2350 BOOTSEL (Pi Pico 2) → RP2  (picotool)
    2E8A:000C / 0004              RPi Debug Probe / picoprobe → DAP (RP2040 layer 1 + native CMSIS-DAP layer 2)
    0D28:0204                     mbed CMSIS-DAP / DAPLink   → DAP+SWD (DETAILS.TXT + pyocd)
    0483:3748 / 374B / 374E / 374F ST-Link V2 / V2-1 / V3    → STL (STM32 layer 1 + native ST-Link SWD layer 2)

INSTALLABLE TOOLS (see --list-tools for live status):
    espflash    cargo install espflash         ESP chip identification & flashing
    pyocd       pip install pyocd              CMSIS-DAP / DAPLink target chip ID
    picotool    GitHub release zip             Pi Pico (RP2040 / RP2350) inspection
    avrdude     GitHub release zip / brew /    Arduino (ATmega328P / 328PB /
                apt-get                        2560 / 32U4) chip ID & flashing
    stm32flash  bundled zip (windows-driver/)  STM32 / GD32 UART-bootloader chip ID
                                               (e.g. GD32F103RET6 behind a CH340)
    dfu-util    brew / apt / manual (Win)      STM32 USB DFU (0483:DF11) memory-map ID
    ravedude    cargo install ravedude         avr-hal `cargo run` runner (Rust AVR)
    arduino-cli download (Win/Linux) /         Arduino core manager — bundles avrdude
                brew (macOS)                   via `core install arduino:avr`, etc.
    zadig       libwdi GitHub release          Win-only: replace USB driver → WinUSB
    cp210x      silabs.com universal driver    Win-only: CP2102/CP2104 VCP driver
    ch340       wch-ic.com CH341SER.EXE        Win-only: CH340/CH341 USB-Serial driver
    ftdi        ftdichip.com CDM (manual)      Win-only: FTDI FT232R VCP driver

EXAMPLES:
    # Listing only — no chip reset, safe to run any time
    usbipd-rs

    # Full probe — chip type, revision, flash size, MAC, etc. For an ST-Link it
    # auto-detects and shows the downstream SWD target (read-only) when present.
    usbipd-rs --probe

    # Minimal test that the target MCU responds through ST-Link/SWD
    usbipd-rs --mcu-alive

    # Prove USB hardware enumeration and diagnose missing/broken Windows drivers
    usbipd-rs --driver-status

    # Preview exactly what an ST-Link MI_00 driver install would change (no writes)
    usbipd-rs --install-driver

    # Apply it (Administrator shell), then auto-verify recovery without reboot
    usbipd-rs --install-driver --confirm

    # Install picotool from upstream Raspberry Pi release
    usbipd-rs --install picotool

    # Install pyocd via pip (needed for DAPLink target identification)
    usbipd-rs --install pyocd

    # Download Zadig + see manual driver-replacement steps for Pi Pico BOOTSEL
    usbipd-rs --install zadig

NOTES:
    * --probe's ST-Link layer-2 step is read-only (native SWD over nusb): it
      enters SWD and reads ID registers only — no halt, reset, erase, or write.
      On Windows it needs WinUSB on MI_00 (see --install-driver); without it the
      layer-2 line just reports the missing binding.
    * stm32flash probing requires the chip in system bootloader mode
      (BOOT0=HIGH at reset). CH340/CP210x boards typically don't wire DTR/RTS
      to BOOT0, so the probe can't trigger bootloader entry automatically — if
      you want stm32flash to identify a GD32/STM32, pull BOOT0 HIGH and press
      RESET before running --probe.
    * --install caches downloads under <project>/windows-driver/ (Windows) or
      <project>/tools/ (macOS/Linux). Existing files are reused — delete to
      force re-download.
    * Pi Pico (BOOTSEL) on Windows additionally needs a WinUSB driver swap via
      Zadig. Run `usbipd-rs --install zadig` and follow the printed steps.
    * macOS / Linux ship CP210x and CH340 kernel modules; only Windows needs
      those installers.

DATA SOURCES:
    USB enumeration   nusb (cross-platform native USB enumeration)
    COM-port mapping  serialport crate
    Bundled binaries  windows-driver/picotool/, windows-driver/avrdude/, ...
"#;
    print!("{help}");
}

fn cmd_list_usb() -> Result<()> {
    let probe = std::env::args()
        .any(|a| matches!(a.as_str(), "--probe" | "--probe-esp" | "--probe-arduino" | "-p"));

    let entries = list_entries()?;
    let nusb_devs = nusb_by_vidpid();

    let rows: Vec<[String; 4]> = entries
        .iter()
        .map(|e| {
            let speed = nusb_devs
                .get(&e.vidpid_pair())
                .and_then(|d| d.speed())
                .map(|s| format!("{:?}", s))
                .unwrap_or_else(|| "-".into());
            [
                e.busid.clone(),
                e.vidpid.clone(),
                e.device.clone(),
                speed,
            ]
        })
        .collect();

    print_table(&rows);

    if rows.is_empty() {
        println!("(no connected USB devices found)");
        return Ok(());
    }

    let candidates: Vec<(&Entry, &'static KnownBoard)> = entries
        .iter()
        .filter_map(|e| {
            let (v, p) = e.vidpid_pair();
            lookup_board(v, p).map(|b| (e, b))
        })
        .collect();

    if candidates.is_empty() {
        return Ok(());
    }

    println!();
    if probe {
        probe_boards(&candidates);
    } else {
        println!("Probable boards detected:");
        for (e, b) in &candidates {
            let labels: Vec<&str> = b
                .probes
                .iter()
                .map(|p| match p {
                    ProbeKind::Espflash => "ESP",
                    ProbeKind::Avrdude { .. } => "AVR",
                    ProbeKind::Stm32Flash => "STM32",
                    ProbeKind::Dfu => "DFU",
                    ProbeKind::Ftdi => "FTDI",
                    ProbeKind::Picotool => "RP2",
                    ProbeKind::Daplink => "DAP",
                    ProbeKind::Pyocd => "SWD",
                    ProbeKind::Stlink => "STL",
                    ProbeKind::StlinkTarget => "SWD2",
                    ProbeKind::CmsisDap => "DAP",
                    ProbeKind::CmsisDapTarget => "SWD2",
                })
                .collect();
            println!(
                "  - [{}] {} ({}) at BUSID {}",
                labels.join("+"),
                e.vidpid,
                b.name,
                e.busid
            );
        }
        println!();
        println!("Run with --probe to query each board (espflash / stm32flash / avrdude / picotool / DAPLink / pyocd).");
        println!("Note: probing may reset the chip — do NOT use while a 3D-printer or");
        println!("      other live firmware is communicating.");
        let has_stlink = candidates.iter().any(|(e, _)| e.vidpid.starts_with("0483:374"));
        let has_dap = candidates
            .iter()
            .any(|(_, b)| b.probes.iter().any(|p| matches!(p, ProbeKind::CmsisDapTarget)));
        if has_stlink {
            println!();
            println!("ST-Link present — safe, read-only options (no chip reset):");
            println!("  --mcu-alive-native   read target ID over SWD via nusb (no probe-rs/pyocd).");
            println!("  --driver-status      check the MI_00 debug-interface driver binding (Windows).");
            println!("  --install-driver     bind WinUSB to MI_00 if --mcu-alive-native can't claim it.");
        }
        if has_dap {
            println!();
            println!("CMSIS-DAP probe present — `--probe` reads the downstream SWD target");
            println!("read-only via native CMSIS-DAP (no probe-rs/pyocd). On Windows the");
            println!("'CMSIS-DAP v2' interface must be on WinUSB (use Zadig: --install zadig).");
        }
    }
    Ok(())
}

fn probe_boards(candidates: &[(&Entry, &'static KnownBoard)]) {
    println!("=== Board Probe ===");
    let serial_ports = serialport::available_ports().unwrap_or_default();

    for (entry, board) in candidates {
        let (vid, pid) = entry.vidpid_pair();
        // A single COM port carries exactly one chip: once any serial probe
        // (espflash / stm32flash / avrdude) has identified it, the remaining
        // serial probes in this board's pipeline can only fail — skip them
        // silently.
        let mut serial_chip_found = false;

        for &probe in board.probes {
            let needs_port = probe.needs_serial_port();

            if needs_port && serial_chip_found {
                continue;
            }

            let port_name = if needs_port {
                serial_ports.iter().find_map(|p| match &p.port_type {
                    serialport::SerialPortType::UsbPort(info)
                        if info.vid == vid && info.pid == pid =>
                    {
                        Some(p.port_name.clone())
                    }
                    _ => None,
                })
            } else {
                None
            };

            if needs_port && port_name.is_none() {
                println!(
                    "\n[{}  {}  {}  via {}]  no COM port found",
                    entry.busid, board.name, entry.vidpid, probe.tool_name()
                );
                continue;
            }

            // Serial probes show the COM port; USB-direct probes (FTDI / DFU /
            // pyocd / picotool / ST-Link …) identify the device by VID:PID.
            // (This used to print "(via libusb)", which confused users into
            // thinking the ST-Link wasn't on WinUSB — but libusb on Windows *is*
            // the WinUSB access path. VID:PID is unambiguous.)
            let header_port: &str = match port_name.as_deref() {
                Some(p) => p,
                None => entry.vidpid.as_str(),
            };
            println!(
                "\n[{}  {}  {}  via {}]",
                entry.busid, board.name, header_port, probe.tool_name()
            );

            let result = match probe {
                ProbeKind::Espflash => run_espflash_board_info(port_name.as_deref().unwrap()),
                ProbeKind::Avrdude { targets } => {
                    run_avrdude_query(port_name.as_deref().unwrap(), targets)
                }
                ProbeKind::Stm32Flash => run_stm32flash_query(port_name.as_deref().unwrap()),
                ProbeKind::Dfu => run_dfu_info(vid, pid),
                ProbeKind::Ftdi => run_ftdi_info(vid, pid),
                ProbeKind::Picotool => run_picotool_info(vid, pid),
                ProbeKind::Daplink => run_daplink_query(),
                ProbeKind::Pyocd => run_pyocd_query(vid, pid),
                ProbeKind::Stlink => run_stlink_controller_query(vid, pid),
                ProbeKind::StlinkTarget => run_stlink_target_query(vid, pid),
                ProbeKind::CmsisDap => run_cmsisdap_controller_query(vid, pid),
                ProbeKind::CmsisDapTarget => run_cmsisdap_target_query(vid, pid),
            };

            match result {
                Ok(info) if !info.is_empty() => {
                    if needs_port {
                        serial_chip_found = true;
                    }
                    match probe {
                        ProbeKind::Espflash => print_esp_info(&info),
                        ProbeKind::Avrdude { .. } => print_avr_info(&info, board.name),
                        ProbeKind::Stm32Flash => print_stm32_info(&info, board.name),
                        ProbeKind::Dfu => print_dfu_info(&info, board.name),
                        ProbeKind::Ftdi => print_ftdi_info(&info, board.name),
                        ProbeKind::Picotool => print_pico_info(&info, board.name),
                        ProbeKind::Daplink => print_daplink_info(&info, board.name),
                        ProbeKind::Pyocd => print_pyocd_info(&info),
                        ProbeKind::Stlink => print_stlink_controller_info(&info),
                        ProbeKind::StlinkTarget => print_stlink_target_info(&info, board.name),
                        ProbeKind::CmsisDap => print_stlink_controller_info(&info),
                        ProbeKind::CmsisDapTarget => print_stlink_target_info(&info, board.name),
                    }
                }
                Ok(_) => println!("  (no info parsed from output)"),
                Err(e) => {
                    let msg = e.to_string();
                    let mut lines = msg.lines();
                    if let Some(first) = lines.next() {
                        println!("  Error: {first}");
                        for line in lines {
                            println!("         {line}");
                        }
                    }
                }
            }
        }
    }

    print_probe_install_suggestions(candidates);
}

/// The driver a detected board needs so the host can talk to it (Windows only;
/// other OSes ship kernel modules / use udev). Returns `(what, install command)`.
fn driver_suggestion(vid: u16, pid: u16) -> Option<(&'static str, String)> {
    match vid {
        0x0483 if (0x3748..=0x3757).contains(&pid) => match stlink_driver_check(pid) {
            // Already bound to WinUSB — nothing to install.
            Some(StlinkDriver::WinUsb) => None,
            _ => Some((
                "WinUSB on the ST-Link MI_00 debug interface",
                "usbipd-rs --install-driver --confirm   (run in an Administrator shell)".into(),
            )),
        },
        0x0483 if pid == 0xdf11 => Some((
            "WinUSB for the STM32 DFU bootloader (Zadig)",
            "usbipd-rs --install zadig".into(),
        )),
        0x1a86 => Some(("WCH CH340 / CH9102 USB-serial driver", "usbipd-rs --install ch340".into())),
        0x10c4 => Some(("Silicon Labs CP210x VCP driver", "usbipd-rs --install cp210x".into())),
        0x0403 => Some(("FTDI VCP driver", "usbipd-rs --install ftdi".into())),
        0x2e8a => Some(("WinUSB on the RP2 BOOTSEL interface (Zadig)", "usbipd-rs --install zadig".into())),
        _ => None,
    }
}

/// The Rust-based flashing tool that fits a detected board, chosen by its first
/// probe kind. Returns `(what, cargo install command)`.
fn flasher_suggestion(board: &KnownBoard) -> Option<(&'static str, String)> {
    for p in board.probes {
        return Some(match p {
            ProbeKind::Espflash => ("espflash — ESP flashing & board-info (Rust)", "cargo install espflash".into()),
            ProbeKind::Avrdude { .. } => ("ravedude — AVR `cargo run` flasher (Rust)", "cargo install ravedude".into()),
            ProbeKind::Stlink
            | ProbeKind::StlinkTarget
            | ProbeKind::CmsisDap
            | ProbeKind::CmsisDapTarget
            | ProbeKind::Pyocd
            | ProbeKind::Daplink
            | ProbeKind::Stm32Flash
            | ProbeKind::Dfu
            | ProbeKind::Picotool => ("probe-rs — SWD/JTAG flash & debug (Rust)", "cargo install probe-rs-tools".into()),
            // FTDI is a descriptor-only probe; keep looking for a flashable kind.
            ProbeKind::Ftdi => continue,
        });
    }
    None
}

/// Print, at the end of `--probe`, the two things a user typically still needs:
/// (1) the driver so the host can reach the board, and (2) a Rust flashing tool.
fn print_probe_install_suggestions(candidates: &[(&Entry, &'static KnownBoard)]) {
    let mut flashers: Vec<(&str, String)> = Vec::new();
    for (_, b) in candidates {
        if let Some(s) = flasher_suggestion(b) {
            if !flashers.iter().any(|(_, c)| *c == s.1) {
                flashers.push(s);
            }
        }
    }

    println!("\n=== Suggested installs ===");

    println!("1. Driver — let the host talk to the board:");
    if current_os() == Os::Windows {
        let mut drivers: Vec<(&str, String)> = Vec::new();
        for (e, _) in candidates {
            let (vid, pid) = e.vidpid_pair();
            if let Some(s) = driver_suggestion(vid, pid) {
                if !drivers.iter().any(|(_, c)| *c == s.1) {
                    drivers.push(s);
                }
            }
        }
        if drivers.is_empty() {
            println!("   - detected board(s) already have a working driver.");
        } else {
            for (what, cmd) in drivers {
                println!("   - {what}");
                println!("       {cmd}");
            }
        }
    } else {
        println!("   - no Windows-style driver needed on this OS (kernel modules are built in).");
        println!("     For SWD probes, ensure udev rules / group permissions allow USB access.");
    }

    println!("2. Rust flashing tool:");
    if flashers.is_empty() {
        println!("   - no Rust flasher mapping for the detected board(s).");
    } else {
        for (what, cmd) in flashers {
            println!("   - {what}");
            println!("       {cmd}");
        }
    }
}

fn run_espflash_board_info(port: &str) -> Result<HashMap<String, String>> {
    let output = Command::new("espflash")
        .args(["board-info", "--port", port])
        .output()
        .context("espflash not found on PATH (install with: cargo install espflash)")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        let last = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .unwrap_or("unknown");
        anyhow::bail!("{}", last);
    }
    Ok(parse_espflash_output(&format!("{stdout}\n{stderr}")))
}

fn parse_espflash_output(s: &str) -> HashMap<String, String> {
    let mut info = HashMap::new();
    let interesting = [
        "Chip type",
        "Chip revision",
        "Crystal frequency",
        "Flash size",
        "Features",
        "MAC address",
        "MAC",
    ];
    for raw in s.lines() {
        let line = strip_log_prefix(raw).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            if interesting.contains(&key) {
                info.insert(key.to_string(), v.trim().to_string());
            }
        }
    }
    info
}

fn strip_log_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(close) = rest.find(']') {
            return rest[close + 1..].trim_start();
        }
    }
    line
}

fn print_esp_info(info: &HashMap<String, String>) {
    let order = [
        "Chip type",
        "Chip revision",
        "Crystal frequency",
        "Flash size",
        "Features",
        "MAC address",
        "MAC",
    ];
    for k in order {
        if let Some(v) = info.get(k) {
            println!("  {:<20} {}", format!("{k}:"), v);
        }
    }
}

fn run_avrdude_query(port: &str, targets: &[AvrTarget]) -> Result<HashMap<String, String>> {
    // Serial bootloader programmers (arduino/wiring/avr109/stk500v1/stk500v2)
    // can read flash/eeprom/signature but NOT fuses — fuse reads return 0
    // silently. Skip fuse reads here; only an ISP programmer on the ICSP
    // header can read them.
    //
    // `targets` holds one or more (MCU, programmer, bauds) profiles to try in
    // order — the first that syncs wins. A board on a dedicated Arduino VID
    // passes exactly one; a board behind a generic USB-UART bridge passes
    // several, because the bridge can't reveal whether a Uno-class (STK500v1
    // "arduino") or a Mega-class (STK500v2 "wiring") MCU is on its lines.
    //
    // `-F` overrides avrdude's signature check: when the actual MCU differs
    // from `-p` (a 328PB / 168 / LGT8F328P clone), the run still succeeds and
    // reports the true `Device signature` instead of bailing. `-F` only
    // relaxes the post-connect check — a board that never syncs (wrong baud,
    // wrong bootloader dialect, or no Arduino at all) still fails cleanly, so
    // chaining mismatched profiles produces no false positives. Probing is
    // read-only here (no -U), so overriding the check has no side effects.
    let mut last_err = String::from("avrdude produced no output");

    for target in targets {
        for &baud in target.bauds {
            let baud_str = baud.to_string();
            let output = Command::new("avrdude")
                .args([
                    "-c", target.programmer,
                    "-p", target.mcu,
                    "-P", port,
                    "-b", &baud_str,
                    "-F",
                    "-v",
                ])
                .output()
                .context("avrdude not found on PATH (install via Arduino IDE, PlatformIO, or scoop install avrdude)")?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                return Ok(parse_avrdude_output(&stdout, &stderr, target.mcu));
            }
            last_err = summarize_avrdude_error(&stderr, target, baud);
        }
    }
    anyhow::bail!("{last_err}");
}

fn summarize_avrdude_error(stderr: &str, target: &AvrTarget, baud: u32) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let last = lines.last().copied().unwrap_or("unknown");
    // Name the attempt (programmer + baud) so a multi-target probe makes clear
    // which bootloader dialect failed. A signature mismatch (wrong -p) is the
    // most actionable failure, but the "Device signature = ..." line isn't the
    // last one avrdude prints — pull it out so the caller can tell a wrong MCU
    // guess from a baud/wiring problem.
    let attempt = format!("{} @ {baud} baud", target.programmer);
    match lines.iter().rev().find(|l| l.contains("Device signature")) {
        Some(sig) if *sig != last => format!("{attempt} — {sig}; {last}"),
        _ => format!("{attempt} — {last}"),
    }
}

fn parse_avrdude_output(_stdout: &str, stderr: &str, requested_mcu: &str) -> HashMap<String, String> {
    let mut info = HashMap::new();
    info.insert("Requested MCU".to_string(), requested_mcu.to_string());

    let prefixes: &[(&str, &str)] = &[
        ("AVR part",          "Detected MCU"),
        ("Programmer type",   "Programmer"),
        ("Description",       "Bootloader"),
        ("HW Version",        "HW Version"),
        ("FW Version",        "FW Version"),
        ("Programming modes", "Modes"),
    ];

    for raw in stderr.lines() {
        let line = raw.trim_end();

        for (prefix, label) in prefixes {
            if let Some(rest) = line.strip_prefix(prefix) {
                if let Some(idx) = rest.find(':') {
                    let value = rest[idx + 1..].trim();
                    if !value.is_empty() {
                        info.insert((*label).to_string(), value.to_string());
                    }
                }
            }
        }

        // "Device signature = 1E 95 0F (ATmega328P, ATA6614Q, LGT8F328P)"
        if let Some(rest) = line.strip_prefix("Device signature = ") {
            let (sig, alt) = match rest.split_once(" (") {
                Some((s, a)) => (s.trim(), a.trim_end_matches(')').trim()),
                None => (rest.trim(), ""),
            };
            info.insert("Signature".to_string(), sig.to_string());
            if !alt.is_empty() {
                info.insert("Sig matches".to_string(), alt.to_string());
            }
        }
    }
    info
}

fn print_avr_info(info: &HashMap<String, String>, board_name: &str) {
    println!("  {:<20} {}", "Board:", board_name);
    let order = [
        "Detected MCU",
        "Signature",
        "Sig matches",
        "Programmer",
        "Bootloader",
        "HW Version",
        "FW Version",
        "Modes",
    ];
    for k in order {
        if let Some(v) = info.get(k) {
            println!("  {:<20} {}", format!("{k}:"), v);
        }
    }
    if let Some(mcu) = info.get("Detected MCU") {
        let summary = avr_chip_summary(mcu.as_str());
        if !summary.is_empty() {
            println!("  {:<20} {}", "Summary:", summary);
        }
    }
    println!("  {:<20} (fuses require an ISP programmer on the ICSP header)", "Fuses:");
}

fn find_stm32flash() -> PathBuf {
    if let Ok(p) = std::env::var("STM32FLASH_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    // Bundled v0.7 zip carries per-OS binaries with distinct names — pick the
    // matching one. (The zip extracts as windows-driver/stm32flash/stm32flash-
    // 0.7-binaries/<binary>.)
    let binary_name = match std::env::consts::OS {
        "windows" => "stm32flash.exe",
        "linux"   => "stm32flash_linux",
        "macos"   => "stm32flash_macos",
        _         => "stm32flash",
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Candidate paths relative to the binary:
            //   target/release/usbipd-rs.exe → ../../windows-driver/stm32flash/...
            //   target/debug/usbipd-rs.exe   → same shape
            //   plus a flat ./stm32flash{.exe,_linux,_macos} for portable layouts
            let inner = Path::new("windows-driver")
                .join("stm32flash")
                .join("stm32flash-0.7-binaries")
                .join(binary_name);
            let candidates = [
                dir.join(binary_name),
                dir.join("..").join("..").join(&inner),
                dir.join("..").join("..").join("..").join(&inner),
            ];
            for c in candidates {
                if c.exists() {
                    return c;
                }
            }
        }
    }
    // Fallback: bare name, resolved via PATH (covers users who ran
    // `scoop install stm32flash` / `brew install stm32flash` / `apt-get install
    // stm32flash` instead of the bundled extract).
    PathBuf::from("stm32flash")
}

fn run_stm32flash_query(port: &str) -> Result<HashMap<String, String>> {
    let stm32flash = find_stm32flash();
    let output = Command::new(&stm32flash)
        .arg(port)
        .output()
        .with_context(|| format!(
            "stm32flash not found (tried {} and PATH). Install with: usbipd-rs --install stm32flash",
            stm32flash.display()
        ))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        // stm32flash prints "Failed to init device, attempt N." when the chip
        // doesn't respond at 0x7F — that's the normal "no STM32 here" case,
        // not an install/usage error. Surface a one-line summary plus a hint
        // about BOOT0 (CH340 boards don't auto-trigger bootloader mode).
        let combined: Vec<&str> = stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| !l.starts_with("stm32flash "))
            .filter(|l| !l.starts_with("http"))
            .filter(|l| !l.starts_with("Using Parser"))
            .filter(|l| !l.starts_with("Interface"))
            .collect();
        let summary = combined.last().copied().unwrap_or("no response");
        anyhow::bail!(
            "{summary}\n\
             Hint: stm32flash needs the chip in system bootloader mode.\n\
             Pull BOOT0 HIGH and press RESET, then re-run --probe.\n\
             (Generic CH340 boards don't wire DTR/RTS to BOOT0/RESET, so this\n\
              cannot be triggered automatically.)"
        );
    }
    Ok(parse_stm32flash_output(&stdout))
}

fn parse_stm32flash_output(s: &str) -> HashMap<String, String> {
    // Typical successful output (one field per line, ':' separator):
    //   Version      : 0x22
    //   Option 1     : 0x00
    //   Option 2     : 0x00
    //   Device ID    : 0x0414 (STM32F10xxx High-density)
    //   - RAM        : Up to 64KiB  (12288b reserved by bootloader)
    //   - Flash      : Up to 512KiB (size first sector: 4x2048)
    //   - Option RAM : 16b
    //   - System RAM : 2KiB
    let mut info = HashMap::new();
    let interesting = [
        "Version",
        "Option 1",
        "Option 2",
        "Device ID",
        "RAM",
        "Flash",
        "Option RAM",
        "System RAM",
    ];
    for raw in s.lines() {
        // Strip leading '- ' (used for RAM/Flash/Option RAM/System RAM rows).
        let line = raw.trim().trim_start_matches('-').trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            if interesting.contains(&key) {
                info.insert(key.to_string(), v.trim().to_string());
            }
        }
    }
    info
}

fn run_ftdi_info(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    // Pull cached descriptor info from nusb's enumeration — no device IO,
    // no chip reset, safe to chain before any serial probe. Windows caches
    // product/serial strings via setupapi, but not the manufacturer string
    // (that field will be None on Win regardless of descriptor contents).
    let dev = nusb::list_devices()
        .wait()
        .context("nusb::list_devices() failed")?
        .find(|d| d.vendor_id() == vid && d.product_id() == pid)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "nusb could not find {:04x}:{:04x} (device replugged between listing and probe?)",
                vid, pid
            )
        })?;

    let mut info = HashMap::new();
    info.insert("VID:PID".into(), format!("{:04x}:{:04x}", vid, pid));
    if let Some(m) = dev.manufacturer_string() {
        info.insert("Manufacturer".into(), m.into());
    }
    if let Some(p) = dev.product_string() {
        info.insert("Product".into(), p.into());
    }
    if let Some(s) = dev.serial_number() {
        info.insert("Serial number".into(), s.into());
    }

    let bcd_dev = dev.device_version();
    let variant = ftdi_chip_variant(pid, bcd_dev);
    let bcd_str = format!("0x{bcd_dev:04x}");
    info.insert(
        "Chip variant".into(),
        if variant.is_empty() {
            format!("bcdDevice {bcd_str}")
        } else {
            format!("{variant} (bcdDevice {bcd_str})")
        },
    );

    // Windows: only populated for composite devices bound to usbccgp. An
    // FT232R bound straight to ftdibus reports zero interfaces here — skip
    // the field rather than print a misleading "0".
    let ifaces: Vec<String> = dev
        .interfaces()
        .map(|i| {
            let cls = i.class();
            let label = match cls {
                0xff => "Vendor-specific",
                0x02 => "CDC Communications",
                0x0a => "CDC Data",
                0x03 => "HID",
                _    => "Other",
            };
            format!("#{} {label} (0x{cls:02x})", i.interface_number())
        })
        .collect();
    if !ifaces.is_empty() {
        info.insert("Interfaces".into(), ifaces.join(", "));
    }

    // Windows-only: which kernel driver currently owns the device. For FTDI
    // boards the expected values are "FTDIBUS" (VCP unloaded) or "ftser2k" /
    // "ftdibus + Serial" (VCP loaded → COMx visible). Reveals when the driver
    // is missing without having to open Device Manager.
    #[cfg(windows)]
    if let Some(drv) = dev.driver() {
        if !drv.is_empty() {
            info.insert("Driver".into(), drv.into());
        }
    }

    Ok(info)
}

fn ftdi_chip_variant(pid: u16, bcd_device: u16) -> &'static str {
    // bcdDevice encodes the FTDI silicon revision; for PID 0x6001 (FT232x
    // family) the meaningful values are 0x0200/0x0400/0x0600. The R and RL
    // share 0x0600 — they're the same die in different packages, so listing
    // them together is correct.
    match (pid, bcd_device) {
        (0x6001, 0x0200) => "FT8U232AM",
        (0x6001, 0x0400) => "FT232BM",
        (0x6001, 0x0600) => "FT232R / FT232RL",
        _ => "",
    }
}

fn print_ftdi_info(info: &HashMap<String, String>, board_name: &str) {
    println!("  {:<20} {}", "Board:", board_name);
    let order = [
        "Manufacturer",
        "Product",
        "Serial number",
        "Chip variant",
        "Interfaces",
        "Driver",
        "VID:PID",
    ];
    for k in order {
        if let Some(v) = info.get(k) {
            println!("  {:<20} {}", format!("{k}:"), v);
        }
    }
}

fn print_stm32_info(info: &HashMap<String, String>, board_name: &str) {
    println!("  {:<20} {}", "Bridge:", board_name);
    let order = [
        ("Device ID",  "Device ID"),
        ("Flash",      "Flash"),
        ("RAM",        "RAM"),
        ("System RAM", "System ROM"),
        ("Option RAM", "Option RAM"),
        ("Option 1",   "Option byte 1"),
        ("Option 2",   "Option byte 2"),
        ("Version",    "Bootloader ver"),
    ];
    for (key, label) in order {
        if let Some(v) = info.get(key) {
            println!("  {:<20} {}", format!("{label}:"), v);
        }
    }
}

fn run_dfu_info(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    // `dfu-util -l` lists every DFU device's alt settings + DfuSe memory map
    // on stdout (the version banner goes to stderr). Read-only; no chip reset.
    let output = Command::new("dfu-util")
        .arg("-l")
        .output()
        .context("dfu-util not found on PATH (install with: usbipd-rs --install dfu-util)")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let info = parse_dfu_output(&stdout, vid, pid);
    if info.is_empty() {
        // dfu-util ran but didn't list our device — usually a missing libusb
        // binding (Windows needs WinUSB on the DFU interface) or the chip left
        // bootloader mode between listing and probe.
        let hint = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .find(|l| l.contains("Cannot open") || l.contains("libusb") || l.contains("permission"))
            .unwrap_or(
                "dfu-util did not list this device — it may have left DFU mode, \
                 or (on Windows) the DFU interface needs a WinUSB driver \
                 (run `usbipd-rs --install zadig`).",
            );
        anyhow::bail!("{hint}");
    }
    Ok(info)
}

fn parse_dfu_output(s: &str, vid: u16, pid: u16) -> HashMap<String, String> {
    // Each matching line looks like:
    //   Found DFU: [0483:df11] ver=2200, devnum=5, cfg=1, intf=0, path="20-1.4",
    //              alt=0, name="@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg",
    //              serial="3576345C3137"
    // One line per alt setting; we aggregate them into a single report.
    let target = format!("[{:04x}:{:04x}]", vid, pid);
    let mut info = HashMap::new();
    let mut alt_count = 0u32;
    let mut regions: Vec<String> = Vec::new();

    for line in s.lines() {
        let line = line.trim();
        if !line.starts_with("Found DFU:") || !line.contains(&target) {
            continue;
        }
        alt_count += 1;

        if let Some(v) = dfu_field(line, "ver") {
            info.entry("bcdDevice".to_string()).or_insert(format!("0x{v}"));
        }
        if let Some(v) = dfu_field(line, "serial") {
            info.entry("Serial number".to_string()).or_insert(v);
        }
        if let Some(name) = dfu_field(line, "name") {
            if let Some((label, detail, flash_kb)) = format_dfu_region(&name) {
                regions.push(format!("{label}: {detail}"));
                if let Some(kb) = flash_kb {
                    info.insert("Flash size".to_string(), format!("{kb} KB"));
                }
            }
        }
    }

    if alt_count == 0 {
        return info;
    }
    info.insert("VID:PID".to_string(), format!("{:04x}:{:04x}", vid, pid));
    info.insert("Alt settings".to_string(), alt_count.to_string());
    if !regions.is_empty() {
        info.insert("Memory map".to_string(), regions.join("\n"));
    }
    info
}

/// Pull the value of `key=` from a dfu-util listing line. Quoted values
/// (`name="..."`, `serial="..."`) run to the closing quote; bare values
/// (`ver=2200`) run to the next comma.
fn dfu_field(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let idx = line.find(&needle)?;
    let rest = &line[idx + needle.len()..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(stripped[..end].to_string())
    } else {
        let end = rest.find(',').unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

/// Turn a DfuSe alt-setting name into (label, detail, flash_kb).
/// Input: `@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg`
/// flash_kb is Some only for the internal-flash region.
fn format_dfu_region(name: &str) -> Option<(String, String, Option<u64>)> {
    let mut parts = name.splitn(3, '/');
    let label = parts.next()?.trim().trim_start_matches('@').trim().to_string();
    let addr = parts.next()?.trim();
    let layout = parts.next().unwrap_or("").trim();

    if label.eq_ignore_ascii_case("Internal Flash") {
        if let Some(kb) = dfu_region_size_kb(layout) {
            return Some((label, format!("{kb} KB @ {addr} ({layout})"), Some(kb)));
        }
    }
    let detail = if layout.is_empty() {
        format!("@ {addr}")
    } else {
        format!("@ {addr} ({layout})")
    };
    Some((label, detail, None))
}

/// Sum a DfuSe sector layout like `04*016Kg,01*064Kg,07*128Kg` into total KB.
/// Each segment is `<count>*<pagesize><unit><flags>` (unit K = KiB, M = MiB).
fn dfu_region_size_kb(layout: &str) -> Option<u64> {
    let mut total_bytes: u64 = 0;
    for seg in layout.split(',') {
        let seg = seg.trim();
        let (count_s, rest) = seg.split_once('*')?;
        let count: u64 = count_s.trim().parse().ok()?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let page: u64 = digits.parse().ok()?;
        let unit = rest[digits.len()..].chars().next().unwrap_or(' ');
        let mult = match unit {
            'K' => 1024,
            'M' => 1024 * 1024,
            _ => 1,
        };
        total_bytes += count * page * mult;
    }
    Some(total_bytes / 1024)
}

fn print_dfu_info(info: &HashMap<String, String>, board_name: &str) {
    println!("  {:<20} {}", "Board:", board_name);
    let order = [
        ("Flash size",    "Flash size"),
        ("bcdDevice",     "bcdDevice"),
        ("Serial number", "Serial number"),
        ("Alt settings",  "Alt settings"),
        ("VID:PID",       "VID:PID"),
    ];
    for (key, label) in order {
        if let Some(v) = info.get(key) {
            println!("  {:<20} {}", format!("{label}:"), v);
        }
    }
    if let Some(mm) = info.get("Memory map") {
        println!("  {:<20}", "Memory map:");
        for region in mm.lines() {
            println!("    - {region}");
        }
    }
    println!(
        "  {:<20} (DFU mode reports a generic 0483:DF11; the flash geometry above\n  {:<20}  identifies the STM32 family — exact part can't be read over DFU)",
        "Note:", ""
    );
}

fn find_picotool() -> PathBuf {
    if let Ok(p) = std::env::var("PICOTOOL_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Candidates relative to our binary.
            //
            // Dev layout (manual placement):
            //   <project>/windows-driver/picotool/picotool.exe
            // Fresh `--install picotool` layout (zip has top-level picotool/):
            //   <project>/windows-driver/picotool/picotool/picotool.exe
            // Either way the binary sits at most 2 levels under `windows-driver/`.
            let candidates = [
                dir.join("picotool.exe"),
                dir.join("picotool").join("picotool.exe"),
                dir.join("..").join("..").join("windows-driver").join("picotool").join("picotool.exe"),
                dir.join("..").join("..").join("windows-driver").join("picotool").join("picotool").join("picotool.exe"),
                dir.join("..").join("..").join("..").join("windows-driver").join("picotool").join("picotool.exe"),
                dir.join("..").join("..").join("..").join("windows-driver").join("picotool").join("picotool").join("picotool.exe"),
            ];
            for c in candidates {
                if c.exists() {
                    return c;
                }
            }
        }
    }
    PathBuf::from("picotool")
}

fn run_picotool_info(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let picotool = find_picotool();
    let vid_hex = format!("0x{vid:04x}");
    let pid_hex = format!("0x{pid:04x}");

    let try_run = |args: &[&str]| Command::new(&picotool).args(args).output();

    // Newer picotool accepts --vid/--pid to disambiguate; older versions don't.
    let output = try_run(&["info", "-a", "--vid", &vid_hex, "--pid", &pid_hex])
        .or_else(|_| try_run(&["info", "-a"]))
        .with_context(|| {
            format!(
                "picotool not found (tried {} and PATH). Install with: scoop install picotool",
                picotool.display()
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        let combined = stderr
            .lines()
            .chain(stdout.lines())
            .filter(|l| !l.trim().is_empty())
            .filter(|l| !l.trim_start().starts_with("Use \"picotool help"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut msg = combined;
        if msg.contains("Zadig") || msg.to_lowercase().contains("install a driver") {
            msg.push_str(
                "\n\nFix: install Zadig (https://zadig.akeo.ie/), then replace the driver of\n\
                 the BOOTSEL interface with WinUSB. Steps:\n\
                   1. Run Zadig as admin\n\
                   2. Options menu → check 'List All Devices'\n\
                   3. Select the device showing as 'RP2 Boot' (or similar)\n\
                   4. Pick 'WinUSB' on the right side, click 'Replace Driver'\n\
                   5. Re-run usbipd-rs --probe",
            );
        }
        anyhow::bail!("{msg}");
    }
    Ok(parse_picotool_output(&stdout))
}

fn parse_picotool_output(s: &str) -> HashMap<String, String> {
    let mut info = HashMap::new();
    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Lines like "Device Information" / "Program Information" have no ':' → skip.
        let Some(idx) = line.find(':') else { continue };
        let key = line[..idx].trim();
        let value = line[idx + 1..].trim();
        if !key.is_empty() && !value.is_empty() && value != "none" {
            info.insert(key.to_string(), value.to_string());
        }
    }
    info
}

fn print_pico_info(info: &HashMap<String, String>, board_name: &str) {
    println!("  {:<20} {}", "Board:", board_name);
    let order = [
        // ── Silicon ──────────────────────────────────────────
        ("type",                "Chip"),
        ("revision",            "Chip revision"),
        ("package",             "Package"),
        ("chipid",              "Chip ID"),
        ("unique id",           "Unique ID"),
        // ── Flash ────────────────────────────────────────────
        ("flash size",          "Flash size"),
        ("flash devinfo",       "Flash devinfo"),
        // ── Boot ROM / CPU ───────────────────────────────────
        ("ROM version",         "ROM version"),
        ("rom gitrev",          "ROM gitrev"),
        ("default cpu",         "Default CPU"),
        ("current cpu",         "Current CPU"),
        ("available cpus",      "Available CPUs"),
        ("boot type",           "Boot type"),
        ("last booted partition","Last booted part"),
        // ── Security / debug ────────────────────────────────
        ("secure boot",         "Secure boot"),
        ("debug enable",        "Debug enable"),
        ("secure debug enable", "Secure debug"),
        ("boot_random",         "Boot random"),
        ("boot2 name",          "Boot2 stage"),
        // ── Application info (if firmware present) ───────────
        ("name",                "Program name"),
        ("description",         "Program desc"),
        ("features",            "Features"),
        ("sdk version",         "SDK version"),
        ("pico_board",          "pico_board"),
        ("binary start",        "Binary start"),
        ("binary end",          "Binary end"),
        ("build date",          "Build date"),
        ("build attributes",    "Build attrs"),
        ("build id",            "Build ID"),
        ("embedded drive",      "Embedded drive"),
    ];
    for (key, label) in order {
        if let Some(v) = info.get(key) {
            let display_value = if key == "flash devinfo" {
                format!("{} ({})", v, decode_flash_devinfo(v))
            } else {
                v.clone()
            };
            println!("  {:<20} {}", format!("{label}:"), display_value);
        }
    }
}

fn decode_flash_devinfo(v: &str) -> String {
    // RP2350 flash_devinfo bitfield (per pico-bootrom-rp2350):
    //   bits 0:3   CS0 size code (0 = OTP unset → SDK auto-detects)
    //   bits 4:7   CS1 size code
    //   bits 8:11  CS1 GPIO
    //   bit  14    D8h erase supported
    //   bit  15    CS1 present
    let raw = v.trim().trim_start_matches("0x");
    let Ok(n) = u32::from_str_radix(raw, 16) else { return "?".into() };
    let cs0 = n & 0xF;
    let cs1_present = (n >> 15) & 0x1;
    let cs1_size = (n >> 4) & 0xF;
    let cs1_gpio = (n >> 8) & 0xF;

    let mut parts = Vec::new();
    parts.push(if cs0 == 0 {
        "CS0 default — SDK auto-detects".into()
    } else {
        format!("CS0 code 0x{cs0:x}")
    });
    if cs1_present == 1 {
        parts.push(format!("CS1 on GPIO {cs1_gpio}, code 0x{cs1_size:x}"));
    }
    parts.join("; ")
}

fn run_daplink_query() -> Result<HashMap<String, String>> {
    let drive = find_daplink_drive().context(
        "DAPLink mass storage drive not found. \
         Check that the device shows up as a USB drive and DETAILS.TXT exists at its root.",
    )?;
    let path = drive.join("DETAILS.TXT");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Could not read {}", path.display()))?;
    let mut info = parse_daplink_details(&content);
    info.insert("Drive letter".to_string(), drive.to_string_lossy().into_owned());
    Ok(info)
}

fn find_daplink_drive() -> Option<PathBuf> {
    // Windows: DAPLink mounts as a lettered drive (D:\, E:\, ...).
    if current_os() == Os::Windows {
        for letter in 'A'..='Z' {
            let drive = PathBuf::from(format!("{letter}:\\"));
            if drive.join("DETAILS.TXT").is_file() {
                return Some(drive);
            }
        }
        return None;
    }

    // macOS / Linux: removable media mounts as a subdirectory under a few
    // well-known roots. `/Volumes/*` (macOS), `/media/*` and `/mnt/*` (Linux),
    // plus the per-user `/run/media/<user>/<label>` layout (one level deeper).
    let direct_roots = ["/Volumes", "/media", "/mnt"];
    let nested_roots = ["/run/media", "/media"];

    for root in direct_roots {
        if let Some(drive) = scan_mount_children(Path::new(root)) {
            return Some(drive);
        }
    }
    for root in nested_roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            for user_dir in entries.flatten() {
                if let Some(drive) = scan_mount_children(&user_dir.path()) {
                    return Some(drive);
                }
            }
        }
    }
    None
}

/// Return the first immediate child of `root` that holds a `DETAILS.TXT` file.
fn scan_mount_children(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("DETAILS.TXT").is_file() {
            return Some(path);
        }
    }
    None
}

fn parse_daplink_details(s: &str) -> HashMap<String, String> {
    let mut info = HashMap::new();
    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim();
        let value = v.trim();
        if !key.is_empty() && !value.is_empty() {
            info.insert(key.to_string(), value.to_string());
        }
    }
    info
}

fn print_daplink_info(info: &HashMap<String, String>, board_name: &str) {
    println!("  {:<20} {}", "Board:", board_name);
    let order = [
        ("Drive letter",       "MSD mount"),
        ("Unique ID",          "Unique ID"),
        ("HIC ID",             "HIC ID"),
        ("Daplink Mode",       "DAPLink mode"),
        ("Interface Version",  "Interface FW"),
        ("Bootloader Version", "Bootloader FW"),
        ("Git SHA",            "DAPLink commit"),
        ("Local Mods",         "Local mods"),
        ("USB Interfaces",     "USB interfaces"),
        ("Auto Reset",         "Auto reset"),
        ("Automation allowed", "Automation"),
        ("Overflow detection", "Overflow det."),
        ("Remount count",      "Remount count"),
        ("URL",                "Board URL"),
    ];
    for (key, label) in order {
        if let Some(v) = info.get(key) {
            println!("  {:<20} {}", format!("{label}:"), v);
        }
    }

    // Identify the specific board variant from Unique ID prefix and surface
    // the underlying target chip, since DAPLink itself is just the interface.
    if let Some(uid) = info.get("Unique ID") {
        if let Some((variant, target)) = microbit_identify(uid) {
            println!("  {:<20} {}", "Variant:", variant);
            println!("  {:<20} {}", "Target chip:", target);
        }
    }
}

fn run_pyocd_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    // Use `pyocd json --probes` for stable parseable output (vs. the table form
    // of `pyocd list -p`). This does NOT connect to or reset the SWD target —
    // chip identity comes from pyocd's internal board database keyed by USB
    // unique ID prefix.
    //
    // Force UTF-8 IO in the child Python so pyocd's checkmark glyphs don't
    // crash on Windows consoles that default to cp950/cp936/etc.
    let output = Command::new("pyocd")
        .args(["json", "--probes"])
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .output()
        .context("pyocd not found on PATH (install with: pip install pyocd)")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        let last = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .unwrap_or("unknown");
        anyhow::bail!("{}", last);
    }
    Ok(parse_pyocd_output(&stdout, vid, pid))
}

fn parse_pyocd_output(s: &str, vid: u16, pid: u16) -> HashMap<String, String> {
    // pyocd JSON shape:
    //   { "boards": [ { "unique_id": "...", "info": "...",
    //                   "board_vendor": "...", "board_name": "...",
    //                   "target": "nrf52833", "vendor_name": "Arm",
    //                   "product_name": "BBC micro:bit CMSIS-DAP" } ] }
    //
    // We want fields from the first board entry. JSON parsing is small enough
    // here that adding serde_json isn't worth the compile-time cost — extract
    // each "key": "value" pair by string search.
    let mut info = HashMap::new();
    info.insert("USB filter".to_string(), format!("{:04x}:{:04x}", vid, pid));

    let keys = [
        "unique_id",
        "info",
        "board_vendor",
        "board_name",
        "target",
        "vendor_name",
        "product_name",
    ];
    for key in keys {
        if let Some(v) = extract_json_string(s, key) {
            info.insert(key.to_string(), v);
        }
    }
    info
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let idx = json.find(&needle)?;
    let rest = json[idx + needle.len()..].trim_start();
    let after = rest.strip_prefix('"')?;
    // Find unescaped closing quote. pyocd values don't contain backslashes,
    // but be defensive anyway.
    let mut end = 0;
    let bytes = after.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'\\' {
            end += 2;
            continue;
        }
        if bytes[end] == b'"' {
            return Some(after[..end].to_string());
        }
        end += 1;
    }
    None
}

fn print_pyocd_info(info: &HashMap<String, String>) {
    let order = [
        ("vendor_name",  "Probe vendor"),
        ("product_name", "Probe product"),
        ("unique_id",    "Probe unique ID"),
        ("board_vendor", "Board vendor"),
        ("board_name",   "Board name"),
        ("target",       "Target chip"),
        ("info",         "Combined"),
        ("USB filter",   "USB VID:PID"),
    ];
    let mut printed_any = false;
    for (key, label) in order {
        if let Some(v) = info.get(key) {
            println!("  {:<20} {}", format!("{label}:"), v);
            printed_any = true;
        }
    }
    if !printed_any {
        println!("  (pyocd returned no probe info — is the device still in the same port?)");
    }
}

struct StlinkControllerProfile {
    generation: &'static str,
    controller_mcu: &'static str,
    core: &'static str,
    architecture: &'static str,
    max_clock: &'static str,
    flash: &'static str,
    sram: &'static str,
    package: &'static str,
    supply: &'static str,
    flash_map: &'static str,
    sram_map: &'static str,
    system_memory: &'static str,
    option_bytes: &'static str,
    unique_id: &'static str,
    self_debug: &'static str,
    upstream: &'static str,
    downstream: &'static str,
    confidence: &'static str,
}

fn stlink_controller_profile(pid: u16) -> Option<StlinkControllerProfile> {
    Some(match pid {
        0x3748 => StlinkControllerProfile {
            generation: "ST-Link/V2",
            controller_mcu: "STM32F103C8T6/CBT6 (implementation-dependent)",
            core: "Arm Cortex-M3",
            architecture: "Armv7-M, Thumb/Thumb-2",
            max_clock: "72 MHz",
            flash: "64/128 KB (implementation-dependent)",
            sram: "20 KB",
            package: "Implementation-dependent",
            supply: "2.0-3.6 V",
            flash_map: "0x08000000 (size depends on controller variant)",
            sram_map: "0x20000000-0x20004FFF",
            system_memory: "0x1FFFF000-0x1FFFF7FF",
            option_bytes: "0x1FFFF800-0x1FFFF80F",
            unique_id: "96-bit UID at 0x1FFFF7E8",
            self_debug: "PA13/SWDIO, PA14/SWCLK, NRST; external probe required",
            upstream: "USB 2.0 Full Speed",
            downstream: "SWD/JTAG",
            confidence: "VID:PID identifies ST-Link/V2; exact MCU varies on clones",
        },
        0x374b => StlinkControllerProfile {
            generation: "ST-Link/V2-1",
            controller_mcu: "STM32F103CBT6",
            core: "Arm Cortex-M3",
            architecture: "Armv7-M, Thumb/Thumb-2",
            max_clock: "72 MHz",
            flash: "128 KB",
            sram: "20 KB",
            package: "LQFP48",
            supply: "2.0-3.6 V",
            flash_map: "0x08000000-0x0801FFFF",
            sram_map: "0x20000000-0x20004FFF",
            system_memory: "0x1FFFF000-0x1FFFF7FF",
            option_bytes: "0x1FFFF800-0x1FFFF80F",
            unique_id: "96-bit UID at 0x1FFFF7E8",
            self_debug: "PA13/SWDIO, PA14/SWCLK, NRST; external probe required",
            upstream: "USB 2.0 Full Speed composite device",
            downstream: "SWD (Nucleo on-board target link)",
            confidence: "Known ST-Link/V2-1 hardware profile",
        },
        0x374e | 0x374f => StlinkControllerProfile {
            generation: "ST-Link/V3",
            controller_mcu: "Not determined from VID:PID alone",
            core: "Not determined from VID:PID alone",
            architecture: "Implementation-dependent",
            max_clock: "Implementation-dependent",
            flash: "Implementation-dependent",
            sram: "Implementation-dependent",
            package: "Implementation-dependent",
            supply: "Implementation-dependent",
            flash_map: "Implementation-dependent",
            sram_map: "Implementation-dependent",
            system_memory: "Implementation-dependent",
            option_bytes: "Implementation-dependent",
            unique_id: "Implementation-dependent",
            self_debug: "Implementation-dependent; external probe required",
            upstream: "USB 2.0 High Speed capable",
            downstream: "SWD/JTAG",
            confidence: "VID:PID identifies ST-Link/V3 generation only",
        },
        _ => return None,
    })
}

/// Return only the USB-visible debug-controller architecture (layer 1).
/// This function enumerates cached descriptors and never opens the debug
/// interface, so it cannot issue an SWD/JTAG command or touch layer 2.
fn run_stlink_controller_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let profile = stlink_controller_profile(pid)
        .ok_or_else(|| anyhow::anyhow!("no layer-1 ST-Link profile for {vid:04x}:{pid:04x}"))?;
    // Descriptor details are optional; the architecture profile remains valid
    // from the VID:PID already present in the native USB list.
    let dev = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut devices| devices.find(|d| d.vendor_id() == vid && d.product_id() == pid));

    let mut info = HashMap::new();
    info.insert("Layer".into(), "1 - USB debug controller".into());
    info.insert("Generation".into(), profile.generation.into());
    info.insert("Controller MCU".into(), profile.controller_mcu.into());
    info.insert("Core".into(), profile.core.into());
    info.insert("Architecture".into(), profile.architecture.into());
    info.insert("Max clock".into(), profile.max_clock.into());
    info.insert("Flash".into(), profile.flash.into());
    info.insert("SRAM".into(), profile.sram.into());
    info.insert("Package".into(), profile.package.into());
    info.insert("Supply".into(), profile.supply.into());
    info.insert("Flash map".into(), profile.flash_map.into());
    info.insert("SRAM map".into(), profile.sram_map.into());
    info.insert("System memory".into(), profile.system_memory.into());
    info.insert("Option bytes".into(), profile.option_bytes.into());
    info.insert("Unique ID".into(), profile.unique_id.into());
    info.insert("Controller debug".into(), profile.self_debug.into());
    info.insert("Upstream".into(), profile.upstream.into());
    info.insert("Downstream".into(), profile.downstream.into());
    info.insert("USB identity".into(), format!("{vid:04x}:{pid:04x}"));
    info.insert("Identification".into(), profile.confidence.into());
    info.insert(
        "Firmware access".into(),
        "Not attempted; read-protection status unknown".into(),
    );
    info.insert(
        "Layer 2".into(),
        "Auto-probed read-only below (native SWD)".into(),
    );
    if let Some(dev) = dev {
        info.insert("Descriptor access".into(), "Available through nusb".into());
        if let Some(product) = dev.product_string() {
            info.insert("USB product".into(), product.into());
        }
        if let Some(serial) = dev.serial_number() {
            info.insert("USB serial".into(), serial.into());
        }
        info.insert(
            "USB device version".into(),
            format!("0x{:04x}", dev.device_version()),
        );
    } else {
        info.insert(
            "Descriptor access".into(),
            "Unavailable through nusb; VID:PID profile still shown".into(),
        );
    }
    if let Some(driver) = stlink_driver_check(pid) {
        let driver = match driver {
            StlinkDriver::WinUsb => "WinUSB".to_string(),
            StlinkDriver::Other(service) => service,
        };
        info.insert("Debug interface driver".into(), driver);
    }
    Ok(info)
}

fn print_stlink_controller_info(info: &HashMap<String, String>) {
    println!("  Architecture:");
    let order = [
        "Layer",
        "Generation",
        "Controller MCU",
        "Core",
        "Architecture",
        "Max clock",
        "Flash",
        "SRAM",
        "Package",
        "Supply",
        "Flash map",
        "SRAM map",
        "System memory",
        "Option bytes",
        "Unique ID",
        "Controller debug",
        "Upstream",
        "Downstream",
        "USB identity",
        "Descriptor access",
        "USB product",
        "USB serial",
        "USB device version",
        "Debug interface driver",
        "Identification",
        "Firmware access",
        "Layer 2",
    ];
    for key in order {
        if let Some(value) = info.get(key) {
            println!("  {:<24} {}", format!("{key}:"), value);
        }
    }
}

/// Where a given STM32 family keeps its flash-size word, 96-bit UID, and
/// read-out-protection option register. DBGMCU_IDCODE (0xE0042000) and CPUID
/// (0xE000ED00) are at fixed addresses across families, but these three move,
/// so we can only read them once DEV_ID tells us the family.
#[derive(Clone, Copy)]
struct StmFamily {
    name: &'static str,
    flash_size_addr: u32,
    uid_addr: u32,
    rdp_addr: u32,
    rdp_kind: RdpKind,
}

#[derive(Clone, Copy)]
enum RdpKind {
    /// FLASH_OBR with RDPRT in bit 1 (STM32F0/F1/F3).
    Obr,
    /// FLASH_OPTCR-style register with the RDP level byte in bits [15:8]
    /// (STM32F2/F4/F7): 0xAA = Level 0, 0xCC = Level 2, anything else = Level 1.
    OptByte,
}

// Per-family register address groups.
const ADDRS_F1:   (u32, u32, u32, RdpKind) = (0x1FFFF7E0, 0x1FFFF7E8, 0x4002201C, RdpKind::Obr);
const ADDRS_F0F3: (u32, u32, u32, RdpKind) = (0x1FFFF7CC, 0x1FFFF7AC, 0x4002201C, RdpKind::Obr);
const ADDRS_F2F4: (u32, u32, u32, RdpKind) = (0x1FFF7A22, 0x1FFF7A10, 0x40023C14, RdpKind::OptByte);
const ADDRS_F7:   (u32, u32, u32, RdpKind) = (0x1FF0F442, 0x1FF0F420, 0x40023C14, RdpKind::OptByte);

/// Map a 12-bit DBGMCU DEV_ID to its family name + register addresses.
fn stm_family(dev_id: u16) -> Option<StmFamily> {
    let (name, (flash_size_addr, uid_addr, rdp_addr, rdp_kind)) = match dev_id {
        // ── STM32F0 (Cortex-M0) ──
        0x440 => ("STM32F030x8 / F05x — Cortex-M0", ADDRS_F0F3),
        0x442 => ("STM32F030xC / F09x — Cortex-M0", ADDRS_F0F3),
        0x444 => ("STM32F03x — Cortex-M0", ADDRS_F0F3),
        0x445 => ("STM32F04x — Cortex-M0", ADDRS_F0F3),
        0x448 => ("STM32F07x — Cortex-M0", ADDRS_F0F3),
        // ── STM32F1 (Cortex-M3) ──
        0x412 => ("STM32F1 low-density (F101/102/103) — Cortex-M3", ADDRS_F1),
        0x410 => ("STM32F1 medium-density (F101/102/103) — Cortex-M3", ADDRS_F1),
        0x414 => ("STM32F1 high-density (F101/103) — Cortex-M3", ADDRS_F1),
        0x418 => ("STM32F1 connectivity line (F105/107) — Cortex-M3", ADDRS_F1),
        0x420 => ("STM32F100 medium-density value line — Cortex-M3", ADDRS_F1),
        0x428 => ("STM32F100 high-density value line — Cortex-M3", ADDRS_F1),
        0x430 => ("STM32F1 XL-density (F101/103) — Cortex-M3", ADDRS_F1),
        // ── STM32F2 (Cortex-M3) ──
        0x411 => ("STM32F2 — Cortex-M3", ADDRS_F2F4),
        // ── STM32F3 (Cortex-M4F) ──
        0x422 => ("STM32F302xB/C / F303xB/C / F358 — Cortex-M4F", ADDRS_F0F3),
        0x432 => ("STM32F373 / F378 — Cortex-M4F", ADDRS_F0F3),
        0x438 => ("STM32F303x4/6/8 / F334 / F328 — Cortex-M4F", ADDRS_F0F3),
        0x439 => ("STM32F301 / F302x6/8 / F318 — Cortex-M4F", ADDRS_F0F3),
        0x446 => ("STM32F302xD/E / F303xD/E / F398 — Cortex-M4F", ADDRS_F0F3),
        // ── STM32F4 (Cortex-M4F) ──
        0x413 => ("STM32F405/407/415/417 — Cortex-M4F", ADDRS_F2F4),
        0x419 => ("STM32F42x/43x — Cortex-M4F", ADDRS_F2F4),
        0x423 => ("STM32F401xB/C — Cortex-M4F", ADDRS_F2F4),
        0x431 => ("STM32F411 — Cortex-M4F", ADDRS_F2F4),
        0x433 => ("STM32F401xD/E — Cortex-M4F", ADDRS_F2F4),
        0x441 => ("STM32F412 — Cortex-M4F", ADDRS_F2F4),
        0x421 => ("STM32F446 — Cortex-M4F", ADDRS_F2F4),
        0x434 => ("STM32F469/479 — Cortex-M4F", ADDRS_F2F4),
        0x458 => ("STM32F410 — Cortex-M4F", ADDRS_F2F4),
        0x463 => ("STM32F413/423 — Cortex-M4F", ADDRS_F2F4),
        // ── STM32F7 (Cortex-M7) ──
        0x449 => ("STM32F74x/75x — Cortex-M7", ADDRS_F7),
        0x451 => ("STM32F76x/77x — Cortex-M7", ADDRS_F7),
        0x452 => ("STM32F72x/73x — Cortex-M7", ADDRS_F7),
        _ => return None,
    };
    Some(StmFamily { name, flash_size_addr, uid_addr, rdp_addr, rdp_kind })
}

/// Documented genuine-ST silicon REV_IDs (DBGMCU IDCODE bits [31:16]) for the
/// families most often cloned. STM32-compatible parts from GigaDevice (GD32),
/// CKS, APM32 (Geehy), MindMotion, etc. mirror ST's DEV_ID but report a REV_ID
/// outside ST's published set — so a known DEV_ID with an unknown REV_ID is a
/// strong "not genuine ST" signal. Returns `None` for families we don't track
/// closely enough to second-guess (then we make no clone claim).
fn stm32_known_revs(dev_id: u16) -> Option<&'static [u16]> {
    Some(match dev_id {
        0x412 => &[0x1000],                          // F1 low-density
        0x410 => &[0x0000, 0x2000, 0x2001, 0x2003],  // F1 medium-density
        0x414 => &[0x1000, 0x1001, 0x1003],          // F1 high-density
        0x418 => &[0x1000, 0x1001],                  // F1 connectivity line
        0x420 => &[0x1000, 0x1001],                  // F100 value line, MD
        0x428 => &[0x1000, 0x1001],                  // F100 value line, HD
        0x430 => &[0x1000],                          // F1 XL-density
        _ => return None,
    })
}

/// GigaDevice GD32 lines that mirror an ST DBGMCU DEV_ID (so they would decode
/// as STM32 by IDCODE alone). Keyed by the mirrored DEV_ID → (family, core, max
/// MHz). GD32 reports the SAME DEV_ID as the ST part it replaces, but a REV_ID
/// outside ST's published set (see `stm32_known_revs`) and a faster top clock
/// (108 MHz on the F1 line vs ST's 72 MHz). The package/pin letter (C=48, R=64,
/// V=100, Z=144) and temperature grade are NOT exposed over SWD, so the density
/// from the flash-size word is the finest model the silicon will tell us.
const GD32_FAMILIES: &[(u16, &str, &str, u16)] = &[
    (0x410, "GD32F103", "Cortex-M3", 108), // medium-density mirror (≤128 KB)
    (0x414, "GD32F103", "Cortex-M3", 108), // high-density mirror (256–512 KB)
    (0x418, "GD32F105", "Cortex-M3", 108), // connectivity-line mirror
    (0x430, "GD32F103", "Cortex-M3", 108), // XL-density mirror
];

/// Density letter for a GD32F10x from its flash size in KB (x4=16 … xE=512).
fn gd32_density(flash_kb: Option<u32>) -> &'static str {
    match flash_kb {
        Some(k) if k >= 512 => "xE",
        Some(k) if k >= 384 => "xD",
        Some(k) if k >= 256 => "xC",
        Some(k) if k >= 128 => "xB",
        Some(k) if k >= 64 => "x8",
        Some(k) if k >= 32 => "x6",
        Some(k) if k >= 16 => "x4",
        _ => "",
    }
}

/// Resolve a GigaDevice GD32 model from the read-only signals: a known ST DEV_ID
/// reporting a non-ST REV_ID (the clone signature) selects the family, and the
/// flash size selects the density. Returns `(display_name, core, max_mhz)`, or
/// `None` when this isn't a recognized GD32 (then the STM32 name/clone-flag path
/// runs). The canonical 64-pin part is cited as an example for the F103 line;
/// the exact package can't be read over SWD.
fn gd32_identify(dev_id: u16, rev_id: u16, flash_kb: Option<u32>) -> Option<(String, &'static str, u16)> {
    // Must look like a clone first: ST DEV_ID we know, REV_ID ST never shipped.
    match stm32_known_revs(dev_id) {
        Some(revs) if !revs.contains(&rev_id) => {}
        _ => return None,
    }
    let &(_, family, core, max_mhz) = GD32_FAMILIES.iter().find(|&&(id, ..)| id == dev_id)?;
    let density = gd32_density(flash_kb);
    let example = if family == "GD32F103" {
        match density {
            "xE" => " — e.g. GD32F103RET6 (64-pin, 512 KB)",
            "xD" => " — e.g. GD32F103RDT6 (384 KB)",
            "xC" => " — e.g. GD32F103RCT6 (256 KB)",
            "xB" => " — e.g. GD32F103CBT6 (128 KB)",
            "x8" => " — e.g. GD32F103C8T6 (64 KB)",
            "x6" => " — e.g. GD32F103C6T6 (32 KB)",
            "x4" => " — e.g. GD32F103C4T6 (16 KB)",
            _ => "",
        }
    } else {
        ""
    };
    Some((format!("{family}{density} (GigaDevice){example} — {core}"), core, max_mhz))
}

/// What the ST-Link debug interface's WinUSB binding looks like on Windows.
enum StlinkDriver {
    /// Bound to WinUSB — pyocd/libusb can open it. (A stale/broken WinUSB bind
    /// still reports as this; only a runtime probe failure exposes that case.)
    WinUsb,
    /// Present but bound to some other service (e.g. `usbccgp` / a vendor
    /// driver / nothing) — pyocd cannot open it until it's switched to WinUSB.
    Other(String),
}

/// Check which driver the ST-Link *debug* interface is bound to. pyocd/libusb
/// can only open it when it's WinUSB (installed via Zadig — `--install zadig`);
/// a wrong/missing driver makes every SWD read fail with a misleading
/// "No device connected", so we look *before* probing and tell the user how to
/// fix it. Returns `None` when the binding can't be determined (then we probe
/// anyway rather than block on a guess). Windows-only — pyocd uses libusb/udev
/// elsewhere, so there is nothing to check.
#[cfg(windows)]
fn stlink_driver_check(pid: u16) -> Option<StlinkDriver> {
    // Match the debug interface: MI_00 on the composite V2-1/V3. Fall back to
    // the bare device (no MI_xx) only for the single-interface V2 — otherwise
    // the composite *parent* (also has no MI_, service `usbccgp`) gets picked
    // and we'd misreport the debug interface as not-WinUSB.
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         $all = Get-PnpDevice -PresentOnly | Where-Object {{ $_.InstanceId -match 'VID_0483&PID_{pid:04X}' }};\
         $d = $all | Where-Object {{ $_.InstanceId -match '&MI_00' }} | Select-Object -First 1;\
         if (-not $d) {{ $d = $all | Where-Object {{ $_.InstanceId -notmatch '&MI_' }} | Select-Object -First 1 }};\
         if ($d) {{\
           $svc=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_Service').Data;\
           $node=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DevNodeStatus').Data;\
           $problem=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data;\
           $filters=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_UpperFilters').Data -join ',';\
           $inf=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data;\
           $provider=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverProvider').Data;\
           $version=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverVersion').Data;\
           Write-Output \"SERVICE=$svc\"; Write-Output \"DEVNODE=$node\"; Write-Output \"PROBLEM=$problem\"; Write-Output \"FILTERS=$filters\";\
           Write-Output \"INF=$inf\"; Write-Output \"PROVIDER=$provider\"; Write-Output \"VERSION=$version\"\
         }}"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let svc = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("SERVICE="))?
        .trim()
        .to_string();
    let started = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("DEVNODE="))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|flags| flags & 0x8 != 0)
        .unwrap_or(true);
    let problem = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("PROBLEM="))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let field = |name: &str| {
        stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .unwrap_or("?")
            .trim()
    };
    if svc.is_empty() {
        return Some(StlinkDriver::Other(format!(
            "no function driver is bound (problem code {problem}); INF {}, provider {}, version {}",
            value_or_dash(field("INF=")),
            value_or_dash(field("PROVIDER=")),
            value_or_dash(field("VERSION="))
        )));
    }
    Some(if svc.eq_ignore_ascii_case("WinUSB") && started {
        StlinkDriver::WinUsb
    } else if svc.eq_ignore_ascii_case("WinUSB") {
        let filters = field("FILTERS=");
        let conflict = if filters.is_empty() {
            String::new()
        } else {
            format!("; conflicting upper filter(s): {filters}")
        };
        StlinkDriver::Other(format!(
            "WinUSB device node stopped (problem code {problem}){conflict}; INF {}, provider {}, version {}",
            field("INF="),
            field("PROVIDER="),
            field("VERSION=")
        ))
    } else {
        StlinkDriver::Other(svc)
    })
}

#[cfg(not(windows))]
fn stlink_driver_check(_pid: u16) -> Option<StlinkDriver> {
    None
}

/// Layer-2 probe: read the downstream SWD target's identity natively (read-only,
/// via `nusb` — no probe-rs/pyocd, no halt/reset). Returns a `HashMap` keyed for
/// `print_stlink_target_info`. Auto-detects whether a target is present: with no
/// SWD response only the probe-level keys are returned (one-layer result).
fn run_stlink_target_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let mut info = HashMap::new();

    let probe = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid));
    let Some(probe) = probe else {
        info.insert("Layer 2".into(), "SKIP - ST-Link USB device not found".into());
        return Ok(info);
    };

    let mut link = match StlinkLink::open_device(&probe) {
        Ok(l) => l,
        Err(e) => {
            info.insert("Layer 2".into(), format!("SKIP - cannot open debug interface: {e}"));
            #[cfg(windows)]
            info.insert(
                "Fix".into(),
                "bind WinUSB to MI_00: usbipd-rs --install-driver --confirm (Administrator)".into(),
            );
            return Ok(info);
        }
    };

    if let Ok(v) = link.get_version() {
        let volt = link
            .get_voltage_mv()
            .map(|mv| format!(", target {:.2} V", mv as f64 / 1000.0))
            .unwrap_or_default();
        info.insert("Probe".into(), format!("{v}{volt}"));
    }

    let status = link.enter_swd().unwrap_or(0);
    match link.read_idcode() {
        Ok(dpidr) => {
            info.insert("DP IDCODE".into(), format!("0x{dpidr:08X}"));
        }
        Err(_) => {
            info.insert(
                "Layer 2".into(),
                format!("none - SWD target did not answer (enter-SWD status 0x{status:02X})"),
            );
            info.insert("Hint".into(), swd_no_target_hint(&mut link));
            return Ok(info);
        }
    }

    let db = load_chip_db();
    let (regs, dev_id, resolved) = stlink_read_regs(&mut link, &db);
    for (k, v) in format_target_rows(&regs, dev_id, resolved.as_ref()) {
        info.insert(k.to_string(), v);
    }
    Ok(info)
}

// ============================================================================
// Optional external chip database (stlink-style `etc/chips/*.chip` files)
//
// The built-in `stm_family` table is the always-available base library. A
// `.chip` file lets a user add or override a model WITHOUT recompiling: a model
// with a matching `.chip` takes priority; otherwise the built-in table is used.
// The format is a subset of stlink-org/stlink's, so real stlink chip files drop
// in unmodified (unknown keys are ignored).
// ============================================================================

/// A chip definition parsed from a `.chip` file. Only the fields this tool uses
/// are kept.
struct ChipDef {
    dev_id: u16,
    name: String,
    flash_size_addr: Option<u32>,
    sram_kb: Option<u32>,
    source: String,
}

/// Parse a C-style integer as written in `.chip` files: `0x...` hex or decimal.
fn parse_chip_int(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches([',', ';']);
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Parse one `.chip` file's text. Returns `None` if it has no `chip_id`.
fn parse_chip_file(text: &str) -> Option<ChipDef> {
    let mut dev_id = None;
    let mut name = None;
    let mut flash_size_addr = None;
    let mut sram_bytes = None;
    for raw in text.lines() {
        // Strip `// ...` and `# ...` comments, then split key/value.
        let line = raw.split("//").next().unwrap_or(raw);
        let line = line.split('#').next().unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let (key, val) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        match key {
            "chip_id" => dev_id = parse_chip_int(val).map(|v| v as u16),
            "dev_type" => name = Some(val.replace('_', " ")),
            "flash_size_reg" => flash_size_addr = parse_chip_int(val),
            "sram_size" => sram_bytes = parse_chip_int(val),
            _ => {}
        }
    }
    let dev_id = dev_id?;
    Some(ChipDef {
        dev_id,
        name: name.unwrap_or_else(|| format!("STM32 (chip_id 0x{dev_id:03X})")),
        flash_size_addr,
        sram_kb: sram_bytes.map(|b| b / 1024),
        source: String::new(),
    })
}

/// Directories searched for `.chip` files: `etc/chips/` relative to the working
/// directory (running from a checkout) and `etc/chips` / `chips` next to the
/// executable (running a packaged binary).
fn chip_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("etc/chips")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("etc/chips"));
            dirs.push(dir.join("chips"));
        }
    }
    dirs
}

/// Load every readable `.chip` file from the search directories.
fn load_chip_db() -> Vec<ChipDef> {
    let mut defs = Vec::new();
    for dir in chip_search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("chip") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(mut def) = parse_chip_file(&text) {
                    def.source = path.display().to_string();
                    defs.push(def);
                }
            }
        }
    }
    defs
}

/// Resolved chip parameters used by the native reader: `.chip` overrides the
/// built-in name / flash address / SRAM; the (verified) UID and RDP register
/// addresses always come from the built-in family table.
struct ResolvedChip {
    name: String,
    flash_size_addr: Option<u32>,
    uid_addr: Option<u32>,
    rdp_addr: Option<u32>,
    rdp_kind: Option<RdpKind>,
    sram_kb: Option<u32>,
    source: String,
}

/// Authoritative main-SRAM sizes (KB) for built-in families, taken from the
/// stlink chip database. Used when no `.chip` file supplies one.
fn builtin_sram_kb(dev_id: u16) -> Option<u32> {
    Some(match dev_id {
        0x412 => 10,
        0x410 => 20,
        0x414 | 0x418 => 64,
        0x411 => 128,
        0x423 => 64,
        0x433 => 96,
        0x413 => 192,
        0x419 => 256,
        0x431 | 0x421 => 128,
        0x441 => 256,
        0x449 => 320,
        0x451 => 512,
        0x452 => 256,
        _ => return None,
    })
}

/// Resolve a DBGMCU DEV_ID into chip parameters, preferring a matching `.chip`
/// file over the built-in table. Returns `None` if neither knows the model.
fn resolve_chip(dev_id: u16, db: &[ChipDef]) -> Option<ResolvedChip> {
    let builtin = stm_family(dev_id);
    match db.iter().find(|c| c.dev_id == dev_id) {
        Some(chip) => Some(ResolvedChip {
            name: chip.name.clone(),
            flash_size_addr: chip.flash_size_addr.or(builtin.map(|f| f.flash_size_addr)),
            uid_addr: builtin.map(|f| f.uid_addr),
            rdp_addr: builtin.map(|f| f.rdp_addr),
            rdp_kind: builtin.map(|f| f.rdp_kind),
            sram_kb: chip.sram_kb.or_else(|| builtin_sram_kb(dev_id)),
            source: chip.source.clone(),
        }),
        None => builtin.map(|f| ResolvedChip {
            name: f.name.to_string(),
            flash_size_addr: Some(f.flash_size_addr),
            uid_addr: Some(f.uid_addr),
            rdp_addr: Some(f.rdp_addr),
            rdp_kind: Some(f.rdp_kind),
            sram_kb: builtin_sram_kb(dev_id),
            source: "built-in".to_string(),
        }),
    }
}

/// Decode an Arm Cortex-M `CPUID` (0xE000ED00) into a "core rNpM (CPUID …)"
/// string. Used by both the pyocd-backed and native SWD paths.
fn cortex_core(cpuid: u32) -> String {
    let partno = (cpuid >> 4) & 0xFFF;
    let variant = (cpuid >> 20) & 0xF;
    let revision = cpuid & 0xF;
    let core = match partno {
        0xC20 => "Cortex-M0",
        0xC60 => "Cortex-M0+",
        0xC21 => "Cortex-M1",
        0xC23 => "Cortex-M3",
        0xC24 => "Cortex-M4",
        0xC27 => "Cortex-M7",
        0xD20 => "Cortex-M23",
        0xD21 => "Cortex-M33",
        _ => "Cortex-M (unknown)",
    };
    format!("{core} r{variant}p{revision} (CPUID 0x{cpuid:08X})")
}

/// Decode a flash read-protection register value into a human description.
fn decode_rdp(opt: u32, kind: RdpKind) -> String {
    match kind {
        RdpKind::Obr => {
            let mut s = if (opt >> 1) & 1 == 1 {
                "Enabled — flash read-protected (RDP active)".to_string()
            } else {
                "Disabled — flash readable (RDP Level 0)".to_string()
            };
            if opt & 1 == 1 {
                s.push_str(", OPTERR set");
            }
            s
        }
        RdpKind::OptByte => {
            let rdp = (opt >> 8) & 0xFF;
            match rdp {
                0xAA => "Disabled — flash readable (RDP Level 0)".to_string(),
                0xCC => "Enabled — RDP Level 2 (permanent, debug locked)".to_string(),
                _ => format!("Enabled — RDP Level 1 (RDP byte 0x{rdp:02X})"),
            }
        }
    }
}

fn decode_stlink_regs(regs: &HashMap<u32, Vec<u32>>, dev_id: Option<u16>) -> HashMap<String, String> {
    let mut info = HashMap::new();
    let first = |addr: u32| regs.get(&addr).and_then(|w| w.first()).copied();

    // ── Core (CPUID is universal across ARMv6-M/ARMv7-M) ──
    if let Some(cpuid) = first(0xE000ED00) {
        info.insert("Core".to_string(), cortex_core(cpuid));
    }

    let fam = dev_id.and_then(stm_family);
    if dev_id == Some(0x421) {
        info.insert(
            "Architecture".to_string(),
            "Armv7E-M, Thumb-2, DSP, single-precision FPU".to_string(),
        );
        info.insert("Max clock".to_string(), "180 MHz".to_string());
        info.insert("SRAM".to_string(), "128 KB".to_string());
        info.insert(
            "Identification".to_string(),
            "DBGMCU DEV_ID identifies STM32F446 family; package suffix not readable over SWD"
                .to_string(),
        );
    }
    info.insert("Access".to_string(), "Read-only identity registers".to_string());
    info.insert("Transport".to_string(), "SWD at 100 kHz".to_string());

    // ── Device ID + revision ──
    if let Some(idcode) = first(0xE0042000) {
        let did = (idcode & 0xFFF) as u16;
        let rev_id = ((idcode >> 16) & 0xFFFF) as u16;
        // Flash size (KB) is decoded again below for the Flash rows, but GD32
        // model resolution needs it now to pick the density letter.
        let flash_kb = fam
            .and_then(|f| regs.get(&f.flash_size_addr))
            .and_then(|w| w.first().copied())
            .map(|v| v & 0xFFFF)
            .filter(|&kb| kb != 0 && kb != 0xFFFF);

        // GigaDevice GD32 mirrors ST's DEV_ID; when the REV_ID proves it's a
        // clone we lead with the GD32 model, not the ST family name.
        let gd32 = gd32_identify(did, rev_id, flash_kb);
        if let Some((name, _core, max_mhz)) = &gd32 {
            info.insert("Device ID".to_string(), format!("0x{did:03X} — {name}"));
            info.insert(
                "Max clock".to_string(),
                format!("{max_mhz} MHz (GigaDevice GD32; ST's F103 tops out at 72 MHz)"),
            );
        } else {
            let name = fam.map(|f| f.name).unwrap_or("unknown (unrecognized STM32 DEV_ID)");
            info.insert("Device ID".to_string(), format!("0x{did:03X} — {name}"));
        }

        // REV_ID → silicon-revision letter is only well-defined for genuine ST
        // parts (GD32 REV_IDs don't follow ST's scheme); cover ST's 0x410 here.
        let rev = if gd32.is_none() && did == 0x410 {
            match rev_id {
                0x0000 => " (rev A)",
                0x2000 => " (rev B)",
                0x2001 => " (rev Z)",
                0x2003 => " (rev 1/2/3/X/Y)",
                _ => "",
            }
        } else {
            ""
        };
        info.insert("Revision".to_string(), format!("0x{rev_id:04X}{rev}"));

        // Provenance note: firm for a recognized GD32, generic for any other
        // STM32-compatible clone (known DEV_ID, REV_ID outside ST's set).
        if gd32.is_some() {
            info.insert(
                "Vendor".to_string(),
                format!(
                    "GigaDevice GD32 — pin-compatible with ST's STM32F103 but NOT genuine ST silicon. \
                     DEV_ID 0x{did:03X} mirrors ST; REV_ID 0x{rev_id:04X} (outside ST's set) confirms GD32. \
                     Package/pin (C=48 R=64 V=100 Z=144) + temp grade are not SWD-readable."
                ),
            );
        } else if let Some(revs) = stm32_known_revs(did) {
            if !revs.contains(&rev_id) {
                info.insert(
                    "Vendor".to_string(),
                    format!(
                        "likely an STM32-compatible clone (GigaDevice GD32 / CKS / APM32 / MindMotion) — \
                         REV_ID 0x{rev_id:04X} is not a documented ST revision for DEV_ID 0x{did:03X}"
                    ),
                );
            }
        }
    }

    // ── Flash size / UID / RDP — only readable once we know the family ──
    if let Some(fam) = fam {
        if let Some(words) = regs.get(&fam.flash_size_addr) {
            let kb = words[0] & 0xFFFF;
            // 0x0000 / 0xFFFF mean the field is unprogrammed or unreadable.
            if kb != 0 && kb != 0xFFFF {
                info.insert("Flash size".to_string(), format!("{kb} KB"));
                info.insert(
                    "Flash map".to_string(),
                    format!("0x08000000-0x{:08X}", 0x08000000u32 + kb * 1024 - 1),
                );
            }
        }

        if let Some(w) = regs.get(&fam.uid_addr) {
            // All-0xFF means the read faulted (wrong address / locked), not a
            // real UID — skip it rather than print a bogus serial.
            let blank = w.iter().take(3).all(|&x| x == 0xFFFFFFFF);
            if w.len() >= 3 && !blank {
                // 96-bit UID, printed most-significant word first.
                info.insert(
                    "Unique ID".to_string(),
                    format!("{:08X} {:08X} {:08X}", w[2], w[1], w[0]),
                );
            }
        }

        if let Some(opt) = first(fam.rdp_addr) {
            info.insert("Read protection".to_string(), decode_rdp(opt, fam.rdp_kind));
        }
    }

    info
}

fn print_stlink_target_info(info: &HashMap<String, String>, board: &str) {
    // Probe-level keys (always present once the debug interface opens).
    for key in ["Probe", "DP IDCODE"] {
        if let Some(value) = info.get(key) {
            println!("  {:<16} {value}", format!("{key}:"));
        }
    }

    // No downstream target: one-layer result. Show the reason and stop.
    if !info.contains_key("Device ID") {
        if let Some(state) = info.get("Layer 2") {
            println!("  Layer 2:         {state}");
        } else {
            println!("  Layer 2:         none - no SWD/JTAG target detected");
        }
        if let Some(fix) = info.get("Fix") {
            println!("  Fix:             {fix}");
        }
        if let Some(hint) = info.get("Hint") {
            println!("  Hint:            {hint}");
        }
        return;
    }

    // Two-layer result: a target answered. Print its read-only identity.
    println!("  Layer 2 - downstream SWD target (read-only) via {board}:");
    for key in [
        "Device ID", "Revision", "Vendor", "Core", "Architecture", "Max clock", "Flash size",
        "Flash map", "SRAM", "Unique ID", "Read protection", "Identification",
        "Transport", "Access", "Source",
    ] {
        if let Some(v) = info.get(key) {
            println!("    {:<16} {v}", format!("{key}:"));
        }
    }
}

fn microbit_identify(unique_id: &str) -> Option<(&'static str, &'static str)> {
    // First 4 hex digits of the Unique ID identify the board hardware revision
    // (this prefix is assigned by Microbit Foundation / DAPLink board database).
    let prefix = unique_id.get(..4)?;
    let nrf51 = "nRF51822 — Cortex-M0, 16 KB SRAM, 256 KB flash, 16 MHz, BLE 4.0";
    let nrf52 = "nRF52833 — Cortex-M4F, 128 KB SRAM, 512 KB flash, 64 MHz, BLE 5.x";
    Some(match prefix {
        "9900" => ("BBC micro:bit V1.3",          nrf51),
        "9901" => ("BBC micro:bit V1.5",          nrf51),
        "9903" => ("BBC micro:bit V2.0",          nrf52),
        "9904" => ("BBC micro:bit V2.21",         nrf52),
        "9905" => ("BBC micro:bit V2.21 (later)", nrf52),
        "9906" => ("BBC micro:bit V2.x",          nrf52),
        _ => return None,
    })
}

fn avr_chip_summary(mcu: &str) -> &'static str {
    match mcu.to_ascii_lowercase().as_str() {
        "atmega328p" | "atmega328" => "32 KB flash, 2 KB SRAM, 1 KB EEPROM, 16 MHz",
        "atmega2560" => "256 KB flash, 8 KB SRAM, 4 KB EEPROM, 16 MHz",
        "atmega32u4" => "32 KB flash, 2.5 KB SRAM, 1 KB EEPROM, 16 MHz, native USB",
        "atmega168" => "16 KB flash, 1 KB SRAM, 512 B EEPROM, 16 MHz",
        _ => "",
    }
}

fn print_table(rows: &[[String; 4]]) {
    let mut widths = HEADERS.map(UnicodeWidthStr::width);
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }

    let render = |cells: &[&str; 4]| -> String {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(&pad_display(cell, widths[i]));
            if i + 1 < cells.len() {
                out.push_str("  ");
            }
        }
        out.trim_end().to_string()
    };

    println!("{}", render(&HEADERS));

    let total: usize = widths.iter().sum::<usize>() + (widths.len() - 1) * 2;
    println!("{}", "-".repeat(total));

    for r in rows {
        let cells: [&str; 4] = [&r[0], &r[1], &r[2], &r[3]];
        println!("{}", render(&cells));
    }
}

fn pad_display(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(s.len() + (width - w));
        out.push_str(s);
        for _ in 0..(width - w) {
            out.push(' ');
        }
        out
    }
}

#[derive(Debug)]
struct Entry {
    busid: String,
    vidpid: String,
    device: String,
}

impl Entry {
    fn vidpid_pair(&self) -> (u16, u16) {
        let mut sp = self.vidpid.split(':');
        let v = sp
            .next()
            .and_then(|s| u16::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        let p = sp
            .next()
            .and_then(|s| u16::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        (v, p)
    }
}

fn nusb_by_vidpid() -> HashMap<(u16, u16), nusb::DeviceInfo> {
    nusb::list_devices()
        .wait()
        .map(|it| it.map(|d| ((d.vendor_id(), d.product_id()), d)).collect())
        .unwrap_or_default()
}

/// Enumerate USB devices directly through the cross-platform `nusb` crate.
/// Listing local hardware must not depend on the optional usbipd-win service
/// or CLI.
fn list_entries() -> Result<Vec<Entry>> {
    Ok(nusb_entries())
}

fn nusb_bus_numbers() -> HashMap<String, u8> {
    #[cfg(target_os = "windows")]
    {
        let mut ids: Vec<String> = nusb::list_buses()
            .wait()
            .map(|buses| buses.map(|bus| bus.bus_id().to_string()).collect())
            .unwrap_or_default();
        ids.sort();
        ids.dedup();
        return ids
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index as u8))
            .collect();
    }
    #[cfg(not(target_os = "windows"))]
    {
        HashMap::new()
    }
}

fn nusb_bus_number(device: &nusb::DeviceInfo, _windows_buses: &HashMap<String, u8>) -> u8 {
    #[cfg(target_os = "windows")]
    {
        return _windows_buses.get(device.bus_id()).copied().unwrap_or(0);
    }
    #[cfg(target_os = "macos")]
    {
        return u8::from_str_radix(device.bus_id(), 16).unwrap_or(0);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        device.bus_id().parse::<u8>().unwrap_or(0)
    }
}

fn nusb_busid(device: &nusb::DeviceInfo, windows_buses: &HashMap<String, u8>) -> String {
    let bus = nusb_bus_number(device, windows_buses);
    let ports = device.port_chain();
    if ports.is_empty() {
        format!("{bus}-{}", device.device_address())
    } else {
        let path = ports
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(".");
        format!("{bus}-{path}")
    }
}

/// Build listing rows straight from `nusb`, sorted by bus and physical port
/// chain for deterministic, topology-based output.
fn nusb_entries() -> Vec<Entry> {
    let bus_numbers = nusb_bus_numbers();
    let mut devs: Vec<nusb::DeviceInfo> = nusb::list_devices()
        .wait()
        .map(|it| it.collect())
        .unwrap_or_default();
    devs.sort_by_key(|d| (nusb_bus_number(d, &bus_numbers), d.port_chain().to_vec()));
    devs.iter()
        .map(|d| {
            let device = match (d.manufacturer_string(), d.product_string()) {
                (Some(m), Some(p)) => format!("{m} {p}"),
                (None, Some(p)) => p.to_string(),
                (Some(m), None) => m.to_string(),
                (None, None) => "(unknown device)".to_string(),
            };
            Entry {
                busid: nusb_busid(d, &bus_numbers),
                vidpid: format!("{:04x}:{:04x}", d.vendor_id(), d.product_id()),
                device,
            }
        })
        .collect()
}

#[derive(Default)]
struct WindowsUsbDriverNode {
    status: String,
    class: String,
    name: String,
    instance_id: String,
    service: String,
    problem: String,
    inf: String,
    provider: String,
    version: String,
}

impl WindowsUsbDriverNode {
    fn is_healthy(&self) -> bool {
        self.status.eq_ignore_ascii_case("OK")
            && (self.problem.is_empty() || self.problem == "0" || self.problem == "CM_PROB_NONE")
    }

    fn set_field(&mut self, key: &str, value: &str) {
        let target = match key {
            "STATUS" => &mut self.status,
            "CLASS" => &mut self.class,
            "NAME" => &mut self.name,
            "INSTANCE" => &mut self.instance_id,
            "SERVICE" => &mut self.service,
            "PROBLEM" => &mut self.problem,
            "INF" => &mut self.inf,
            "PROVIDER" => &mut self.provider,
            "VERSION" => &mut self.version,
            _ => return,
        };
        *target = value.trim().to_string();
    }
}

#[cfg(windows)]
fn windows_usb_driver_nodes() -> Result<Vec<WindowsUsbDriverNode>> {
    let script = r#"
$ErrorActionPreference='SilentlyContinue'
function Prop($d, $key) {
  $p = Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName $key
  if ($null -ne $p.Data) { return ($p.Data -join ',') }
  return ''
}
Get-PnpDevice -PresentOnly |
  Where-Object { $_.InstanceId -like 'USB\VID_*' } |
  Sort-Object InstanceId |
  ForEach-Object {
    Write-Output '@@NODE@@'
    Write-Output ('STATUS=' + $_.Status)
    Write-Output ('CLASS=' + $_.Class)
    Write-Output ('NAME=' + $_.FriendlyName)
    Write-Output ('INSTANCE=' + $_.InstanceId)
    Write-Output ('SERVICE=' + (Prop $_ 'DEVPKEY_Device_Service'))
    Write-Output ('PROBLEM=' + (Prop $_ 'DEVPKEY_Device_ProblemCode'))
    Write-Output ('INF=' + (Prop $_ 'DEVPKEY_Device_DriverInfPath'))
    Write-Output ('PROVIDER=' + (Prop $_ 'DEVPKEY_Device_DriverProvider'))
    Write-Output ('VERSION=' + (Prop $_ 'DEVPKEY_Device_DriverVersion'))
  }
"#;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .context("failed to query Windows Plug and Play devices")?;
    if !output.status.success() {
        anyhow::bail!(
            "Windows Plug and Play query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut nodes = Vec::new();
    let mut current: Option<WindowsUsbDriverNode> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim();
        if line == "@@NODE@@" {
            if let Some(node) = current.take() {
                nodes.push(node);
            }
            current = Some(WindowsUsbDriverNode::default());
        } else if let Some((key, value)) = line.split_once('=') {
            if let Some(node) = current.as_mut() {
                node.set_field(key, value);
            }
        }
    }
    if let Some(node) = current {
        nodes.push(node);
    }
    Ok(nodes)
}

#[cfg(not(windows))]
fn windows_usb_driver_nodes() -> Result<Vec<WindowsUsbDriverNode>> {
    Ok(Vec::new())
}

/// Recommended official driver for an unhealthy interface, plus — when this
/// repository ships the matching package — the relative path to the bundled
/// INF so `--driver-status` can report "available locally" instead of blindly
/// telling the user to download.
struct DriverAdvice {
    name: &'static str,
    install: &'static str,
    /// Relative path (from the repo root / exe dir) to a bundled INF, if any.
    local_inf: Option<&'static str>,
}

fn known_driver_advice(instance_id: &str) -> Option<DriverAdvice> {
    let id = instance_id.to_ascii_uppercase();
    if id.contains("VID_0483&PID_374B&MI_00")
        || id.contains("VID_0483&PID_374A&MI_00")
        || id.contains("VID_0483&PID_374E&MI_00")
        || id.contains("VID_0483&PID_374F&MI_00")
    {
        return Some(DriverAdvice {
            name: "STMicroelectronics STSW-LINK009 (WinUSB binding for ST-Link Debug)",
            install: r#"pnputil /add-driver ".\windows-driver\stsw-link009\stlink_dbg_winusb.inf" /install"#,
            local_inf: Some("windows-driver/stsw-link009/stlink_dbg_winusb.inf"),
        });
    }
    if id.contains("VID_0483&PID_DF11") {
        return Some(DriverAdvice {
            name: "STM32CubeProgrammer driver package or WinUSB",
            install: "Install STM32CubeProgrammer, or bind this DFU interface to WinUSB with Zadig.",
            local_inf: None,
        });
    }
    if id.contains("VID_10C4&PID_EA") {
        return Some(DriverAdvice {
            name: "Silicon Labs CP210x Universal Windows Driver",
            install: "Download from https://www.silabs.com/developers/usb-to-uart-bridge-vcp-drivers",
            local_inf: None,
        });
    }
    if id.contains("VID_1A86&") {
        return Some(DriverAdvice {
            name: "WCH CH34x/CH91xx Windows driver",
            install: "Download from https://www.wch-ic.com/downloads/CH341SER_EXE.html",
            local_inf: None,
        });
    }
    if id.contains("VID_0403&") {
        return Some(DriverAdvice {
            name: "FTDI CDM/VCP driver",
            install: "Download from https://ftdichip.com/drivers/vcp-drivers/",
            local_inf: None,
        });
    }
    if id.contains("VID_2E8A&PID_0003") || id.contains("VID_2E8A&PID_000F") {
        return Some(DriverAdvice {
            name: "Microsoft WinUSB for Raspberry Pi BOOTSEL",
            install: "Use Zadig to bind only the RP2 BOOTSEL interface to WinUSB.",
            local_inf: None,
        });
    }
    None
}

/// Print whether the recommended driver is already on hand — bundled in this
/// repo and/or staged in the Windows driver store — so the user knows they can
/// install offline instead of downloading.
fn report_local_driver_availability(advice: &DriverAdvice) {
    let Some(rel) = advice.local_inf else { return };
    match find_local_inf(rel) {
        Some(path) => println!("  Local INF: AVAILABLE - {} (no download needed)", path.display()),
        None => println!("  Local INF: not bundled at {rel}; download required"),
    }
    #[cfg(windows)]
    if let Some(original) = Path::new(rel).file_name().and_then(|n| n.to_str()) {
        let staged = driverstore_matches(original);
        if staged.is_empty() {
            println!("  Driver store: no package staged from {original} yet");
        } else {
            println!("  Driver store: already staged as {}", staged.join(", "));
        }
    }
}

/// Look for a bundled INF on disk: first relative to the current working
/// directory (running from the repo), then next to the executable (running an
/// installed/copied binary). Returns the first path that exists.
fn find_local_inf(rel: &str) -> Option<PathBuf> {
    let cwd = PathBuf::from(rel);
    if cwd.is_file() {
        return Some(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Published `oemNN.inf` names in the Windows driver store whose package was
/// staged from `original_inf` (e.g. `stlink_dbg_winusb.inf`). Parses
/// `pnputil /enum-drivers` locale-independently: it splits the output into
/// per-driver blocks and, for any block mentioning the original file name,
/// extracts the `oemNN.inf` token (which Windows assigns regardless of UI
/// language). An empty result means the package is not yet staged.
#[cfg(windows)]
fn driverstore_matches(original_inf: &str) -> Vec<String> {
    let output = match Command::new("pnputil").args(["/enum-drivers"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    parse_driverstore_oem(&String::from_utf8_lossy(&output.stdout), original_inf)
}

/// Pure parser for `pnputil /enum-drivers` output: returns the `oemNN.inf`
/// published names of every driver block that mentions `original_inf`. Split
/// out from `driverstore_matches` so it is testable on any OS and independent
/// of the locale-specific field labels Windows prints.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_driverstore_oem(text: &str, original_inf: &str) -> Vec<String> {
    let target = original_inf.to_ascii_lowercase();
    let mut matches = Vec::new();
    for block in text.split("\r\n\r\n").flat_map(|b| b.split("\n\n")) {
        if !block.to_ascii_lowercase().contains(&target) {
            continue;
        }
        if let Some(oem) = block.split_whitespace().find(|token| {
            let t = token.trim_end_matches([',', ';']).to_ascii_lowercase();
            t.starts_with("oem") && t.ends_with(".inf")
        }) {
            let oem = oem.trim_end_matches([',', ';']).to_string();
            if !matches.contains(&oem) {
                matches.push(oem);
            }
        }
    }
    matches
}

fn instance_vidpid(instance_id: &str) -> Option<(u16, u16)> {
    let id = instance_id.to_ascii_uppercase();
    let vid = id.split("VID_").nth(1)?.get(..4)?;
    let pid = id.split("PID_").nth(1)?.get(..4)?;
    Some((
        u16::from_str_radix(vid, 16).ok()?,
        u16::from_str_radix(pid, 16).ok()?,
    ))
}

fn cmd_driver_status() -> Result<()> {
    #[cfg(not(windows))]
    {
        println!("--driver-status currently reports Windows Plug and Play driver bindings only.");
        return Ok(());
    }

    #[cfg(windows)]
    {
        let devices: Vec<nusb::DeviceInfo> = nusb::list_devices()
            .wait()
            .context("failed to enumerate USB hardware through nusb")?
            .collect();
        let nodes = windows_usb_driver_nodes()?;

        println!("=== USB Hardware Evidence ===");
        println!("PASS - Windows USB hub enumeration returned {} physical USB device(s).", devices.len());
        println!("This proves USB signaling/enumeration works; it does not prove every function driver works.");

        println!("\n=== Windows USB Interface Drivers ===");
        let mut failures = 0usize;
        for node in &nodes {
            let health = if node.is_healthy() { "OK" } else { failures += 1; "NEEDS ATTENTION" };
            println!("\n[{health}] {}", if node.name.is_empty() { "(unnamed USB interface)" } else { &node.name });
            println!("  Instance: {}", node.instance_id);
            println!("  Class/service: {}/{}", value_or_dash(&node.class), value_or_dash(&node.service));
            println!("  Driver: {} / {} / {}", value_or_dash(&node.provider), value_or_dash(&node.inf), value_or_dash(&node.version));
            println!("  PnP status/problem: {} / {}", value_or_dash(&node.status), value_or_dash(&node.problem));
            if !node.is_healthy() {
                if let Some(issue) = classify_driver_issue(node) {
                    println!("  Classification: {} - {}", issue.label(), issue.meaning());
                    println!("  Safe next action: {}", issue.next_action());
                }
                if let Some((vid, pid)) = instance_vidpid(&node.instance_id) {
                    if let Some(device) = devices
                        .iter()
                        .find(|device| device.vendor_id() == vid && device.product_id() == pid)
                    {
                        println!(
                            "  Hardware evidence: PASS - nusb sees {:04x}:{:04x}, product {:?}, serial {:?}, speed {:?}",
                            vid,
                            pid,
                            device.product_string(),
                            device.serial_number(),
                            device.speed()
                        );
                        let interfaces = device
                            .interfaces()
                            .map(|interface| {
                                format!(
                                    "MI_{:02} class {:02x}",
                                    interface.interface_number(),
                                    interface.class()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("  Descriptor interfaces: {interfaces}");
                    }
                }
                if let Some(advice) = known_driver_advice(&node.instance_id) {
                    println!("  Known official driver: {}", advice.name);
                    println!("  Install: {}", advice.install);
                    report_local_driver_availability(&advice);
                } else {
                    println!("  Next step: search the exact VID:PID on the hardware vendor's support site or Microsoft Update Catalog.");
                }
            }
        }

        println!("\n=== Diagnosis ===");
        if failures == 0 {
            println!("All {} present USB PnP function(s) report healthy.", nodes.len());
        } else {
            println!("{failures} of {} present USB PnP function(s) need attention.", nodes.len());
            println!("A device listed under Hardware Evidence but failing here is primarily a Windows driver/binding problem, not proof of faulty hardware.");
            println!("This still cannot prove that every downstream MCU, sensor, or external circuit behind the USB controller is healthy.");
        }
        Ok(())
    }
}

/// `--install-driver`: dry-run by default, executing only with `--confirm`.
/// Restricted to the ST-Link `MI_00` debug interface (the only binding this
/// repo ships an INF for) so it can never touch the healthy Mass Storage /
/// COM / composite functions of the same device.
fn cmd_install_driver(confirm: bool) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = confirm;
        println!("--install-driver binds Windows USB function drivers and runs on Windows only.");
        return Ok(());
    }

    #[cfg(windows)]
    {
        println!("=== ST-Link MI_00 Driver Install ({}) ===", if confirm { "EXECUTE" } else { "DRY RUN" });
        println!("Scope: only the ST-Link Debug MI_00 interface is touched; Mass Storage, COM, and the composite parent are left alone.\n");

        let nodes = windows_usb_driver_nodes()?;
        let candidates: Vec<&WindowsUsbDriverNode> = nodes
            .iter()
            .filter(|n| n.instance_id.to_ascii_uppercase().contains("&MI_00"))
            .filter(|n| !n.is_healthy())
            .filter(|n| known_driver_advice(&n.instance_id).is_some_and(|a| a.local_inf.is_some()))
            .collect();

        if candidates.is_empty() {
            println!("No unhealthy MI_00 interface with a bundled INF was found.");
            println!("Run `usbipd-rs --driver-status` to inspect current bindings.");
            return Ok(());
        }

        let mut executed = 0usize;
        for node in candidates {
            let advice = known_driver_advice(&node.instance_id).expect("filtered to Some above");
            let rel = advice.local_inf.expect("filtered to Some above");
            let inf = match find_local_inf(rel) {
                Some(path) => path,
                None => {
                    println!("[SKIP] {}", node.instance_id);
                    println!("  Bundled INF missing at {rel}; cannot install offline.");
                    continue;
                }
            };

            println!("Interface that would be modified:");
            println!("  Instance: {}", node.instance_id);
            if let Some(issue) = classify_driver_issue(node) {
                println!("  Current state: {} (status {}, problem {})", issue.label(), value_or_dash(&node.status), value_or_dash(&node.problem));
            }
            println!("  Driver package: {}", advice.name);
            println!("  INF file: {}", inf.display());
            let inf_arg = inf.to_string_lossy();
            println!("  Command: pnputil /add-driver \"{inf_arg}\" /install");

            if !confirm {
                println!("  Action: none (dry run). Re-run with --confirm to apply.\n");
                continue;
            }

            println!("  Action: running pnputil ...");
            let output = Command::new("pnputil")
                .args(["/add-driver", inf_arg.as_ref(), "/install"])
                .output()
                .context("failed to launch pnputil (driver install needs an elevated/admin shell)")?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stdout.lines().chain(stderr.lines()).map(str::trim).filter(|l| !l.is_empty()) {
                println!("    {line}");
            }
            if !output.status.success() {
                println!("  Result: FAIL - pnputil exited with {}. An Administrator shell is required.\n", output.status);
                continue;
            }
            executed += 1;

            // Recovery check: re-query the same instance and confirm it is now
            // healthy *without* a reboot.
            let after = windows_usb_driver_nodes()?;
            match after.iter().find(|n| n.instance_id == node.instance_id) {
                Some(n) if n.is_healthy() => {
                    println!("  Result: PASS - {} is now {} / problem {} (recovered, no reboot).", n.instance_id, value_or_dash(&n.status), value_or_dash(&n.problem));
                }
                Some(n) => {
                    println!("  Result: PARTIAL - still {} / problem {}. Unplug and replug the device, or check it reports NEED_RESTART.", value_or_dash(&n.status), value_or_dash(&n.problem));
                }
                None => println!("  Result: node re-enumerated under a new instance id; re-run --driver-status to confirm."),
            }
            println!();
        }

        if confirm {
            println!("Installed {executed} driver binding(s). Verify with `usbipd-rs --driver-status` and `usbipd-rs --mcu-alive`.");
        } else {
            println!("Dry run complete. No changes were made. Re-run with --confirm in an Administrator shell to apply.");
        }
        Ok(())
    }
}

fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

/// A coarse, actionable category for *why* a present USB interface is
/// unhealthy, derived from its Windows PnP problem code, bound service, and
/// status. The point is to separate "Windows never bound a driver" from
/// "a driver is bound but broken" so the next step is unambiguous.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DriverIssue {
    MissingDriver,
    DriverLoadFailure,
    StoppedNode,
    SignatureFailure,
    Blocked,
    ResourceConflict,
    Unknown,
}

impl DriverIssue {
    fn label(self) -> &'static str {
        match self {
            DriverIssue::MissingDriver => "MISSING FUNCTION DRIVER",
            DriverIssue::DriverLoadFailure => "DRIVER FAILED TO LOAD",
            DriverIssue::StoppedNode => "DEVICE NODE STOPPED/DISABLED",
            DriverIssue::SignatureFailure => "DRIVER SIGNATURE REJECTED",
            DriverIssue::Blocked => "DRIVER BLOCKED",
            DriverIssue::ResourceConflict => "RESOURCE CONFLICT",
            DriverIssue::Unknown => "UNCLASSIFIED PROBLEM",
        }
    }

    fn meaning(self) -> &'static str {
        match self {
            DriverIssue::MissingDriver => {
                "Windows enumerated the interface but bound no function driver (no service)."
            }
            DriverIssue::DriverLoadFailure => {
                "A driver is assigned but its service could not start: stale binding, failed driver entry, or a prior unload."
            }
            DriverIssue::StoppedNode => {
                "The node is disabled, not started, or waiting for a restart."
            }
            DriverIssue::SignatureFailure => {
                "The matched driver's digital signature could not be verified."
            }
            DriverIssue::Blocked => {
                "Windows blocked this driver under a known-bad list or security policy."
            }
            DriverIssue::ResourceConflict => {
                "The device reports an I/O, memory, or IRQ resource conflict."
            }
            DriverIssue::Unknown => "The problem code does not map to a known category.",
        }
    }

    fn next_action(self) -> &'static str {
        match self {
            DriverIssue::MissingDriver => {
                "Bind the correct function driver (see below). This is a driver gap, not a hardware fault."
            }
            DriverIssue::DriverLoadFailure => {
                "Remove the stale binding (pnputil /delete-driver) then reinstall the correct INF."
            }
            DriverIssue::StoppedNode => {
                "Enable/restart the node in Device Manager; reboot only if it reports NEED_RESTART."
            }
            DriverIssue::SignatureFailure => {
                "Install a properly signed driver package from the vendor."
            }
            DriverIssue::Blocked => {
                "Do not force-load. Obtain an updated, unblocked driver from the vendor."
            }
            DriverIssue::ResourceConflict => {
                "Move the device to another port/hub and check for a conflicting device."
            }
            DriverIssue::Unknown => {
                "Inspect the raw problem code for this instance in Device Manager."
            }
        }
    }
}

/// Map an unhealthy node to a `DriverIssue`. Returns `None` when the node is
/// healthy. The numeric values are Windows `CM_PROB_*` configuration-manager
/// problem codes reported via `DEVPKEY_Device_ProblemCode`.
fn classify_driver_issue(node: &WindowsUsbDriverNode) -> Option<DriverIssue> {
    if node.is_healthy() {
        return None;
    }
    let no_service = node.service.trim().is_empty();
    let code: u32 = node.problem.trim().parse().unwrap_or(0);
    let issue = match code {
        // CM_PROB_NOT_CONFIGURED / CM_PROB_FAILED_INSTALL: no driver vs broken.
        1 | 28 => {
            if no_service {
                DriverIssue::MissingDriver
            } else {
                DriverIssue::DriverLoadFailure
            }
        }
        // Disabled, not started, needs restart, phantom, etc.
        10 | 14 | 19 | 21 | 22 | 24 | 45 => DriverIssue::StoppedNode,
        // Stale binding, failed driver entry, failed/aborted load.
        31 | 37 | 38 | 39 => DriverIssue::DriverLoadFailure,
        // CM_PROB_UNSIGNED_DRIVER.
        52 => DriverIssue::SignatureFailure,
        // Blocked driver / boot-time blocked.
        43 | 44 | 48 => DriverIssue::Blocked,
        // CM_PROB_NORMAL_CONFLICT.
        12 => DriverIssue::ResourceConflict,
        // Status not OK but no code: empty service ⇒ missing, else unknown.
        0 if no_service => DriverIssue::MissingDriver,
        _ => DriverIssue::Unknown,
    };
    Some(issue)
}

fn is_stlink_device(device: &nusb::DeviceInfo) -> bool {
    device.vendor_id() == 0x0483 && (0x3748..=0x3757).contains(&device.product_id())
}

fn cmd_mcu_alive() -> Result<()> {
    let probes: Vec<nusb::DeviceInfo> = nusb::list_devices()
        .wait()
        .context("failed to enumerate USB hardware through nusb")?
        .filter(is_stlink_device)
        .collect();

    println!("=== Minimal MCU Alive Test ===");
    println!("Safety: no target reset, halt, flash read/write, erase, or unlock is requested.");
    println!("The SWD discovery scan may reset the SWD debug link state.");

    if probes.is_empty() {
        println!("Step 1 - USB probe: FAIL - no ST-Link USB device found");
        println!("Step 2 - Debug interface: SKIP");
        println!("Step 3 - Target SWD response: SKIP");
        return Ok(());
    }

    for probe in probes {
        let vid = probe.vendor_id();
        let pid = probe.product_id();
        let serial = probe.serial_number().unwrap_or("");
        let selector = if serial.is_empty() {
            format!("{vid:04x}:{pid:04x}")
        } else {
            format!("{vid:04x}:{pid:04x}:{serial}")
        };

        println!("\n[{selector}]");
        println!(
            "Step 1 - USB probe: PASS - product {:?}, serial {:?}, speed {:?}",
            probe.product_string(),
            probe.serial_number(),
            probe.speed()
        );

        if let Some(StlinkDriver::Other(state)) = stlink_driver_check(pid) {
            println!("Step 2 - Debug interface: FAIL - {state}");
            println!("Step 3 - Target SWD response: SKIP - no host-to-probe command path");
            if let Some(advice) =
                known_driver_advice(&format!("USB\\VID_{vid:04X}&PID_{pid:04X}&MI_00"))
            {
                println!("Required driver binding: {}", advice.name);
                println!("Install: {}", advice.install);
                report_local_driver_availability(&advice);
            }
            continue;
        }

        println!("Step 2 - Debug interface: attempting read-only open through probe-rs");
        let output = Command::new("probe-rs")
            .args([
                "info",
                "--probe",
                &selector,
                "--protocol",
                "swd",
                "--speed",
                "100",
                "--non-interactive",
            ])
            .output()
            .context("probe-rs not found on PATH (install with: cargo install probe-rs-tools)")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            println!("Step 2 - Debug interface: PASS - ST-Link accepted commands");
            println!("Step 3 - Target SWD response: PASS - probe-rs discovered a target at 100 kHz");
            for line in stdout
                .lines()
                .chain(stderr.lines())
                .map(str::trim)
                .filter(|line| {
                    !line.is_empty()
                        && !line.starts_with('-')
                        && !line.eq_ignore_ascii_case("Probing target via SWD")
                })
                .take(12)
            {
                println!("  {line}");
            }
        } else {
            let error = stderr
                .lines()
                .chain(stdout.lines())
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('-'))
                .last()
                .unwrap_or("probe-rs could not complete SWD discovery");
            let opened = !error.to_ascii_lowercase().contains("driver")
                && !error.to_ascii_lowercase().contains("open the debug probe")
                && !error.to_ascii_lowercase().contains("usb error");
            println!(
                "Step 2 - Debug interface: {}",
                if opened {
                    "PASS - probe opened; SWD stage failed"
                } else {
                    "FAIL - probe could not be opened"
                }
            );
            println!("Step 3 - Target SWD response: FAIL - {error}");
        }
    }
    Ok(())
}

// ============================================================================
// Native ST-Link SWD reader (--mcu-alive-native)
//
// Speaks the ST-Link bulk command protocol directly over `nusb`, so no
// external tool (probe-rs / pyocd / stlink) is needed. The only thing it
// requires on Windows is a WinUSB binding on the MI_00 vendor interface — the
// irreducible minimum, since user-mode cannot issue bulk transfers to a
// driverless interface (see `--install-driver`). On Linux/macOS nusb's libusb
// / IOKit backend can claim the interface without a manual driver step.
//
// Command/response layout cross-checked against probe-rs, stlink-org/stlink,
// and OpenOCD. The sequence is read-only: it enters SWD and reads ID registers
// only — no halt, reset, erase, or memory write is ever issued.
// ============================================================================

const STLINK_CMD_SIZE: usize = 16;
const STLINK_GET_VERSION: u8 = 0xF1;
const STLINK_DFU_COMMAND: u8 = 0xF3;
const STLINK_DFU_EXIT: u8 = 0x07;
const STLINK_SWIM_COMMAND: u8 = 0xF4;
const STLINK_SWIM_EXIT: u8 = 0x01;
const STLINK_GET_CURRENT_MODE: u8 = 0xF5;
const STLINK_GET_TARGET_VOLTAGE: u8 = 0xF7;
const STLINK_DEBUG_COMMAND: u8 = 0xF2;
const STLINK_DEBUG_APIV2_ENTER: u8 = 0x30;
const STLINK_DEBUG_EXIT: u8 = 0x21;
const STLINK_DEBUG_ENTER_SWD: u8 = 0xA3;
const STLINK_DEBUG_APIV2_READ_IDCODES: u8 = 0x31;
const STLINK_DEBUG_APIV2_READDEBUGREG: u8 = 0x36;
const STLINK_JTAG_OK: u8 = 0x80;

fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// An opened ST-Link debug interface. Owns the device/interface handles so the
/// endpoints stay valid for the link's lifetime.
struct StlinkLink {
    _device: nusb::Device,
    _iface: nusb::Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    in_max: usize,
}

impl StlinkLink {
    /// Open a probe end to end: open the USB device, claim its `MI_00`
    /// vendor/debug interface, open the bulk endpoints, and resync the pipes.
    /// Returns an error whose message names the failing stage — notably
    /// `claim_interface` failing is the Windows "no WinUSB on MI_00" boundary.
    fn open_device(probe: &nusb::DeviceInfo) -> Result<Self> {
        let device = probe.open().wait().context("open USB device")?;
        let iface_num = probe
            .interfaces()
            .find(|i| i.class() == 0xff)
            .map(|i| i.interface_number())
            .unwrap_or(0);
        let iface = device
            .claim_interface(iface_num)
            .wait()
            .with_context(|| format!("claim MI_{iface_num:02} (needs WinUSB on MI_00)"))?;
        // RX is 0x81 on every ST-Link; TX is 0x02 on the original V2, else 0x01.
        let ep_out_addr = if probe.product_id() == 0x3748 { 0x02 } else { 0x01 };
        let out = iface
            .endpoint::<Bulk, Out>(ep_out_addr)
            .map_err(|e| anyhow::anyhow!("open OUT endpoint 0x{ep_out_addr:02x}: {e}"))?;
        let mut inp = iface
            .endpoint::<Bulk, In>(0x81)
            .map_err(|e| anyhow::anyhow!("open IN endpoint 0x81: {e}"))?;
        let in_max = inp.max_packet_size().max(1);
        // Reset both pipes' halt/toggle state and drain any stale response left
        // by a previously interrupted command, so the firmware command FSM
        // starts in sync. Best-effort: ignore errors on a clean device.
        let mut out = out;
        let _ = out.clear_halt().wait();
        let _ = inp.clear_halt().wait();
        let mut link = Self { _device: device, _iface: iface, ep_out: out, ep_in: inp, in_max };
        link.drain_stale();
        Ok(link)
    }

    /// Best-effort flush of any response the probe is still waiting to send
    /// (from an earlier aborted command). Short timeout; stops at the first
    /// empty/failed read so a clean device costs only one quick poll.
    fn drain_stale(&mut self) {
        let timeout = std::time::Duration::from_millis(60);
        for _ in 0..3 {
            let completion = self.ep_in.transfer_blocking(Buffer::new(self.in_max), timeout);
            match completion.status {
                Ok(()) if !completion.buffer.is_empty() => continue,
                _ => break,
            }
        }
    }

    /// Send a 16-byte (zero-padded) command, then read up to `read_len` bytes.
    ///
    /// The bulk IN request length is rounded up to a multiple of the endpoint's
    /// max packet size: WinUSB rejects a non-multiple read with
    /// ERROR_INVALID_PARAMETER ("invalid or unsupported argument"). The device
    /// returns a short packet, so the actual response may be shorter.
    fn cmd(&mut self, bytes: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let timeout = std::time::Duration::from_millis(1000);
        let mut out = vec![0u8; STLINK_CMD_SIZE];
        out[..bytes.len()].copy_from_slice(bytes);
        if let Err(e) = self
            .ep_out
            .transfer_blocking(Buffer::from(out), timeout)
            .into_result()
        {
            self.recover_from(e);
            return Err(anyhow::anyhow!("bulk OUT failed: {e}"));
        }
        if read_len == 0 {
            return Ok(Vec::new());
        }
        let request = read_len.div_ceil(self.in_max) * self.in_max;
        match self
            .ep_in
            .transfer_blocking(Buffer::new(request), timeout)
            .into_result()
        {
            Ok(resp) => Ok(resp.into_vec()),
            Err(e) => {
                self.recover_from(e);
                Err(anyhow::anyhow!("bulk IN failed: {e}"))
            }
        }
    }

    /// Recover the pipes after a failed transfer. A STALL (the probe rejecting an
    /// unsupported command, e.g. some ST-Link/V2 clones STALL GET_TARGET_VOLTAGE)
    /// halts the endpoint; without a CLEAR_FEATURE every later command also fails
    /// (the OUT side then just times out). Clearing both halts lets the next
    /// command — e.g. ENTER_SWD — proceed.
    fn recover_from(&mut self, err: TransferError) {
        if matches!(err, TransferError::Stall) {
            let _ = self.ep_out.clear_halt().wait();
            let _ = self.ep_in.clear_halt().wait();
        }
    }

    /// Probe firmware version (read-only, valid in any mode).
    fn get_version(&mut self) -> Result<String> {
        let r = self.cmd(&[STLINK_GET_VERSION], 6)?;
        if r.len() < 2 {
            anyhow::bail!("short version response ({} bytes)", r.len());
        }
        let v = ((r[0] as u16) << 8) | r[1] as u16;
        Ok(format!(
            "ST-Link v{} JTAG/SWD v{} SWIM v{}",
            (v >> 12) & 0x0F,
            (v >> 6) & 0x3F,
            v & 0x3F
        ))
    }

    /// Target reference voltage in millivolts (read-only ADC sample).
    fn get_voltage_mv(&mut self) -> Result<u32> {
        let r = self.cmd(&[STLINK_GET_TARGET_VOLTAGE], 8)?;
        let factor = le_u32(&r, 0).context("short voltage response")?;
        let reading = le_u32(&r, 4).context("short voltage response")?;
        if factor == 0 {
            anyhow::bail!("voltage divisor is zero (probe not ready)");
        }
        Ok((2400u64 * reading as u64 / factor as u64) as u32)
    }

    /// Current probe mode (0=DFU, 1=mass storage, 2=debug/JTAG, 3=SWIM).
    fn current_mode(&mut self) -> u8 {
        self.cmd(&[STLINK_GET_CURRENT_MODE], 2)
            .ok()
            .and_then(|r| r.first().copied())
            .unwrap_or(0xFF)
    }

    /// Leave whatever mode the probe powered up in, so a fresh ENTER_SWD is
    /// accepted. A bare ST-Link/V2 often enumerates in DFU mode; entering SWD
    /// without leaving it first silently fails (status 0x00). Best-effort.
    fn leave_current_mode(&mut self) {
        let _ = match self.current_mode() {
            0 => self.cmd(&[STLINK_DFU_COMMAND, STLINK_DFU_EXIT], 0),
            2 => self.cmd(&[STLINK_DEBUG_COMMAND, STLINK_DEBUG_EXIT], 0),
            3 => self.cmd(&[STLINK_SWIM_COMMAND, STLINK_SWIM_EXIT], 0),
            _ => Ok(Vec::new()),
        };
    }

    /// Enter SWD mode. Returns the probe status byte (0x80 = OK). Leaves the
    /// power-up mode first. This does not halt or reset the core — it only
    /// initializes the debug link.
    fn enter_swd(&mut self) -> Result<u8> {
        self.leave_current_mode();
        let r = self.cmd(
            &[STLINK_DEBUG_COMMAND, STLINK_DEBUG_APIV2_ENTER, STLINK_DEBUG_ENTER_SWD],
            2,
        )?;
        Ok(r.first().copied().unwrap_or(0))
    }

    /// Read the DP IDCODE (DPIDR) — the first read-only target ID.
    fn read_idcode(&mut self) -> Result<u32> {
        let r = self.cmd(&[STLINK_DEBUG_COMMAND, STLINK_DEBUG_APIV2_READ_IDCODES], 12)?;
        le_u32(&r, 4).context("short idcode response")
    }

    /// Read a 32-bit debug/AP register at `addr` (read-only). The ST-Link
    /// firmware drives the AHB-AP for this command, so no AP setup is needed.
    fn read_debug_reg(&mut self, addr: u32) -> Result<u32> {
        let a = addr.to_le_bytes();
        let r = self.cmd(
            &[STLINK_DEBUG_COMMAND, STLINK_DEBUG_APIV2_READDEBUGREG, a[0], a[1], a[2], a[3]],
            8,
        )?;
        if r.first().copied().unwrap_or(0) != STLINK_JTAG_OK {
            anyhow::bail!("debug-reg read 0x{addr:08X} status 0x{:02X}", r.first().copied().unwrap_or(0));
        }
        le_u32(&r, 4).context("short debug-reg response")
    }
}

/// After ENTER_SWD, read the read-only identity registers into a map keyed by
/// address (the shape `decode_stlink_regs` consumes), and resolve the chip
/// (a matching `.chip` file in `db` wins over the built-in table). Returns
/// `(regs, dev_id, resolved)`.
fn stlink_read_regs(
    link: &mut StlinkLink,
    db: &[ChipDef],
) -> (HashMap<u32, Vec<u32>>, Option<u16>, Option<ResolvedChip>) {
    collect_target_regs(|addr| link.read_debug_reg(addr), db)
}

/// Read the read-only identity registers using a generic 32-bit reader closure
/// (ST-Link READDEBUGREG or CMSIS-DAP MEM-AP), resolve the chip (a matching
/// `.chip` file wins), and return `(regs, dev_id, resolved)`.
fn collect_target_regs(
    mut read: impl FnMut(u32) -> Result<u32>,
    db: &[ChipDef],
) -> (HashMap<u32, Vec<u32>>, Option<u16>, Option<ResolvedChip>) {
    let mut regs: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(cpuid) = read(0xE000ED00) {
        regs.insert(0xE000ED00, vec![cpuid]);
    }
    // DBGMCU_IDCODE: 0xE0042000 on Cortex-M3/M4/M7 STM32, 0x40015800 on M0.
    let mut idcode = None;
    for &addr in &[0xE0042000u32, 0x40015800u32] {
        if let Ok(v) = read(addr) {
            if (v & 0xFFF) != 0 && (v & 0xFFF) != 0xFFF {
                regs.insert(0xE0042000, vec![v]);
                idcode = Some(v);
                break;
            }
        }
    }
    let dev_id = idcode.map(|v| (v & 0xFFF) as u16);
    let resolved = dev_id.and_then(|d| resolve_chip(d, db));
    if let Some(r) = resolved.as_ref() {
        // Flash size is a 16-bit field that may be non-word-aligned (0x1FFF7A22
        // on F4/F7); 32-bit reads are word-aligned, so read the containing word
        // and keep the correct half in the low 16 bits.
        if let Some(fsa) = r.flash_size_addr {
            if let Ok(word) = read(fsa & !0x3) {
                regs.insert(fsa, vec![(word >> ((fsa & 0x3) * 8)) & 0xFFFF]);
            }
        }
        if let Some(uid_addr) = r.uid_addr {
            let mut uid = Vec::new();
            for k in 0..3 {
                match read(uid_addr + k * 4) {
                    Ok(v) => uid.push(v),
                    Err(_) => break,
                }
            }
            if uid.len() == 3 {
                regs.insert(uid_addr, uid);
            }
        }
        if let Some(rdp_addr) = r.rdp_addr {
            if let Ok(v) = read(rdp_addr) {
                regs.insert(rdp_addr, vec![v]);
            }
        }
    }
    (regs, dev_id, resolved)
}

/// Build ordered (key, value) identity rows from decoded registers, applying
/// `.chip` overrides (name, SRAM, source) and a flash fallback for parts the
/// built-in family table doesn't cover.
fn format_target_rows(
    regs: &HashMap<u32, Vec<u32>>,
    dev_id: Option<u16>,
    resolved: Option<&ResolvedChip>,
) -> Vec<(&'static str, String)> {
    let mut map = decode_stlink_regs(regs, dev_id);
    if let Some(r) = resolved {
        if let Some(did) = dev_id {
            // `decode_stlink_regs` already named GD32 clones (which mirror this
            // DEV_ID) as GigaDevice parts — don't clobber that with the generic
            // ST family name from the built-in table / .chip override.
            let decoded_is_gd32 = map.get("Device ID").is_some_and(|d| d.contains("GD32"));
            if !decoded_is_gd32 {
                map.insert("Device ID".into(), format!("0x{:03X} — {}", did, r.name));
            }
        }
        if let Some(kb) = r.sram_kb {
            map.insert("SRAM".into(), format!("{kb} KB"));
        }
        // Flash + RDP fallback for a model the built-in table doesn't cover
        // (decode only reads those for built-in families).
        if !map.contains_key("Flash size") {
            if let Some(kb) = r.flash_size_addr.and_then(|a| regs.get(&a)).and_then(|w| w.first()).copied() {
                if kb != 0 && kb != 0xFFFF {
                    map.insert("Flash size".into(), format!("{kb} KB"));
                    map.insert("Flash map".into(), format!("0x08000000-0x{:08X}", 0x08000000u32 + kb * 1024 - 1));
                }
            }
        }
        if !map.contains_key("Read protection") {
            if let (Some(addr), Some(kind)) = (r.rdp_addr, r.rdp_kind) {
                if let Some(opt) = regs.get(&addr).and_then(|w| w.first()).copied() {
                    map.insert("Read protection".into(), decode_rdp(opt, kind));
                }
            }
        }
        map.insert("Source".into(), r.source.clone());
    }
    // The native path doesn't set a fixed SWD clock; correct decode's default.
    if map.contains_key("Transport") {
        map.insert("Transport".into(), "SWD (native nusb, read-only)".into());
    }
    [
        "Device ID", "Revision", "Vendor", "Core", "Architecture", "Max clock", "Flash size",
        "Flash map", "SRAM", "Unique ID", "Read protection", "Transport", "Access", "Source",
    ]
    .iter()
    .filter_map(|k| map.get(*k).map(|v| (*k, v.clone())))
    .collect()
}

/// Actionable checklist when SWD enters but no target answers (DPIDR fails).
/// Reads the probe's sense of target voltage — an unreadable / 0 V reading is a
/// strong sign the target isn't powered or VTREF isn't wired.
fn swd_no_target_hint(link: &mut StlinkLink) -> String {
    let volt = link
        .get_voltage_mv()
        .map(|mv| format!("{:.2} V", mv as f64 / 1000.0))
        .unwrap_or_else(|_| "unreadable".into());
    format!(
        "probe-sensed target voltage: {volt}. Check: target is powered; ST-Link VTREF(3V3)/SWDIO/SWCLK/GND all wired; \
         NRST not held low; using the SWD (not JTAG) header. On a Nucleo driven by an EXTERNAL ST-Link, remove the two \
         CN2 (ST-LINK) jumpers to disconnect the on-board ST-Link, and power the board (USB or E5V)."
    )
}

// ============================================================================
// Native CMSIS-DAP v2 reader (RP2040 Debug Probe / picoprobe — layer 2)
//
// Speaks the CMSIS-DAP v2 bulk protocol directly over nusb to read the
// downstream SWD target's identity read-only (DPIDR, then CPUID / DBGMCU /
// flash / UID / RDP via the MEM-AP). No probe-rs/pyocd. Brings up SWD and reads
// ID registers only — no halt, reset, erase, or write. The DAP_Transfer request
// byte is APnDP(bit0) | RnW(bit1) | (regaddr & 0x0C); reads pull the result on
// the next transfer (DRW then RDBUFF), per the SWD posted-read pipeline.
// ============================================================================

const DAP_CONNECT: u8 = 0x02;
const DAP_TRANSFER_CONFIGURE: u8 = 0x04;
const DAP_TRANSFER: u8 = 0x05;
const DAP_SWJ_CLOCK: u8 = 0x11;
const DAP_SWJ_SEQUENCE: u8 = 0x12;
const DAP_SWD_CONFIGURE: u8 = 0x13;

// RP2040 is a SWD multidrop (DPv2) target with two cores. TARGETSEL for core 0;
// SYSINFO CHIP_ID/GITREF identify the chip + stepping over the MEM-AP.
const RP2040_TARGETSEL_CORE0: u32 = 0x01002927;
const RP2040_TARGETSEL_CORE1: u32 = 0x11002927;
const RP2040_SYSINFO_CHIP_ID: u32 = 0x40000000;
const RP2040_SYSINFO_GITREF: u32 = 0x40000014;
// The RP2040's fixed DPIDR (DPv2, designer 0x927, partno 0xC1). Used to accept a
// multidrop bring-up by exact match — see `bringup()` for why a version-only test
// is unsafe when a single-drop STM32 (F103 etc.) hangs off the same probe.
const RP2040_DPIDR: u32 = 0x0BC12477;

struct CmsisDapLink {
    _device: nusb::Device,
    _iface: nusb::Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
}

impl CmsisDapLink {
    /// Open the probe's CMSIS-DAP v2 vendor interface and its bulk endpoints.
    fn open_device(probe: &nusb::DeviceInfo) -> Result<Self> {
        // Prefer the vendor interface whose string names CMSIS-DAP (the v2 one
        // with bulk endpoints); fall back to the first vendor (0xFF) interface.
        let iface_num = probe
            .interfaces()
            .find(|i| {
                i.class() == 0xff
                    && i.interface_string()
                        .is_some_and(|s| s.to_ascii_uppercase().contains("CMSIS-DAP"))
            })
            .or_else(|| probe.interfaces().find(|i| i.class() == 0xff))
            .map(|i| i.interface_number())
            .context("no CMSIS-DAP vendor interface found")?;

        let device = probe.open().wait().context("open USB device")?;
        let iface = device
            .claim_interface(iface_num)
            .wait()
            .with_context(|| format!("claim CMSIS-DAP interface MI_{iface_num:02} (needs WinUSB on Windows)"))?;

        let config = device.active_configuration().context("read active configuration")?;
        let alt = config
            .interface_alt_settings()
            .find(|a| a.interface_number() == iface_num && a.alternate_setting() == 0)
            .context("interface alt setting 0 not found")?;
        // The CMSIS-DAP v2 interface carries only bulk endpoints (OUT, IN, and
        // optionally a SWO IN). Take the first OUT and first IN by address.
        let mut ep_out_addr = None;
        let mut ep_in_addr = None;
        for ep in alt.endpoints() {
            let a = ep.address();
            if a & 0x80 == 0 {
                ep_out_addr.get_or_insert(a);
            } else {
                ep_in_addr.get_or_insert(a);
            }
        }
        let ep_out_addr = ep_out_addr.context("no bulk OUT endpoint")?;
        let ep_in_addr = ep_in_addr.context("no bulk IN endpoint")?;
        let out = iface
            .endpoint::<Bulk, Out>(ep_out_addr)
            .map_err(|e| anyhow::anyhow!("open OUT endpoint 0x{ep_out_addr:02x}: {e}"))?;
        let inp = iface
            .endpoint::<Bulk, In>(ep_in_addr)
            .map_err(|e| anyhow::anyhow!("open IN endpoint 0x{ep_in_addr:02x}: {e}"))?;
        Ok(Self { _device: device, _iface: iface, ep_out: out, ep_in: inp })
    }

    /// Send a CMSIS-DAP v2 command (raw bytes, no report id) and read the reply.
    /// Validates the echoed command id in byte 0.
    fn command(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let timeout = std::time::Duration::from_millis(1000);
        self.ep_out
            .transfer_blocking(Buffer::from(payload.to_vec()), timeout)
            .into_result()
            .map_err(|e| anyhow::anyhow!("DAP OUT failed: {e}"))?;
        let resp = self
            .ep_in
            .transfer_blocking(Buffer::new(64), timeout)
            .into_result()
            .map_err(|e| anyhow::anyhow!("DAP IN failed: {e}"))?
            .into_vec();
        if resp.first().copied() != payload.first().copied() {
            anyhow::bail!(
                "DAP response id 0x{:02X} != command 0x{:02X}",
                resp.first().copied().unwrap_or(0),
                payload.first().copied().unwrap_or(0)
            );
        }
        Ok(resp)
    }

    fn connect_swd(&mut self) -> Result<()> {
        let r = self.command(&[DAP_CONNECT, 0x01])?;
        if r.get(1).copied() != Some(0x01) {
            anyhow::bail!("DAP_Connect SWD not accepted (resp {:?})", r.get(1));
        }
        Ok(())
    }

    fn swj_clock(&mut self, hz: u32) -> Result<()> {
        let b = hz.to_le_bytes();
        self.command(&[DAP_SWJ_CLOCK, b[0], b[1], b[2], b[3]])?;
        Ok(())
    }

    fn swj_sequence(&mut self, bits: u8, data: &[u8]) -> Result<()> {
        let mut payload = vec![DAP_SWJ_SEQUENCE, bits];
        payload.extend_from_slice(data);
        self.command(&payload)?;
        Ok(())
    }

    /// SWD line reset + JTAG-to-SWD switch + line reset + idle. Read-only.
    fn line_reset_and_switch(&mut self) -> Result<()> {
        self.swj_sequence(51, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07])?; // >=50 clocks high
        self.swj_sequence(16, &[0x9E, 0xE7])?; // JTAG-to-SWD magic 0xE79E
        self.swj_sequence(51, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07])?; // line reset
        self.swj_sequence(8, &[0x00])?; // idle
        Ok(())
    }

    /// One single-word DAP_Transfer. `req` is the request byte; `write` carries
    /// data for writes. Returns the 32-bit value for reads.
    fn transfer(&mut self, req: u8, write: Option<u32>) -> Result<u32> {
        let mut payload = vec![DAP_TRANSFER, 0x00, 0x01, req]; // DAP index 0, count 1
        if let Some(v) = write {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let resp = self.command(&payload)?;
        let count = resp.get(1).copied().unwrap_or(0);
        let ack = resp.get(2).copied().unwrap_or(0) & 0x07;
        if count != 1 || ack != 1 {
            anyhow::bail!("DAP_Transfer ack 0x{ack:02X} (count {count})");
        }
        if write.is_some() {
            Ok(0)
        } else {
            le_u32(&resp, 3).context("short DAP_Transfer read")
        }
    }

    fn dp_read(&mut self, addr: u8) -> Result<u32> {
        self.transfer(0x02 | (addr & 0x0C), None)
    }
    fn dp_write(&mut self, addr: u8, v: u32) -> Result<u32> {
        self.transfer(addr & 0x0C, Some(v))
    }
    fn ap_read(&mut self, addr: u8) -> Result<u32> {
        self.transfer(0x03 | (addr & 0x0C), None)
    }
    fn ap_write(&mut self, addr: u8, v: u32) -> Result<u32> {
        self.transfer(0x01 | (addr & 0x0C), Some(v))
    }

    /// Put the DP into dormant then wake it to SWD, matching probe-rs's RP2040
    /// path: line reset, JTAG-to-dormant (0x33BBBBBA), the 128-bit leave-dormant
    /// selection alert, then the 8-bit SWD activation code. All raw SWJ bit
    /// sequences — no DP access, no reset/halt of the core. The caller does one
    /// more line reset right before TARGETSEL.
    fn dormant_to_swd(&mut self) -> Result<()> {
        self.swj_sequence(54, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07])?; // line reset 51 high + 3 idle
        self.swj_sequence(31, &0x33BB_BBBAu32.to_le_bytes())?; // JTAG-to-dormant
        self.swj_sequence(8, &[0xFF])?; // >=8 cycles high
        self.swj_sequence(64, &[0x92, 0xF3, 0x09, 0x62, 0x95, 0x2D, 0x85, 0x86])?; // alert [0:63]
        self.swj_sequence(64, &[0xE9, 0xAF, 0xDD, 0xE3, 0xA2, 0x0E, 0xBC, 0x19])?; // alert [64:127]
        self.swj_sequence(12, &[0xA0, 0x01])?; // 4 low cycles + 8-bit SWD activation code 0x1A
        Ok(())
    }

    /// Line reset (51 high + 3 idle) immediately followed by the TARGETSEL
    /// packet, emitted as ONE DAP_SWJ_Sequence so no probe-side gap can fall
    /// between them (TARGETSEL must be the first packet after a line reset).
    fn line_reset_then_targetsel(&mut self, targetsel: u32) -> Result<()> {
        let parity = (targetsel.count_ones() % 2) as u128;
        let ts = (parity << 45) | ((targetsel as u128) << 13) | 0x1f99;
        let line_reset: u128 = 0x0007_FFFF_FFFF_FFFF; // 51 ones in a 54-bit field
        let combined = (ts << 54) | line_reset; // 54-bit reset, then 48-bit TARGETSEL
        self.swj_sequence(102, &combined.to_le_bytes()[..13])
    }

    /// One multidrop attempt: reset, select `targetsel`, read DPIDR. `dormant`
    /// chooses the dormant-to-SWD alert vs a plain line reset for the reset step.
    fn try_multidrop(&mut self, dormant: bool, targetsel: u32) -> Result<u32> {
        if dormant {
            self.dormant_to_swd()?;
        }
        self.line_reset_then_targetsel(targetsel)?; // contiguous reset + TARGETSEL
        let dpidr = self.dp_read(0x0)?;
        let _ = self.dp_write(0x0, 0x1E); // clear ABORT sticky (ORUNERR after reset)
        Ok(dpidr)
    }

    /// Bring up SWD and return `(DPIDR, multidrop)`. A DPv1 part that answers a
    /// plain DPIDR read is single-drop (STM32). The RP2040 is a DPv2 multidrop
    /// part that shares the bus with its sibling core and needs a TARGETSEL to
    /// route AP accesses. Tries line-reset+TARGETSEL (then dormant->SWD) first,
    /// accepting multidrop only on an EXACT RP2040 DPIDR match, then falls back
    /// to the single-drop STM32 path. A version-only (>= 2) test is unsafe here:
    /// a contended/floating bus — or an STM32 that ignores the TARGETSEL write —
    /// can return a value with the version field set, which would wrongly send a
    /// single-drop STM32 (F103 etc.) down the RP2040 SYSINFO path.
    fn bringup(&mut self) -> Result<(u32, bool)> {
        self.connect_swd()?;
        self.swj_clock(100_000)?;
        self.command(&[DAP_SWD_CONFIGURE, 0x00])?;
        // idle 0, wait_retry 128 (LE), match_retry 0 — let the probe retry WAITs.
        self.command(&[DAP_TRANSFER_CONFIGURE, 0x00, 0x80, 0x00, 0x00, 0x00])?;

        // Try SWD multidrop first (RP2040). Accept only the exact RP2040 DPIDR —
        // never the bus-contention garbage a non-selected read returns, nor a
        // single-drop STM32 that ignored the TARGETSEL. Each attempt resets first.
        let mut errs = Vec::new();
        for &dormant in &[true, false] {
            for &targetsel in &[RP2040_TARGETSEL_CORE0, RP2040_TARGETSEL_CORE1] {
                match self.try_multidrop(dormant, targetsel) {
                    Ok(dpidr) if dpidr == RP2040_DPIDR => return Ok((dpidr, true)),
                    Ok(dpidr) => errs.push(format!(
                        "{}/{targetsel:07x}: DPIDR 0x{dpidr:08X} != RP2040",
                        if dormant { "dormant" } else { "reset" }
                    )),
                    Err(e) => errs.push(format!(
                        "{}/{targetsel:07x}: {e}",
                        if dormant { "dormant" } else { "reset" }
                    )),
                }
            }
        }

        // Single-drop (STM32 / DPv1).
        self.line_reset_and_switch()?;
        if let Ok(dpidr) = self.dp_read(0x0) {
            if (dpidr >> 12) & 0xF < 2 {
                let _ = self.dp_write(0x0, 0x1E);
                return Ok((dpidr, false));
            }
        }
        anyhow::bail!("no SWD target answered; multidrop: {}", errs.join("; "))
    }

    /// Clear all sticky error flags via the DP ABORT register (STKCMP/STKERR/
    /// WDERR/ORUNERR, no DAPABORT). A single faulted AP access latches STKERR and
    /// then every later AP transfer FAULTs until this clears it.
    fn clear_sticky(&mut self) -> Result<()> {
        self.dp_write(0x0, 0x1E)?;
        Ok(())
    }

    /// Power up the debug/system domains and select AP0 for 32-bit MEM-AP reads.
    fn open_mem_ap(&mut self) -> Result<()> {
        self.dp_write(0x4, 0x5000_0000)?; // CTRL/STAT: CSYSPWRUPREQ | CDBGPWRUPREQ
        let mut ok = false;
        for _ in 0..50 {
            if self.dp_read(0x4)? & 0xA000_0000 == 0xA000_0000 {
                ok = true;
                break;
            }
        }
        if !ok {
            anyhow::bail!("DP power-up not acknowledged");
        }
        self.clear_sticky()?; // a prior multidrop/dormant probe can leave STKERR set
        self.dp_write(0x8, 0x0000_0000)?; // SELECT: AP 0, bank 0
        self.ap_write(0x0, 0x2300_0052)?; // CSW: 32-bit, debug enable
        Ok(())
    }

    /// Read one 32-bit word from the target's memory map (read-only). For a
    /// single CMSIS-DAP transfer the firmware completes the posted AP read and
    /// returns the data directly, so use the DRW read result. On a FAULT (sticky
    /// STKERR latched by an earlier access), clear it and retry once.
    fn read_mem32(&mut self, addr: u32) -> Result<u32> {
        match self.read_mem32_once(addr) {
            Ok(v) => Ok(v),
            Err(e) => {
                let _ = self.clear_sticky();
                self.read_mem32_once(addr).map_err(|_| e)
            }
        }
    }

    fn read_mem32_once(&mut self, addr: u32) -> Result<u32> {
        self.ap_write(0x4, addr)?; // TAR
        self.ap_read(0xC) // DRW — data returned by the probe
    }
}

/// Layer 1: identify an RP2040-based CMSIS-DAP debug probe from VID:PID + USB
/// descriptors (no SWD command issued). Keys match `print_stlink_controller_info`.
fn run_cmsisdap_controller_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let mut info = HashMap::new();
    let generation = match pid {
        0x000c => "Raspberry Pi Debug Probe (debugprobe firmware)",
        0x0004 => "picoprobe (Pico-as-probe firmware)",
        _ => "RP2040 CMSIS-DAP probe",
    };
    for (k, v) in [
        ("Layer", "1 - USB debug probe (CMSIS-DAP)"),
        ("Generation", generation),
        ("Controller MCU", "RP2040 (dual Arm Cortex-M0+)"),
        ("Core", "2x Arm Cortex-M0+"),
        ("Architecture", "Armv6-M, Thumb/Thumb-2 subset"),
        ("Max clock", "133 MHz"),
        ("Flash", "External QSPI (e.g. 2 MB on a Pico)"),
        ("SRAM", "264 KB"),
        ("Upstream", "USB 2.0 Full Speed (CMSIS-DAP v2 bulk + CDC UART)"),
        ("Downstream", "SWD (+ UART bridge)"),
    ] {
        info.insert(k.to_string(), v.to_string());
    }
    info.insert("USB identity".into(), format!("{vid:04x}:{pid:04x}"));
    if let Some(dev) = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid))
    {
        info.insert("Descriptor access".into(), "Available through nusb".into());
        if let Some(p) = dev.product_string() {
            info.insert("USB product".into(), p.to_string());
        }
        if let Some(s) = dev.serial_number() {
            info.insert("USB serial".into(), s.to_string());
        }
    }
    info.insert("Identification".into(), "VID:PID identifies an RP2040 CMSIS-DAP probe".into());
    info.insert("Layer 2".into(), "Auto-probed read-only below (native CMSIS-DAP)".into());
    Ok(info)
}

/// Layer 2: read the downstream SWD target through the CMSIS-DAP probe natively.
/// Returns keys for `print_stlink_target_info`; auto-detects target presence.
fn run_cmsisdap_target_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let mut info = HashMap::new();
    let probe = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid));
    let Some(probe) = probe else {
        info.insert("Layer 2".into(), "SKIP - CMSIS-DAP USB device not found".into());
        return Ok(info);
    };

    let mut link = match CmsisDapLink::open_device(&probe) {
        Ok(l) => l,
        Err(e) => {
            info.insert("Layer 2".into(), format!("SKIP - cannot open CMSIS-DAP interface: {e}"));
            #[cfg(windows)]
            info.insert(
                "Fix".into(),
                "bind WinUSB to the 'CMSIS-DAP v2' interface with Zadig (usbipd-rs --install zadig)".into(),
            );
            return Ok(info);
        }
    };
    info.insert("Probe".into(), "CMSIS-DAP v2 (native nusb)".into());

    let multidrop = match link.bringup() {
        Ok((dpidr, multidrop)) => {
            info.insert("DP IDCODE".into(), format!("0x{dpidr:08X}{}", if multidrop { " (SWD multidrop)" } else { "" }));
            multidrop
        }
        Err(e) => {
            info.insert("Layer 2".into(), format!("none - SWD bring-up failed: {e}"));
            info.insert(
                "Hint".into(),
                "CMSIS-DAP probe present but no target answered. Check: target powered; SWDIO/SWCLK/GND wired to the probe; correct SWD pins; NRST not held low.".into(),
            );
            return Ok(info);
        }
    };
    if let Err(e) = link.open_mem_ap() {
        info.insert("Layer 2".into(), format!("partial - DP up but MEM-AP failed: {e}"));
        return Ok(info);
    }

    // A multidrop DP is an RP-class part (RP2040 DPIDR 0x0BC12477): identify it
    // from SYSINFO CHIP_ID + CPUID rather than the STM32 DBGMCU path.
    if multidrop {
        match link.read_mem32(RP2040_SYSINFO_CHIP_ID) {
            Ok(chip_id) => {
                for (k, v) in rp2040_rows(&mut link, chip_id, multidrop) {
                    info.insert(k.to_string(), v);
                }
            }
            Err(e) => {
                info.insert("Layer 2".into(), format!("partial - DP/AP up but MEM read failed: {e}"));
            }
        }
        return Ok(info);
    }

    let db = load_chip_db();
    let (regs, dev_id, resolved) = collect_target_regs(|addr| link.read_mem32(addr), &db);
    if dev_id.is_none() {
        // The DP answered (we printed a DP IDCODE) but no identity register could
        // be read — the AHB-AP faulted every access. Report this distinctly
        // instead of falling through to the generic "no target detected": a debug
        // port DID respond, the memory bus just didn't. Capture the live fault.
        let why = link
            .read_mem32(0xE000ED00)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "AP read returned no usable value".into());
        info.insert(
            "Layer 2".into(),
            format!("target present but unreadable — Arm SW-DP answered, memory/AP access faulted ({why})"),
        );
        info.insert(
            "Hint".into(),
            "An Arm debug port responded but its memory bus did not (SWD ACK=FAULT). Likely causes: NRST held low \
             (target stuck in reset); target not fully powered (wire VTREF/3V3 + GND); on a Nucleo driven by an \
             EXTERNAL probe, remove the two CN2 (ST-LINK) jumpers so the on-board ST-LINK stops contending, then power \
             the board (USB/E5V); SWDIO/SWCLK swapped; or the chip has debug permanently disabled (STM32 RDP level 2)."
                .into(),
        );
        return Ok(info);
    }
    for (k, v) in format_target_rows(&regs, dev_id, resolved.as_ref()) {
        info.insert(k.to_string(), v);
    }
    Ok(info)
}

/// Format an RP2040 downstream target's read-only identity from its SYSINFO
/// CHIP_ID (manufacturer 0x927, part 0x0002) + GITREF + CPUID.
fn rp2040_rows(link: &mut CmsisDapLink, chip_id: u32, multidrop: bool) -> Vec<(&'static str, String)> {
    let part = (chip_id >> 12) & 0xFFFF;
    let revision = (chip_id >> 28) & 0xF;
    let name = if part == 0x0002 { "RP2040" } else { "Raspberry Pi silicon" };
    let stepping = match (part, revision) {
        (0x0002, 1) => " (B0/B1)",
        (0x0002, 2) => " (B2)",
        _ => "",
    };
    let mfr = chip_id & 0xFFF;
    let mut rows = vec![
        ("Device ID", format!("{name}{stepping} — Raspberry Pi (mfr 0x{mfr:03X}, part 0x{part:04X})")),
        ("Revision", format!("0x{revision:X}")),
    ];
    if let Ok(cpuid) = link.read_mem32(0xE000ED00) {
        rows.push(("Core", format!("{} (dual-core; core 0 selected)", cortex_core(cpuid))));
    }
    if let Ok(gitref) = link.read_mem32(RP2040_SYSINFO_GITREF) {
        rows.push(("Identification", format!("bootrom GITREF 0x{gitref:08X}, CHIP_ID 0x{chip_id:08X}")));
    }
    rows.push(("Flash size", "external QSPI (not read over SWD)".to_string()));
    rows.push(("SRAM", "264 KB".to_string()));
    rows.push(("Transport", format!("SWD{} (native CMSIS-DAP, read-only)", if multidrop { " multidrop" } else { "" })));
    rows.push(("Access", "Read-only identity registers".to_string()));
    rows
}

fn cmd_mcu_alive_native() -> Result<()> {
    let probes: Vec<nusb::DeviceInfo> = nusb::list_devices()
        .wait()
        .context("failed to enumerate USB hardware through nusb")?
        .filter(is_stlink_device)
        .collect();

    println!("=== Native ST-Link SWD Probe (no probe-rs / pyocd / stlink) ===");
    println!("Transport: raw USB bulk to MI_00 via nusb. On Windows this needs WinUSB on MI_00 (see --install-driver).");
    println!("Safety: only ENTER_SWD + read-only ID-register reads are issued — no halt, reset, erase, or memory write.");

    let chip_db = load_chip_db();
    if chip_db.is_empty() {
        println!("Chip data: built-in family table only (drop etc/chips/*.chip files to extend/override).");
    } else {
        println!("Chip data: {} external .chip definition(s) loaded; they take priority over the built-in table.", chip_db.len());
    }

    if probes.is_empty() {
        println!("Step 1 - USB probe: FAIL - no ST-Link USB device found");
        return Ok(());
    }

    for probe in probes {
        let vid = probe.vendor_id();
        let pid = probe.product_id();
        let serial = probe.serial_number().unwrap_or("");
        println!(
            "\n[{vid:04x}:{pid:04x}{}] {}",
            if serial.is_empty() { String::new() } else { format!(":{serial}") },
            probe.product_string().unwrap_or("ST-Link")
        );

        let mut link = match StlinkLink::open_device(&probe) {
            Ok(l) => {
                println!("Step 1/2 - open + claim MI_00: PASS - bulk transfers available");
                l
            }
            Err(e) => {
                println!("Step 1/2 - open + claim MI_00: FAIL - {e}");
                println!("  No-driver boundary: descriptors are readable, transfers are not.");
                println!("  Minimal fix (WinUSB on MI_00 only — no vendor driver / no probe-rs):");
                println!("    usbipd-rs --install-driver --confirm   (run in an Administrator shell)");
                continue;
            }
        };

        match link.get_version() {
            Ok(v) => println!("Step 3 - probe firmware: PASS - {v}"),
            Err(e) => {
                println!("Step 3 - probe firmware: FAIL - {e}");
                continue;
            }
        }

        match link.get_voltage_mv() {
            Ok(mv) => println!("Step 4 - target voltage: {:.2} V", mv as f64 / 1000.0),
            Err(e) if e.to_string().contains("stall") => {
                println!("Step 4 - target voltage: n/a (not reported by this ST-Link)")
            }
            Err(e) => println!("Step 4 - target voltage: FAIL - {e}"),
        }

        let status = match link.enter_swd() {
            Ok(s) => s,
            Err(e) => {
                println!("Step 5 - enter SWD: FAIL - {e}");
                continue;
            }
        };
        if status == STLINK_JTAG_OK {
            println!("Step 5 - enter SWD: PASS (status 0x80, core not halted)");
        } else {
            println!("Step 5 - enter SWD: WARN - status 0x{status:02X} (no target on SWD / wrong wiring?); continuing read attempts");
        }

        // ── Layer 2: read-only target identity ──
        match link.read_idcode() {
            Ok(dpidr) => println!("Step 6 - DP IDCODE (DPIDR): 0x{dpidr:08X}"),
            Err(e) => println!("Step 6 - DP IDCODE: FAIL - {e}"),
        }

        let (regs, dev_id, resolved) = stlink_read_regs(&mut link, &chip_db);
        let rows = format_target_rows(&regs, dev_id, resolved.as_ref());
        println!("Step 7 - target identity (read-only):");
        if rows.is_empty() {
            println!("  (no identity registers read — no SWD target answered)");
            println!("  Hint: {}", swd_no_target_hint(&mut link));
        } else {
            for (k, v) in rows {
                println!("  {:<18} {v}", format!("{k}:"));
            }
        }
    }
    Ok(())
}

// ============================================================================
// Tool installer (--install / --list-tools)
// ============================================================================

#[derive(Clone, Copy, PartialEq, Debug)]
enum Os {
    Windows,
    Macos,
    Linux,
}

fn current_os() -> Os {
    match std::env::consts::OS {
        "windows" => Os::Windows,
        "macos" => Os::Macos,
        _ => Os::Linux,
    }
}

#[derive(Clone, Copy)]
enum InstallStep {
    /// Run a package-manager-style command (e.g., `cargo install foo`).
    Command {
        program: &'static str,
        args: &'static [&'static str],
    },
    /// Download a URL and act on the file.
    Download {
        url: &'static str,
        /// Override filename when URL doesn't yield a sensible one (e.g.,
        /// query-only URLs like `?id=65`).
        filename: Option<&'static str>,
        action: DownloadAction,
    },
    /// Print a download URL and manual steps without fetching anything.
    /// For vendors whose web server blocks automated downloads (e.g. FTDI
    /// returns HTTP 403 to any non-browser client), so an auto-download would
    /// just fail for every user.
    Manual {
        url: &'static str,
        instructions: &'static str,
    },
    /// Extract an archive that's already committed to `windows-driver/` in
    /// the repo — no network round-trip. Use for upstreams whose only
    /// distribution channel is awkward (e.g. SourceForge HTML redirects).
    /// The file lives at `<project>/windows-driver/<filename>` regardless of
    /// host OS (that path is the project's canonical bundled-binary dir).
    LocalArchive {
        filename: &'static str,
        action: DownloadAction,
    },
}

#[derive(Clone, Copy)]
enum DownloadAction {
    /// Save and prompt user to run with given instructions (no execution).
    PromptToRun { instructions: &'static str },
    /// Extract a zip into `windows-driver/<tool_id>/`. Used for CLI bundles.
    ExtractToBundle { binary_hint: &'static str },
    /// Extract a zip, locate the inner binary, then prompt user to run it
    /// (typically as administrator). Used for driver installers shipped as zip.
    ExtractAndPrompt {
        binary_hint: &'static str,
        instructions: &'static str,
    },
}

struct ToolSpec {
    id: &'static str,
    name: &'static str,
    purpose: &'static str,
    /// Resolve install step for the given OS, or None if unsupported there.
    resolve: fn(Os) -> Option<InstallStep>,
    /// Optional: command to test if already installed (returns Some if found).
    check_command: Option<&'static str>,
}

const PICOTOOL_WIN_URL: &str   = "https://github.com/raspberrypi/pico-sdk-tools/releases/download/v2.2.0-3/picotool-2.2.0-a4-x64-win.zip";
const PICOTOOL_MAC_URL: &str   = "https://github.com/raspberrypi/pico-sdk-tools/releases/download/v2.2.0-3/picotool-2.2.0-a4-mac.zip";
const PICOTOOL_LINUX_URL: &str = "https://github.com/raspberrypi/pico-sdk-tools/releases/download/v2.2.0-3/picotool-2.2.0-a4-x86_64-lin.tar.gz";

const PICOTOOL_LINUX_INSTRUCTIONS: &str = "Linux: the archive is .tar.gz (zip extraction skipped). Extract manually:\n\
  tar xzf <downloaded_file> -C ~/.local/bin/picotool/\n\
or install via your package manager (Ubuntu 24.04+: `sudo apt install picotool`).";

const ZADIG_URL: &str = "https://github.com/pbatard/libwdi/releases/download/v1.5.1/zadig-2.9.exe";

const ZADIG_INSTRUCTIONS: &str = "1. Right-click the downloaded zadig-2.9.exe and pick 'Run as administrator'.\n\
2. In Zadig: Options → check 'List All Devices'.\n\
3. Pick the BOOTSEL/PICOBOOT/RP2 Boot device from the dropdown.\n\
4. Set the right-side driver to 'WinUSB' and click 'Replace Driver'.\n\
5. Re-run usbipd-rs --probe to verify pi pico probing works.";

// ── Step-2 tools ──
const AVRDUDE_WIN_URL: &str =
    "https://github.com/avrdudes/avrdude/releases/download/v8.1/avrdude-v8.1-windows-x64.zip";

const CP210X_WIN_URL: &str =
    "https://www.silabs.com/documents/public/software/CP210x_Universal_Windows_Driver.zip";

const CP210X_INSTRUCTIONS: &str =
    "Silabs Universal Driver is .inf-based (no .exe installer). Two ways to install:\n\
     \n\
     A) Right-click the silabser.inf path printed above → 'Install'.\n\
        (Confirm any 'Open File' / UAC prompt.)\n\
     \n\
     B) From an admin PowerShell, run:\n\
          pnputil /add-driver \"<silabser.inf path>\" /install\n\
     \n\
     Replug your CP2102/CP2104 board after install — it should appear as a COM port.";

const CH340_WIN_URL: &str = "https://www.wch-ic.com/download/file?id=65";

const CH340_INSTRUCTIONS: &str =
    "1. Right-click CH341SER.EXE (path printed above) → 'Run as administrator'.\n\
     2. Click 'INSTALL' in the WCH installer dialog.\n\
     3. Replug the CH340-based board to bind the new driver.";

const FTDI_CDM_URL: &str =
    "https://ftdichip.com/wp-content/uploads/2021/08/CDM212364_Setup.zip";

const FTDI_INSTRUCTIONS: &str =
    "FTDI's web server returns HTTP 403 to non-browser clients, so this driver\n\
     cannot be auto-downloaded — fetch it in a browser instead:\n\
     \n\
     1. Open the URL above in a browser and save CDM212364_Setup.zip.\n\
     2. Extract it, then right-click CDM212364_Setup.exe → 'Run as administrator'.\n\
     3. Replug the FT232R board — it should appear as a COM port.\n\
     \n\
     You usually DON'T need this: Windows 10/11 installs the FTDI VCP driver\n\
     automatically via Windows Update. If your board already shows a COMx\n\
     port, the driver is already working — this entry is just for offline or\n\
     freshly-imaged machines.\n\
     \n\
     Chocolatey users can instead run:  choco install ftdi-drivers";

// stm32flash upstream lives on SourceForge whose download URLs require
// browser-side redirects, so instead of auto-downloading we ship the
// upstream v0.7 binary zip in windows-driver/ (227 KB, three-OS bundle:
// stm32flash.exe + stm32flash_linux + stm32flash_macos) and just extract it.
const STM32FLASH_BUNDLE: &str = "stm32flash-0.7-binaries.zip";

// arduino-cli ships per-OS archives; the `_latest_` URLs are stable (302 →
// current release). Windows is a .zip we can auto-extract; macOS uses Homebrew
// (matches avrdude); Linux is a .tar.gz the bundled zip crate can't open, so we
// download and print manual-extract steps (same pattern as picotool on Linux).
const ARDUINO_CLI_WIN_URL: &str =
    "https://downloads.arduino.cc/arduino-cli/arduino-cli_latest_Windows_64bit.zip";
const ARDUINO_CLI_LINUX_URL: &str =
    "https://downloads.arduino.cc/arduino-cli/arduino-cli_latest_Linux_64bit.tar.gz";
const ARDUINO_CLI_LINUX_INSTRUCTIONS: &str = "Linux: the archive is .tar.gz (zip extraction skipped). Extract manually:\n\
  tar xzf <downloaded_file> -C ~/.local/bin/\n\
or install via the official script:\n\
  curl -fsSL https://raw.githubusercontent.com/arduino/arduino-cli/master/install.sh | sh";

// dfu-util upstream ships only a SourceForge .tar.xz behind browser redirects,
// so Windows is a Manual step; macOS/Linux have it in brew / apt.
const DFU_UTIL_WIN_URL: &str = "https://dfu-util.sourceforge.net/releases/";

const DFU_UTIL_WIN_INSTRUCTIONS: &str =
    "Windows has no auto-installer for dfu-util (SourceForge serves a .tar.xz\n\
     behind browser redirects). Install manually:\n\
     \n\
     1. Open the URL above and download the latest dfu-util-<ver>-binaries.tar.xz.\n\
     2. Extract it (7-Zip handles .tar.xz); add the win64\\ folder to PATH, or\n\
        copy dfu-util.exe next to usbipd-rs.exe.\n\
     3. The STM32 DFU interface needs a WinUSB driver — run\n\
          usbipd-rs --install zadig\n\
        and replace the '0483:DF11 / STM32 BOOTLOADER' device driver with WinUSB.\n\
     \n\
     Chocolatey users can instead run:  choco install dfu-util";

const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: "espflash",
        name: "espflash",
        purpose: "ESP32/ESP8266 chip identification & flashing",
        resolve: |_os| Some(InstallStep::Command {
            program: "cargo",
            args: &["install", "espflash"],
        }),
        check_command: Some("espflash"),
    },
    ToolSpec {
        id: "pyocd",
        name: "pyOCD",
        purpose: "CMSIS-DAP / DAPLink target chip identification",
        resolve: |_os| Some(InstallStep::Command {
            program: "pip",
            args: &["install", "pyocd"],
        }),
        check_command: Some("pyocd"),
    },
    ToolSpec {
        id: "picotool",
        name: "picotool",
        purpose: "Raspberry Pi RP2040/RP2350 inspection & flashing",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: PICOTOOL_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "picotool.exe" },
            }),
            Os::Macos => Some(InstallStep::Download {
                url: PICOTOOL_MAC_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "picotool" },
            }),
            Os::Linux => Some(InstallStep::Download {
                url: PICOTOOL_LINUX_URL,
                filename: None,
                action: DownloadAction::PromptToRun { instructions: PICOTOOL_LINUX_INSTRUCTIONS },
            }),
        },
        check_command: None, // bundled — find_picotool() locates it
    },
    ToolSpec {
        id: "zadig",
        name: "Zadig",
        purpose: "Windows-only: replace a USB device's driver with WinUSB so libusb-based tools (picotool, etc.) can talk to it.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: ZADIG_URL,
                filename: None,
                action: DownloadAction::PromptToRun { instructions: ZADIG_INSTRUCTIONS },
            }),
            _ => None,
        },
        check_command: None,
    },

    // ── Step-2 tools ───────────────────────────────────────────────────
    ToolSpec {
        id: "avrdude",
        name: "avrdude",
        purpose: "AVR (Arduino Uno/Mega/Leonardo/Micro) chip ID & flashing",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: AVRDUDE_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "avrdude.exe" },
            }),
            Os::Macos => Some(InstallStep::Command {
                program: "brew",
                args: &["install", "avrdude"],
            }),
            Os::Linux => Some(InstallStep::Command {
                program: "sudo",
                args: &["apt-get", "install", "-y", "avrdude"],
            }),
        },
        check_command: Some("avrdude"),
    },
    ToolSpec {
        id: "stm32flash",
        name: "stm32flash",
        purpose: "STM32 / GD32 (and bootloader-compatible clones) chip ID & flashing via UART bootloader. Bundled v0.7 zip ships binaries for Windows, Linux, and macOS.",
        // The bundled zip carries all three OS binaries; the per-OS
        // binary_hint just tells the post-extract scan which file to highlight.
        resolve: |os| Some(InstallStep::LocalArchive {
            filename: STM32FLASH_BUNDLE,
            action: DownloadAction::ExtractToBundle {
                binary_hint: match os {
                    Os::Windows => "stm32flash.exe",
                    Os::Linux   => "stm32flash_linux",
                    Os::Macos   => "stm32flash_macos",
                },
            },
        }),
        // Bundled binary isn't on PATH — find_stm32flash() locates it at runtime
        // (similar to find_picotool()), so a `which`-style probe would be misleading.
        check_command: None,
    },
    ToolSpec {
        id: "dfu-util",
        name: "dfu-util",
        purpose: "STM32 USB DFU (DfuSe) bootloader (0483:DF11) memory-map / chip-family ID & flashing.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Manual {
                url: DFU_UTIL_WIN_URL,
                instructions: DFU_UTIL_WIN_INSTRUCTIONS,
            }),
            Os::Macos => Some(InstallStep::Command {
                program: "brew",
                args: &["install", "dfu-util"],
            }),
            Os::Linux => Some(InstallStep::Command {
                program: "sudo",
                args: &["apt-get", "install", "-y", "dfu-util"],
            }),
        },
        check_command: Some("dfu-util"),
    },
    ToolSpec {
        id: "ravedude",
        name: "ravedude",
        purpose: "avr-hal `cargo run` runner — wraps avrdude to flash Rust AVR firmware & open a serial monitor.",
        resolve: |_os| Some(InstallStep::Command {
            program: "cargo",
            args: &["install", "ravedude"],
        }),
        check_command: Some("ravedude"),
    },
    ToolSpec {
        id: "arduino-cli",
        name: "Arduino CLI",
        purpose: "Arduino board/core manager. `arduino-cli core install arduino:avr` then bundles avrdude (and esp32 core → esptool, etc.).",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: ARDUINO_CLI_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "arduino-cli.exe" },
            }),
            Os::Macos => Some(InstallStep::Command {
                program: "brew",
                args: &["install", "arduino-cli"],
            }),
            Os::Linux => Some(InstallStep::Download {
                url: ARDUINO_CLI_LINUX_URL,
                filename: None,
                action: DownloadAction::PromptToRun { instructions: ARDUINO_CLI_LINUX_INSTRUCTIONS },
            }),
        },
        check_command: Some("arduino-cli"),
    },
    ToolSpec {
        id: "cp210x",
        name: "Silabs CP210x VCP Driver",
        purpose: "Windows-only: USB Serial driver for ESP32 dev boards using CP2102/CP2104.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: CP210X_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractAndPrompt {
                    binary_hint: "silabser.inf",
                    instructions: CP210X_INSTRUCTIONS,
                },
            }),
            Os::Macos => None,  // Mac CP210x driver is in-kernel since macOS 11+
            Os::Linux => None,  // Linux ships cp210x kernel module by default
        },
        check_command: None,
    },
    ToolSpec {
        id: "ch340",
        name: "WCH CH340/CH341 Driver",
        purpose: "Windows-only: USB Serial driver for cheap clone Arduinos & ESP boards using CH340G.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: CH340_WIN_URL,
                filename: Some("CH341SER.EXE"),
                action: DownloadAction::PromptToRun { instructions: CH340_INSTRUCTIONS },
            }),
            Os::Macos => None,  // WCH provides separate macOS package; out of scope
            Os::Linux => None,  // ch341 kernel module ships with mainline Linux
        },
        check_command: None,
    },
    ToolSpec {
        id: "ftdi",
        name: "FTDI VCP Driver (CDM)",
        purpose: "Windows-only: USB Serial (VCP) driver for FTDI FT232R/FT232RL — classic Arduinos & USB-UART adapters. Windows 10/11 usually installs it automatically.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Manual {
                url: FTDI_CDM_URL,
                instructions: FTDI_INSTRUCTIONS,
            }),
            Os::Macos => None,  // macOS ships an FTDI VCP driver in-kernel
            Os::Linux => None,  // Linux ftdi_sio kernel module ships by default
        },
        check_command: None,
    },
];

fn cmd_list_tools() -> Result<()> {
    let os = current_os();
    println!("Detected OS: {os:?}\n");
    println!("{:<12}  {:<10}  {:<10}  {}", "ID", "STATUS", "PROVIDER", "PURPOSE");
    println!("{}", "-".repeat(80));
    for tool in TOOLS {
        let status = match tool.check_command {
            Some(cmd) => {
                if which_cmd(cmd).is_some() {
                    "installed"
                } else {
                    "missing"
                }
            }
            None => "—",
        };
        let provider = match (tool.resolve)(os) {
            Some(InstallStep::Command { program, .. }) => program,
            Some(InstallStep::Download { .. }) => "download",
            Some(InstallStep::Manual { .. }) => "manual",
            Some(InstallStep::LocalArchive { .. }) => "bundled",
            None => "(n/a on this OS)",
        };
        println!("{:<12}  {:<10}  {:<10}  {}", tool.id, status, provider, tool.purpose);
    }
    println!();
    println!("Run `usbipd-rs --install <ID>` to install.");
    Ok(())
}

fn which_cmd(cmd: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for ext in exts {
            let candidate = dir.join(format!("{cmd}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn cmd_install(tool_id: &str) -> Result<()> {
    let tool = TOOLS
        .iter()
        .find(|t| t.id == tool_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown tool '{tool_id}'. Try `--list-tools`."))?;
    let os = current_os();
    let step = (tool.resolve)(os).ok_or_else(|| {
        anyhow::anyhow!("{} has no install path on {:?}", tool.name, os)
    })?;

    println!("=== Installing {} ===", tool.name);
    println!("Purpose: {}", tool.purpose);
    println!("OS:      {os:?}\n");

    match step {
        InstallStep::Command { program, args } => {
            run_command(program, args)?;
        }
        InstallStep::Download { url, filename, action } => {
            let dest_dir = bundle_dir()?;
            std::fs::create_dir_all(&dest_dir)
                .with_context(|| format!("Could not create {}", dest_dir.display()))?;
            let fname = filename.unwrap_or_else(|| {
                url.rsplit('/').next().unwrap_or("download.bin")
            });
            let dest = dest_dir.join(fname);
            download_file(url, &dest)?;
            apply_download_action(tool.id, &dest, &dest_dir, action)?;
        }
        InstallStep::LocalArchive { filename, action } => {
            let src = local_archive_path(filename)?;
            if !src.is_file() {
                anyhow::bail!(
                    "Bundled archive missing: {}\n\
                     Re-clone the repository or place the file there manually.",
                    src.display()
                );
            }
            let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
            println!("  Source:      {} ({} bytes, bundled)", src.display(), size);
            let dest_dir = bundle_dir()?;
            std::fs::create_dir_all(&dest_dir)
                .with_context(|| format!("Could not create {}", dest_dir.display()))?;
            apply_download_action(tool.id, &src, &dest_dir, action)?;
        }
        InstallStep::Manual { url, instructions } => {
            println!("  Download URL: {url}");
            println!("\n  Manual steps:");
            for line in instructions.lines() {
                println!("    {line}");
            }
        }
    }
    println!("\n  Done.");
    Ok(())
}

fn apply_download_action(
    tool_id: &str,
    src: &Path,
    dest_dir: &Path,
    action: DownloadAction,
) -> Result<()> {
    match action {
        DownloadAction::ExtractToBundle { binary_hint } => {
            let extract_to = dest_dir.join(tool_id);
            std::fs::create_dir_all(&extract_to)?;
            extract_zip(src, &extract_to)?;
            println!("\n  Extracted to: {}", extract_to.display());
            match find_in_dir(&extract_to, binary_hint) {
                Some(p) => println!("  Binary:       {}", p.display()),
                None => println!(
                    "  Binary '{binary_hint}' not found inside archive — inspect the directory manually."
                ),
            }
        }
        DownloadAction::ExtractAndPrompt { binary_hint, instructions } => {
            let extract_to = dest_dir.join(tool_id);
            std::fs::create_dir_all(&extract_to)?;
            extract_zip(src, &extract_to)?;
            println!("\n  Extracted to: {}", extract_to.display());
            match find_in_dir(&extract_to, binary_hint) {
                Some(p) => println!("  Installer:    {}", p.display()),
                None => println!(
                    "  Installer '{binary_hint}' not found — inspect the directory manually."
                ),
            }
            println!("\n  Manual steps:");
            for line in instructions.lines() {
                println!("    {line}");
            }
        }
        DownloadAction::PromptToRun { instructions } => {
            println!("\n  Saved to: {}", src.display());
            println!("\n  Manual steps:");
            for line in instructions.lines() {
                println!("    {line}");
            }
        }
    }
    Ok(())
}

/// Bundled archives (committed to the repo) always live under
/// `<project>/windows-driver/` regardless of host OS — that's the project's
/// canonical home for binary blobs (see CLAUDE.md).
fn local_archive_path(filename: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let project = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("Could not derive project root from exe path"))?;
    Ok(project.join("windows-driver").join(filename))
}

fn bundle_dir() -> Result<PathBuf> {
    // Prefer <project>/windows-driver/ for Win (matches existing layout),
    // <project>/tools/ for Mac/Linux.
    let exe = std::env::current_exe()?;
    let project = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("Could not derive project root from exe path"))?;
    let dir = if current_os() == Os::Windows {
        project.join("windows-driver")
    } else {
        project.join("tools")
    };
    Ok(dir)
}

fn download_file(url: &str, dest: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() > 0 {
            println!("  Existing:    {} ({} bytes)", dest.display(), meta.len());
            println!("  (delete the file to force re-download)");
            return Ok(());
        }
    }
    println!("  Downloading: {url}");
    let response = ureq::get(url).call().context("HTTP request failed")?;
    let total: Option<u64> = response
        .header("Content-Length")
        .and_then(|s| s.parse().ok());
    if let Some(t) = total {
        println!("  Size:        {t} bytes");
    }

    // Stage to <dest>.partial, then atomic rename. This avoids the
    // ERROR_SHARING_VIOLATION on Windows when AV/Defender (or our previous
    // run) still holds a handle to the existing dest file.
    let staging = dest.with_extension(match dest.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.partial"),
        None => "partial".into(),
    });
    let _ = std::fs::remove_file(&staging);

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&staging)
        .with_context(|| format!("Could not create {}", staging.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    let mut last_pct = -1i32;
    loop {
        let n = reader.read(&mut buf).context("Read from server failed")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("Disk write failed")?;
        written += n as u64;
        if let Some(t) = total {
            let pct = (written * 100 / t.max(1)) as i32;
            if pct != last_pct && pct % 10 == 0 {
                print!("  {pct}%... ");
                let _ = std::io::stdout().flush();
                last_pct = pct;
            }
        }
    }
    drop(file);
    println!();

    // Replace the (possibly locked) destination with the staged file.
    if dest.exists() {
        let _ = std::fs::remove_file(dest);
    }
    std::fs::rename(&staging, dest).with_context(|| {
        format!("Could not move {} → {}", staging.display(), dest.display())
    })?;

    println!("  Saved to:    {} ({} bytes)", dest.display(), written);
    Ok(())
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    println!("  Extracting:  {}", zip_path.display());
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("Could not open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid zip file")?;
    archive.extract(dest).context("Zip extraction failed")?;
    Ok(())
}

fn find_in_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_in_dir(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

fn run_command(program: &str, args: &[&str]) -> Result<()> {
    println!("  Running:     {} {}", program, args.join(" "));
    let status = Command::new(program).args(args).status().with_context(|| {
        format!("Could not invoke `{program}` (is it on PATH?)")
    })?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stlink_v2_1_profile_describes_the_usb_controller_not_the_target() {
        let profile = stlink_controller_profile(0x374b).expect("known ST-Link/V2-1 PID");

        assert_eq!(profile.controller_mcu, "STM32F103CBT6");
        assert_eq!(profile.core, "Arm Cortex-M3");
        assert_eq!(profile.flash, "128 KB");
        assert_eq!(profile.sram, "20 KB");
        assert_eq!(profile.flash_map, "0x08000000-0x0801FFFF");
        assert!(profile.self_debug.contains("external probe required"));
        assert!(profile.downstream.starts_with("SWD"));
    }

    #[test]
    fn layer2_decoder_identifies_stm32f446_without_writing_target() {
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x410FC241]);
        regs.insert(0xE0042000, vec![0x10000421]);
        regs.insert(0x1FFF7A22, vec![512]);
        regs.insert(0x1FFF7A10, vec![0x11223344, 0x55667788, 0x99AABBCC]);
        regs.insert(0x40023C14, vec![0x0000AA00]);

        let info = decode_stlink_regs(&regs, Some(0x421));

        assert!(info["Device ID"].contains("STM32F446"));
        assert!(info["Core"].contains("Cortex-M4"));
        assert_eq!(info["Flash size"], "512 KB");
        assert_eq!(info["Flash map"], "0x08000000-0x0807FFFF");
        assert!(info["Read protection"].contains("Disabled"));
        assert_eq!(info["Access"], "Read-only identity registers");
    }

    fn node(status: &str, problem: &str, service: &str) -> WindowsUsbDriverNode {
        let mut n = WindowsUsbDriverNode::default();
        n.set_field("STATUS", status);
        n.set_field("PROBLEM", problem);
        n.set_field("SERVICE", service);
        n
    }

    #[test]
    fn healthy_node_has_no_issue() {
        assert_eq!(classify_driver_issue(&node("OK", "0", "WINUSB")), None);
        assert_eq!(classify_driver_issue(&node("OK", "", "usbccgp")), None);
    }

    #[test]
    fn problem_28_without_service_is_a_missing_driver() {
        // The real ST-Link MI_00 case: enumerated, code 28, no bound service.
        assert_eq!(
            classify_driver_issue(&node("Error", "28", "")),
            Some(DriverIssue::MissingDriver)
        );
    }

    #[test]
    fn problem_28_with_service_is_a_load_failure_not_a_gap() {
        assert_eq!(
            classify_driver_issue(&node("Error", "28", "WINUSB")),
            Some(DriverIssue::DriverLoadFailure)
        );
    }

    #[test]
    fn problem_codes_map_to_distinct_categories() {
        assert_eq!(classify_driver_issue(&node("Error", "22", "x")), Some(DriverIssue::StoppedNode));
        assert_eq!(classify_driver_issue(&node("Error", "39", "x")), Some(DriverIssue::DriverLoadFailure));
        assert_eq!(classify_driver_issue(&node("Error", "52", "x")), Some(DriverIssue::SignatureFailure));
        assert_eq!(classify_driver_issue(&node("Error", "43", "x")), Some(DriverIssue::Blocked));
        assert_eq!(classify_driver_issue(&node("Error", "12", "x")), Some(DriverIssue::ResourceConflict));
        assert_eq!(classify_driver_issue(&node("Error", "99", "x")), Some(DriverIssue::Unknown));
    }

    #[test]
    fn stlink_mi00_advice_points_at_a_bundled_inf() {
        let advice = known_driver_advice("USB\\VID_0483&PID_374B&MI_00\\7&abc&0&0000")
            .expect("ST-Link MI_00 has known advice");
        assert!(advice.name.contains("STSW-LINK009"));
        assert_eq!(advice.local_inf, Some("windows-driver/stsw-link009/stlink_dbg_winusb.inf"));
        // The healthy sibling interfaces must not match this binding.
        assert!(known_driver_advice("USB\\VID_0483&PID_374B&MI_01\\x").is_none());
    }

    #[test]
    fn instance_vidpid_parses_uppercase_and_lowercase() {
        assert_eq!(instance_vidpid("USB\\VID_0483&PID_374B&MI_00\\x"), Some((0x0483, 0x374b)));
        assert_eq!(instance_vidpid("usb\\vid_10c4&pid_ea60"), Some((0x10c4, 0xea60)));
        assert_eq!(instance_vidpid("not-a-usb-id"), None);
    }

    #[test]
    fn driverstore_parser_extracts_oem_name_for_matching_block() {
        // Two blocks separated by a blank line; only the second mentions our INF.
        let text = "Published Name: oem10.inf\r\nOriginal Name: usbser.inf\r\n\r\nPublished Name: oem42.inf\r\nOriginal Name: stlink_dbg_winusb.inf\r\nProvider: STMicroelectronics";
        assert_eq!(parse_driverstore_oem(text, "stlink_dbg_winusb.inf"), vec!["oem42.inf"]);
        assert!(parse_driverstore_oem(text, "absent.inf").is_empty());
    }

    #[test]
    fn driverstore_parser_handles_lf_only_and_trailing_punctuation() {
        let text = "Published Name: oem7.inf,\nOriginal Name: stlink_dbg_winusb.inf";
        assert_eq!(parse_driverstore_oem(text, "STLINK_DBG_WINUSB.INF"), vec!["oem7.inf"]);
    }

    #[test]
    fn le_u32_reads_little_endian_at_offset() {
        // ST-Link debug-reg / idcode responses place the 32-bit value at offset 4.
        let resp = [0x80, 0x00, 0x00, 0x00, 0x41, 0x10, 0x00, 0x10];
        assert_eq!(le_u32(&resp, 4), Some(0x10001041));
        assert_eq!(le_u32(&resp, 0), Some(0x0000_0080));
        assert_eq!(le_u32(&resp, 6), None); // out of bounds, no panic
    }

    #[test]
    fn chip_file_parses_stlink_format() {
        let text = "\
# comment line
dev_type STM32F446
chip_id 0x421                // STM32_CHIPID_F446
flash_type F2_F4
flash_size_reg 0x1fff7a22
sram_size 0x20000            // 128 KB
option_base 0x40023c14
";
        let def = parse_chip_file(text).expect("has chip_id");
        assert_eq!(def.dev_id, 0x421);
        assert_eq!(def.name, "STM32F446");
        assert_eq!(def.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(def.sram_kb, Some(128));
        assert!(parse_chip_file("dev_type Foo\n").is_none()); // no chip_id
    }

    #[test]
    fn chip_int_accepts_hex_and_decimal() {
        assert_eq!(parse_chip_int("0x20000"), Some(0x20000));
        assert_eq!(parse_chip_int("0X10"), Some(16));
        assert_eq!(parse_chip_int("512"), Some(512));
        assert_eq!(parse_chip_int("0x421,"), Some(0x421)); // trailing punctuation
    }

    #[test]
    fn resolve_chip_falls_back_to_builtin_when_no_file() {
        let r = resolve_chip(0x421, &[]).expect("F446 is built-in");
        assert!(r.name.contains("STM32F446"));
        assert_eq!(r.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(r.uid_addr, Some(0x1FFF7A10)); // verified built-in UID addr
        assert_eq!(r.sram_kb, Some(128));
        assert_eq!(r.source, "built-in");
    }

    #[test]
    fn resolve_chip_file_overrides_builtin_but_keeps_verified_addrs() {
        let db = vec![ChipDef {
            dev_id: 0x421,
            name: "My Custom F446 Board".to_string(),
            flash_size_addr: Some(0x1FFF7A22),
            sram_kb: Some(128),
            source: "etc/chips/F446.chip".to_string(),
        }];
        let r = resolve_chip(0x421, &db).unwrap();
        assert_eq!(r.name, "My Custom F446 Board"); // file name wins
        assert_eq!(r.uid_addr, Some(0x1FFF7A10)); // UID still from built-in
        assert_eq!(r.source, "etc/chips/F446.chip");
    }

    #[test]
    fn resolve_chip_file_adds_a_part_unknown_to_builtin() {
        let db = vec![ChipDef {
            dev_id: 0x999,
            name: "STM32 Experimental".to_string(),
            flash_size_addr: Some(0x1FFF7A22),
            sram_kb: Some(64),
            source: "etc/chips/X.chip".to_string(),
        }];
        let r = resolve_chip(0x999, &db).expect("file-defined part resolves");
        assert_eq!(r.name, "STM32 Experimental");
        assert_eq!(r.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(r.sram_kb, Some(64));
        assert_eq!(r.uid_addr, None); // not in built-in → no UID/RDP
        assert_eq!(r.rdp_addr, None);
        assert!(resolve_chip(0x999, &[]).is_none()); // unknown without a file
    }

    #[test]
    fn native_dbgmcu_idcode_decodes_to_stm32_family() {
        // What --mcu-alive-native feeds decode_stlink_regs after reading DBGMCU.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x410FC241]); // CPUID: Cortex-M4
        regs.insert(0xE0042000, vec![0x10010421]); // DBGMCU: DEV_ID 0x421 = F446
        let info = decode_stlink_regs(&regs, Some(0x421));
        assert!(info["Device ID"].contains("STM32F446"));
        assert!(info["Core"].contains("Cortex-M4"));
    }

    #[test]
    fn native_stm32f103_target_decodes_as_layer2() {
        // A "Blue Pill" STM32F103C8 hanging off the probe (ST-Link or the RP2040
        // CMSIS-DAP single-drop path): DEV_ID 0x410, Cortex-M3, 64 KB flash.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]); // CPUID: Cortex-M3
        regs.insert(0xE0042000, vec![0x20036410]); // DBGMCU: DEV_ID 0x410, REV_ID 0x2003
        regs.insert(0x1FFFF7E0, vec![64]); // F1 flash-size register: 64 KB
        let info = decode_stlink_regs(&regs, Some(0x410));
        assert!(info["Device ID"].contains("STM32F1 medium-density"));
        assert!(info["Core"].contains("Cortex-M3"));
        assert_eq!(info["Flash size"], "64 KB");
        assert!(info["Revision"].contains("rev 1/2/3/X/Y"));
        assert!(!info.contains_key("Vendor")); // genuine ST REV_ID → no clone flag
    }

    #[test]
    fn gd32f103ret6_identified_as_gigadevice_not_stm32() {
        // GD32F103RET6 mirrors ST's high-density DEV_ID 0x414 but reports a
        // REV_ID (0x1309) outside ST's {0x1000,0x1001,0x1003} set; with 512 KB
        // flash it resolves to the GD32F103xE density, named as GigaDevice.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]); // Cortex-M3 r2p1
        regs.insert(0xE0042000, vec![0x13090414]); // DBGMCU: DEV_ID 0x414, REV_ID 0x1309
        regs.insert(0x1FFFF7E0, vec![512]); // F1 high-density flash-size word: 512 KB
        let info = decode_stlink_regs(&regs, Some(0x414));
        // Leads with the GD32 model, NOT the ST family name.
        assert!(info["Device ID"].contains("GD32F103xE"));
        assert!(info["Device ID"].contains("GD32F103RET6"));
        assert!(!info["Device ID"].contains("STM32"));
        assert!(info["Max clock"].contains("108 MHz"));
        let vendor = info.get("Vendor").expect("GD32 provenance noted");
        assert!(vendor.contains("GigaDevice") && vendor.contains("0x1309"));

        // A genuine high-density REV_ID must stay STM32 with no GD32 claim.
        regs.insert(0xE0042000, vec![0x10000414]);
        let genuine = decode_stlink_regs(&regs, Some(0x414));
        assert!(genuine["Device ID"].contains("STM32F1 high-density"));
        assert!(!genuine.contains_key("Vendor"));
    }

    #[test]
    fn gd32_density_maps_flash_to_letter() {
        assert_eq!(gd32_density(Some(512)), "xE");
        assert_eq!(gd32_density(Some(256)), "xC");
        assert_eq!(gd32_density(Some(64)), "x8");
        assert_eq!(gd32_density(None), "");
        // Non-clone REV_ID → no GD32 identity even on a GD32 DEV_ID.
        assert!(gd32_identify(0x414, 0x1000, Some(512)).is_none());
        // Clone REV_ID on an untracked DEV_ID → no model (generic clone flag only).
        assert!(gd32_identify(0x412, 0x9999, Some(64)).is_none());
    }

    #[test]
    fn format_target_rows_keeps_gd32_name_over_builtin_stm32() {
        // The full layer-2 path (ST-Link / CMSIS-DAP): decode names the GD32, and
        // the resolved built-in STM32 high-density entry must NOT clobber it.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]);
        regs.insert(0xE0042000, vec![0x13090414]); // DEV_ID 0x414, clone REV_ID 0x1309
        regs.insert(0x1FFFF7E0, vec![512]); // 512 KB → xE
        let resolved = resolve_chip(0x414, &[]).expect("0x414 resolves to built-in STM32");
        let rows = format_target_rows(&regs, Some(0x414), Some(&resolved));
        let dev = &rows.iter().find(|(k, _)| *k == "Device ID").unwrap().1;
        assert!(dev.contains("GD32F103xE") && dev.contains("GD32F103RET6"));
        assert!(!dev.contains("STM32"));
        assert!(rows.iter().any(|(k, v)| *k == "Vendor" && v.contains("GigaDevice")));
    }

    #[test]
    fn cmsisdap_rejects_non_rp2040_multidrop_dpidr() {
        // A bus-contention / STM32-ignored-TARGETSEL read can have version >= 2
        // set; only the exact RP2040 DPIDR must be treated as multidrop so an
        // STM32F103 reaches the single-drop decode path instead of RP2040 SYSINFO.
        assert_eq!(RP2040_DPIDR, 0x0BC12477);
        assert!((RP2040_DPIDR >> 12) & 0xF >= 2); // it IS DPv2…
        let bogus = 0x1BA02477u32; // …but a different DPIDR (also version 2) is not RP2040
        assert!((bogus >> 12) & 0xF >= 2);
        assert_ne!(bogus, RP2040_DPIDR);
    }
}
