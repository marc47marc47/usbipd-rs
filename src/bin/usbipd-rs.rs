use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use unicode_width::UnicodeWidthStr;

const HEADERS: [&str; 5] = ["BUSID", "VID:PID", "DEVICE", "STATE", "SPEED"];

#[derive(Clone, Copy)]
enum ProbeKind {
    Espflash,
    Avrdude {
        mcu: &'static str,
        programmer: &'static str,
        baud: u32,
    },
    Picotool,
}

impl ProbeKind {
    fn needs_serial_port(&self) -> bool {
        !matches!(self, ProbeKind::Picotool)
    }

    fn tool_name(&self) -> &'static str {
        match self {
            ProbeKind::Espflash => "espflash",
            ProbeKind::Avrdude { .. } => "avrdude",
            ProbeKind::Picotool => "picotool",
        }
    }
}

struct KnownBoard {
    vid: u16,
    pid: u16,
    name: &'static str,
    probe: ProbeKind,
}

const KNOWN_BOARDS: &[KnownBoard] = &[
    // ── Arduino official boards (probe via avrdude) ──────────────────────
    KnownBoard { vid: 0x2341, pid: 0x0001, name: "Arduino Uno R1",
        probe: ProbeKind::Avrdude { mcu: "atmega328p", programmer: "arduino", baud: 115200 } },
    KnownBoard { vid: 0x2341, pid: 0x0043, name: "Arduino Uno R3",
        probe: ProbeKind::Avrdude { mcu: "atmega328p", programmer: "arduino", baud: 115200 } },
    KnownBoard { vid: 0x2341, pid: 0x0010, name: "Arduino Mega 2560",
        probe: ProbeKind::Avrdude { mcu: "atmega2560", programmer: "wiring", baud: 115200 } },
    KnownBoard { vid: 0x2341, pid: 0x0042, name: "Arduino Mega 2560 R3",
        probe: ProbeKind::Avrdude { mcu: "atmega2560", programmer: "wiring", baud: 115200 } },
    KnownBoard { vid: 0x2341, pid: 0x0044, name: "Arduino Mega ADK",
        probe: ProbeKind::Avrdude { mcu: "atmega2560", programmer: "wiring", baud: 115200 } },
    KnownBoard { vid: 0x2341, pid: 0x8036, name: "Arduino Leonardo",
        probe: ProbeKind::Avrdude { mcu: "atmega32u4", programmer: "avr109", baud: 57600 } },
    KnownBoard { vid: 0x2341, pid: 0x8037, name: "Arduino Micro",
        probe: ProbeKind::Avrdude { mcu: "atmega32u4", programmer: "avr109", baud: 57600 } },

    // ── ESP32 USB-UART bridges & native USB (probe via espflash) ─────────
    KnownBoard { vid: 0x10c4, pid: 0xea60, name: "CP2102/CP2102N", probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x10c4, pid: 0xea70, name: "CP2105",         probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x10c4, pid: 0xea71, name: "CP2108",         probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x1a86, pid: 0x7523, name: "CH340",          probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x1a86, pid: 0x55d4, name: "CH9102",         probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x0403, pid: 0x6010, name: "FT2232",         probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x0403, pid: 0x6014, name: "FT232H",         probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x0403, pid: 0x6015, name: "FT231X",         probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x303a, pid: 0x1001, name: "ESP32 USB-Serial-JTAG", probe: ProbeKind::Espflash },
    KnownBoard { vid: 0x303a, pid: 0x4001, name: "ESP32 USB-OTG",  probe: ProbeKind::Espflash },

    // ── Raspberry Pi Pico (RP2040 / RP2350) (probe via picotool) ─────────
    KnownBoard { vid: 0x2e8a, pid: 0x0003, name: "RP2040 BOOTSEL (Pi Pico)",   probe: ProbeKind::Picotool },
    KnownBoard { vid: 0x2e8a, pid: 0x000f, name: "RP2350 BOOTSEL (Pi Pico 2)", probe: ProbeKind::Picotool },
];

fn lookup_board(vid: u16, pid: u16) -> Option<&'static KnownBoard> {
    KNOWN_BOARDS.iter().find(|b| b.vid == vid && b.pid == pid)
}

fn main() -> Result<()> {
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
            let kind = match b.probe {
                ProbeKind::Espflash => "ESP",
                ProbeKind::Avrdude { .. } => "AVR",
                ProbeKind::Picotool => "RP2",
            };
            println!("  - [{kind}] {} ({}) at BUSID {}", e.vidpid, b.name, e.busid);
        }
        println!();
        println!("Run with --probe to query each board (espflash / avrdude / picotool).");
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
        let needs_port = board.probe.needs_serial_port();

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
                "\n[{}  {}  {}]  no COM port found (attached to WSL? — `usbipd detach --busid {}` first)",
                entry.busid, board.name, entry.vidpid, entry.busid
            );
            continue;
        }

        let header_port = port_name.as_deref().unwrap_or("(via libusb)");
        println!(
            "\n[{}  {}  {}  via {}]",
            entry.busid, board.name, header_port, board.probe.tool_name()
        );

        let result = match board.probe {
            ProbeKind::Espflash => run_espflash_board_info(port_name.as_deref().unwrap()),
            ProbeKind::Avrdude { mcu, programmer, baud } => {
                run_avrdude_query(port_name.as_deref().unwrap(), mcu, programmer, baud)
            }
            ProbeKind::Picotool => run_picotool_info(vid, pid),
        };

        match result {
            Ok(info) if !info.is_empty() => match board.probe {
                ProbeKind::Espflash => print_esp_info(&info),
                ProbeKind::Avrdude { .. } => print_avr_info(&info, board.name),
                ProbeKind::Picotool => print_pico_info(&info, board.name),
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

fn run_avrdude_query(port: &str, mcu: &str, programmer: &str, baud: u32) -> Result<HashMap<String, String>> {
    // Serial bootloader programmers (arduino/wiring/avr109/stk500v1/stk500v2)
    // can read flash/eeprom/signature but NOT fuses — fuse reads return 0 silently.
    // Skip fuse reads here; only an ISP programmer on the ICSP header can read them.
    let baud_str = baud.to_string();
    let output = Command::new("avrdude")
        .args([
            "-c", programmer,
            "-p", mcu,
            "-P", port,
            "-b", &baud_str,
            "-v",
        ])
        .output()
        .context("avrdude not found on PATH (install via Arduino IDE, PlatformIO, or scoop install avrdude)")?;

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
    Ok(parse_avrdude_output(&stdout, &stderr, mcu))
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
            // Dev layout: <project>/target/release/usbipd-rs.exe
            //             ^^^^^^^^/windows-driver/picotool/picotool.exe
            let candidates = [
                dir.join("picotool.exe"),
                dir.join("picotool").join("picotool.exe"),
                dir.join("..").join("..").join("windows-driver").join("picotool").join("picotool.exe"),
                dir.join("..").join("..").join("..").join("windows-driver").join("picotool").join("picotool.exe"),
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
