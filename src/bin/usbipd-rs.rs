use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use unicode_width::UnicodeWidthStr;

const HEADERS: [&str; 5] = ["BUSID", "VID:PID", "DEVICE", "STATE", "SPEED"];

#[derive(Clone, Copy)]
enum ProbeKind {
    Espflash,
    Avrdude {
        mcu: &'static str,
        programmer: &'static str,
        /// Bootloader sync baud rates to try, in order. FT232R-class boards
        /// don't reveal which one the bootloader uses, so callers can pass
        /// several candidates.
        bauds: &'static [u32],
    },
    Picotool,
    Daplink,
    Pyocd,
}

impl ProbeKind {
    fn needs_serial_port(&self) -> bool {
        matches!(self, ProbeKind::Espflash | ProbeKind::Avrdude { .. })
    }

    fn tool_name(&self) -> &'static str {
        match self {
            ProbeKind::Espflash => "espflash",
            ProbeKind::Avrdude { .. } => "avrdude",
            ProbeKind::Picotool => "picotool",
            ProbeKind::Daplink => "DETAILS.TXT",
            ProbeKind::Pyocd => "pyocd",
        }
    }
}

struct KnownBoard {
    vid: u16,
    pid: u16,
    name: &'static str,
    probes: &'static [ProbeKind],
}

// Reusable probe pipelines
const ESP: &[ProbeKind] = &[ProbeKind::Espflash];
const PICO: &[ProbeKind] = &[ProbeKind::Picotool];
const DAP_PIPELINE: &[ProbeKind] = &[ProbeKind::Daplink, ProbeKind::Pyocd];

const KNOWN_BOARDS: &[KnownBoard] = &[
    // ── Arduino official boards (probe via avrdude) ──────────────────────
    KnownBoard { vid: 0x2341, pid: 0x0001, name: "Arduino Uno R1",
        probes: &[ProbeKind::Avrdude { mcu: "atmega328p", programmer: "arduino", bauds: &[115200] }] },
    KnownBoard { vid: 0x2341, pid: 0x0043, name: "Arduino Uno R3",
        probes: &[ProbeKind::Avrdude { mcu: "atmega328p", programmer: "arduino", bauds: &[115200] }] },
    KnownBoard { vid: 0x2341, pid: 0x0010, name: "Arduino Mega 2560",
        probes: &[ProbeKind::Avrdude { mcu: "atmega2560", programmer: "wiring", bauds: &[115200] }] },
    KnownBoard { vid: 0x2341, pid: 0x0042, name: "Arduino Mega 2560 R3",
        probes: &[ProbeKind::Avrdude { mcu: "atmega2560", programmer: "wiring", bauds: &[115200] }] },
    KnownBoard { vid: 0x2341, pid: 0x0044, name: "Arduino Mega ADK",
        probes: &[ProbeKind::Avrdude { mcu: "atmega2560", programmer: "wiring", bauds: &[115200] }] },
    KnownBoard { vid: 0x2341, pid: 0x8036, name: "Arduino Leonardo",
        probes: &[ProbeKind::Avrdude { mcu: "atmega32u4", programmer: "avr109", bauds: &[57600] }] },
    KnownBoard { vid: 0x2341, pid: 0x8037, name: "Arduino Micro",
        probes: &[ProbeKind::Avrdude { mcu: "atmega32u4", programmer: "avr109", bauds: &[57600] }] },

    // ── Arduino-compatible boards behind a generic FTDI USB-UART bridge ──
    // FT232R (0403:6001) is a bare USB-serial chip — it can't tell us what MCU
    // sits on its TX/RX lines. Classic Arduinos that use it (Nano, Duemilanove)
    // speak the STK500v1 bootloader protocol, so probe via avrdude. We request
    // atmega328p (the common case); avrdude runs with -F, so a 328PB / 168 /
    // LGT8F328P clone still connects and reports its true signature instead of
    // erroring. Bootloader baud varies by board age → try 57600 then 115200.
    KnownBoard { vid: 0x0403, pid: 0x6001, name: "FT232R Arduino (ATmega328-class)",
        probes: &[ProbeKind::Avrdude { mcu: "atmega328p", programmer: "arduino", bauds: &[57600, 115200] }] },

    // ── ESP32 USB-UART bridges & native USB (probe via espflash) ─────────
    KnownBoard { vid: 0x10c4, pid: 0xea60, name: "CP2102/CP2102N", probes: ESP },
    KnownBoard { vid: 0x10c4, pid: 0xea70, name: "CP2105",         probes: ESP },
    KnownBoard { vid: 0x10c4, pid: 0xea71, name: "CP2108",         probes: ESP },
    KnownBoard { vid: 0x1a86, pid: 0x7523, name: "CH340",          probes: ESP },
    KnownBoard { vid: 0x1a86, pid: 0x55d4, name: "CH9102",         probes: ESP },
    KnownBoard { vid: 0x0403, pid: 0x6010, name: "FT2232",         probes: ESP },
    KnownBoard { vid: 0x0403, pid: 0x6014, name: "FT232H",         probes: ESP },
    KnownBoard { vid: 0x0403, pid: 0x6015, name: "FT231X",         probes: ESP },
    KnownBoard { vid: 0x303a, pid: 0x1001, name: "ESP32 USB-Serial-JTAG", probes: ESP },
    KnownBoard { vid: 0x303a, pid: 0x4001, name: "ESP32 USB-OTG",  probes: ESP },

    // ── Raspberry Pi Pico (RP2040 / RP2350) (probe via picotool) ─────────
    KnownBoard { vid: 0x2e8a, pid: 0x0003, name: "RP2040 BOOTSEL (Pi Pico)",   probes: PICO },
    KnownBoard { vid: 0x2e8a, pid: 0x000f, name: "RP2350 BOOTSEL (Pi Pico 2)", probes: PICO },

    // ── DAPLink-based boards (BBC micro:bit, NXP FRDM, etc.) ─────────────
    // Read DETAILS.TXT from MSD first, then ask pyocd what target chip is on
    // the other end of the SWD lines (board database lookup; no chip reset).
    KnownBoard { vid: 0x0d28, pid: 0x0204, name: "DAPLink (mbed CMSIS-DAP)", probes: DAP_PIPELINE },
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
    usbipd-rs --list-tools          Show install status of all probe-tool dependencies.
    usbipd-rs --install <ID>        Download/install one tool by its ID.
    usbipd-rs --help                Show this help.
    usbipd-rs --version             Print the program version and exit.

OPTIONS:
    -p, --probe                Probe each detected board with the matching chip-level
                               tool (espflash, avrdude, picotool, DAPLink, pyocd).
                               Aliases: --probe-esp, --probe-arduino.
        --list-tools           List installable tools, their OS provider (cargo / pip /
                               brew / apt / download / manual) and install status.
        --install <ID>         Install one tool by ID. See --list-tools for IDs.
    -h, --help                 Show this help.
    -V, --version              Print version (from Cargo.toml) and exit.

WHAT GETS DETECTED (VID:PID → board → probe):
    10C4:EA60 / EA70 / EA71      Silabs CP210x bridge       → ESP32 (espflash)
    1A86:7523 / 55D4              WCH CH340 / CH9102         → ESP32 (espflash)
    0403:6010 / 6014 / 6015       FTDI FT2232 / FT232H / X   → ESP32 (espflash)
    0403:6001                     FTDI FT232R (Arduino)      → AVR  (avrdude)
    303A:1001 / 4001              ESP32 native USB-Serial    → ESP32 (espflash)
    2341:0001 / 0043              Arduino Uno R1 / R3        → AVR  (avrdude)
    2341:0010 / 0042 / 0044       Arduino Mega 2560 / ADK    → AVR  (avrdude)
    2341:8036 / 8037              Arduino Leonardo / Micro   → AVR  (avrdude)
    2E8A:0003                     RP2040 BOOTSEL (Pi Pico)   → RP2  (picotool)
    2E8A:000F                     RP2350 BOOTSEL (Pi Pico 2) → RP2  (picotool)
    0D28:0204                     mbed CMSIS-DAP / DAPLink   → DAP+SWD (DETAILS.TXT + pyocd)

INSTALLABLE TOOLS (see --list-tools for live status):
    espflash    cargo install espflash         ESP chip identification & flashing
    pyocd       pip install pyocd              CMSIS-DAP / DAPLink target chip ID
    picotool    GitHub release zip             Pi Pico (RP2040 / RP2350) inspection
    avrdude     GitHub release zip / brew /    Arduino (ATmega328P / 328PB /
                apt-get                        2560 / 32U4) chip ID & flashing
    ravedude    cargo install ravedude         avr-hal `cargo run` runner (Rust AVR)
    zadig       libwdi GitHub release          Win-only: replace USB driver → WinUSB
    cp210x      silabs.com universal driver    Win-only: CP2102/CP2104 VCP driver
    ch340       wch-ic.com CH341SER.EXE        Win-only: CH340/CH341 USB-Serial driver
    ftdi        ftdichip.com CDM (manual)      Win-only: FTDI FT232R VCP driver

EXAMPLES:
    # Listing only — no chip reset, safe to run any time
    usbipd-rs

    # Full probe — gets chip type, revision, flash size, MAC, etc.
    usbipd-rs --probe

    # Install picotool from upstream Raspberry Pi release
    usbipd-rs --install picotool

    # Install pyocd via pip (needed for DAPLink target identification)
    usbipd-rs --install pyocd

    # Download Zadig + see manual driver-replacement steps for Pi Pico BOOTSEL
    usbipd-rs --install zadig

NOTES:
    * --probe asserts DTR/RTS or SWD reset → resets target chip. Do NOT run
      while a 3D-printer or other live firmware is talking on the same port.
    * --install caches downloads under <project>/windows-driver/ (Windows) or
      <project>/tools/ (macOS/Linux). Existing files are reused — delete to
      force re-download.
    * Pi Pico (BOOTSEL) on Windows additionally needs a WinUSB driver swap via
      Zadig. Run `usbipd-rs --install zadig` and follow the printed steps.
    * macOS / Linux ship CP210x and CH340 kernel modules; only Windows needs
      those installers.

DATA SOURCES:
    USB enumeration   usbipd.exe list (Windows) + nusb (cross-platform USB lib)
    COM-port mapping  serialport crate
    Bundled binaries  windows-driver/picotool/, windows-driver/avrdude/, ...
"#;
    print!("{help}");
}

fn cmd_list_usb() -> Result<()> {
    let probe = std::env::args()
        .any(|a| matches!(a.as_str(), "--probe" | "--probe-esp" | "--probe-arduino" | "-p"));

    let entries = run_usbipd_list()?;
    let nusb_devs = nusb_by_vidpid();

    let rows: Vec<[String; 5]> = entries
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
                e.state.clone(),
                speed,
            ]
        })
        .collect();

    print_table(&rows);

    if rows.is_empty() {
        println!("(no connected USB devices reported by usbipd)");
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
                    ProbeKind::Picotool => "RP2",
                    ProbeKind::Daplink => "DAP",
                    ProbeKind::Pyocd => "SWD",
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
        println!("Run with --probe to query each board (espflash / avrdude / picotool / DAPLink / pyocd).");
        println!("Note: probing may reset the chip — do NOT use while a 3D-printer or");
        println!("      other live firmware is communicating.");
    }
    Ok(())
}

fn probe_boards(candidates: &[(&Entry, &'static KnownBoard)]) {
    println!("=== Board Probe ===");
    let serial_ports = serialport::available_ports().unwrap_or_default();

    for (entry, board) in candidates {
        let (vid, pid) = entry.vidpid_pair();

        for &probe in board.probes {
            let needs_port = probe.needs_serial_port();

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

            let header_port = port_name.as_deref().unwrap_or("(via libusb)");
            println!(
                "\n[{}  {}  {}  via {}]",
                entry.busid, board.name, header_port, probe.tool_name()
            );

            let result = match probe {
                ProbeKind::Espflash => run_espflash_board_info(port_name.as_deref().unwrap()),
                ProbeKind::Avrdude { mcu, programmer, bauds } => {
                    run_avrdude_query(port_name.as_deref().unwrap(), mcu, programmer, bauds)
                }
                ProbeKind::Picotool => run_picotool_info(vid, pid),
                ProbeKind::Daplink => run_daplink_query(),
                ProbeKind::Pyocd => run_pyocd_query(vid, pid),
            };

            match result {
                Ok(info) if !info.is_empty() => match probe {
                    ProbeKind::Espflash => print_esp_info(&info),
                    ProbeKind::Avrdude { .. } => print_avr_info(&info, board.name),
                    ProbeKind::Picotool => print_pico_info(&info, board.name),
                    ProbeKind::Daplink => print_daplink_info(&info, board.name),
                    ProbeKind::Pyocd => print_pyocd_info(&info),
                },
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

fn run_avrdude_query(
    port: &str,
    mcu: &str,
    programmer: &str,
    bauds: &[u32],
) -> Result<HashMap<String, String>> {
    // Serial bootloader programmers (arduino/wiring/avr109/stk500v1/stk500v2)
    // can read flash/eeprom/signature but NOT fuses — fuse reads return 0 silently.
    // Skip fuse reads here; only an ISP programmer on the ICSP header can read them.
    //
    // A generic USB-UART bridge (FT232R, etc.) doesn't reveal which baud the
    // Arduino bootloader runs at, so `bauds` may hold several candidates: try
    // each in order and return the first that syncs.
    //
    // `-F` overrides avrdude's signature check: when the actual MCU differs
    // from `-p` (a 328PB / 168 / LGT8F328P clone behind a generic bridge), the
    // run still succeeds and reports the true `Device signature` instead of
    // bailing. `-F` only relaxes the post-connect check — a board that never
    // syncs (wrong baud, or no Arduino at all) still fails cleanly. Probing is
    // read-only here (no -U), so overriding the check has no side effects.
    let mut last_err = String::from("avrdude produced no output");

    for &baud in bauds {
        let baud_str = baud.to_string();
        let output = Command::new("avrdude")
            .args([
                "-c", programmer,
                "-p", mcu,
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
            return Ok(parse_avrdude_output(&stdout, &stderr, mcu));
        }
        last_err = summarize_avrdude_error(&stderr, baud);
    }
    anyhow::bail!("{last_err}");
}

fn summarize_avrdude_error(stderr: &str, baud: u32) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let last = lines.last().copied().unwrap_or("unknown");
    // A signature mismatch (wrong -p) is the most actionable failure, but the
    // "Device signature = ..." line isn't the last one avrdude prints — pull it
    // out so the caller can tell a wrong MCU guess from a baud/wiring problem.
    match lines.iter().rev().find(|l| l.contains("Device signature")) {
        Some(sig) if *sig != last => format!("at {baud} baud — {sig}; {last}"),
        _ => format!("at {baud} baud — {last}"),
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
    for letter in 'A'..='Z' {
        let drive = PathBuf::from(format!("{letter}:\\"));
        let details = drive.join("DETAILS.TXT");
        if details.is_file() {
            return Some(drive);
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

fn print_table(rows: &[[String; 5]]) {
    let mut widths = HEADERS.map(UnicodeWidthStr::width);
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }

    let render = |cells: &[&str; 5]| -> String {
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
        let cells: [&str; 5] = [&r[0], &r[1], &r[2], &r[3], &r[4]];
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
    state: String,
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
        .map(|it| it.map(|d| ((d.vendor_id(), d.product_id()), d)).collect())
        .unwrap_or_default()
}

fn run_usbipd_list() -> Result<Vec<Entry>> {
    let output = Command::new("usbipd.exe")
        .arg("list")
        .output()
        .context("Failed to invoke usbipd. Install usbipd-win and ensure it's on PATH.")?;
    if !output.status.success() {
        anyhow::bail!(
            "usbipd list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_usbipd(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_usbipd(s: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut in_connected = false;
    let states = [
        "Attached - Shared",
        "Attached",
        "Not shared",
        "Shared",
    ];

    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed == "Connected:" {
            in_connected = true;
            continue;
        }
        if trimmed == "Persisted:" {
            in_connected = false;
            continue;
        }
        if !in_connected
            || trimmed.is_empty()
            || trimmed.starts_with("BUSID")
            || trimmed.starts_with("GUID")
        {
            continue;
        }

        let (busid, rest) = take_token(trimmed);
        let (vidpid, rest) = take_token(rest);
        let mut device = rest.trim().to_string();
        let mut state = String::new();
        for cand in states {
            if let Some(stripped) = device.strip_suffix(cand) {
                device = stripped.trim_end().to_string();
                state = cand.to_string();
                break;
            }
        }
        out.push(Entry {
            busid: busid.to_string(),
            vidpid: vidpid.to_string(),
            device,
            state,
        });
    }
    out
}

fn take_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
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
            match action {
                DownloadAction::ExtractToBundle { binary_hint } => {
                    let extract_to = dest_dir.join(tool.id);
                    std::fs::create_dir_all(&extract_to)?;
                    extract_zip(&dest, &extract_to)?;
                    println!("\n  Extracted to: {}", extract_to.display());
                    match find_in_dir(&extract_to, binary_hint) {
                        Some(p) => println!("  Binary:       {}", p.display()),
                        None => println!(
                            "  Binary '{binary_hint}' not found inside archive — inspect the directory manually."
                        ),
                    }
                }
                DownloadAction::ExtractAndPrompt { binary_hint, instructions } => {
                    let extract_to = dest_dir.join(tool.id);
                    std::fs::create_dir_all(&extract_to)?;
                    extract_zip(&dest, &extract_to)?;
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
                    println!("\n  Saved to: {}", dest.display());
                    println!("\n  Manual steps:");
                    for line in instructions.lines() {
                        println!("    {line}");
                    }
                }
            }
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

