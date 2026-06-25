
use crate::Result;

/// The sub-command selected by the command-line flags. Parsing is centralized
/// here so `run()` is a single match instead of a flag-checking ladder.
pub(crate) enum Cli {
    Help,
    Version,
    ListTools,
    DriverStatus,
    InstallDriver { confirm: bool },
    McuAliveNative,
    McuAlive,
    Install { tool: String },
    ListUsb { probe: bool },
}

/// Map raw args to a `Cli`. Earlier checks win, preserving the historical
/// flag precedence (e.g. `--help` overrides everything).
pub(crate) fn parse(args: &[String]) -> Result<Cli> {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        return Ok(Cli::Help);
    }
    if args.iter().any(|a| matches!(a.as_str(), "-V" | "--version")) {
        return Ok(Cli::Version);
    }
    if args.iter().any(|a| a == "--list-tools") {
        return Ok(Cli::ListTools);
    }
    if args.iter().any(|a| a == "--driver-status") {
        return Ok(Cli::DriverStatus);
    }
    if args.iter().any(|a| a == "--install-driver") {
        let confirm = args.iter().any(|a| matches!(a.as_str(), "--confirm" | "--yes" | "-y"));
        return Ok(Cli::InstallDriver { confirm });
    }
    if args.iter().any(|a| a == "--mcu-alive-native") {
        return Ok(Cli::McuAliveNative);
    }
    if args.iter().any(|a| a == "--mcu-alive") {
        return Ok(Cli::McuAlive);
    }
    if let Some(idx) = args.iter().position(|a| a == "--install") {
        let tool = args.get(idx + 1).map(String::as_str).unwrap_or("");
        if tool.is_empty() {
            anyhow::bail!("--install requires a tool name. Try `--list-tools`.");
        }
        return Ok(Cli::Install { tool: tool.to_string() });
    }
    let probe =
        args.iter().any(|a| matches!(a.as_str(), "--probe" | "--probe-esp" | "--probe-arduino" | "-p"));
    Ok(Cli::ListUsb { probe })
}

pub(crate) fn print_help() {
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

