use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Command;
use unicode_width::UnicodeWidthStr;

const HEADERS: [&str; 5] = ["BUSID", "VID:PID", "DEVICE", "STATE", "SPEED"];

const ESP_UART_BRIDGES: &[(u16, u16, &str)] = &[
    (0x10c4, 0xea60, "CP2102/CP2102N"),
    (0x10c4, 0xea70, "CP2105"),
    (0x10c4, 0xea71, "CP2108"),
    (0x1a86, 0x7523, "CH340"),
    (0x1a86, 0x55d4, "CH9102"),
    (0x0403, 0x6010, "FT2232"),
    (0x0403, 0x6014, "FT232H"),
    (0x0403, 0x6015, "FT231X"),
    (0x303a, 0x1001, "ESP32 USB-Serial-JTAG"),
    (0x303a, 0x4001, "ESP32 USB-OTG"),
];

fn is_esp_bridge(vid: u16, pid: u16) -> Option<&'static str> {
    ESP_UART_BRIDGES
        .iter()
        .find(|(v, p, _)| *v == vid && *p == pid)
        .map(|(_, _, n)| *n)
}

fn main() -> Result<()> {
    let probe = std::env::args().any(|a| a == "--probe-esp" || a == "-p");

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

    let candidates: Vec<(&Entry, &'static str)> = entries
        .iter()
        .filter_map(|e| {
            let (v, p) = e.vidpid_pair();
            is_esp_bridge(v, p).map(|n| (e, n))
        })
        .collect();

    if candidates.is_empty() {
        return Ok(());
    }

    println!();
    if probe {
        probe_esp(&candidates);
    } else {
        println!("ESP32 candidate bridge(s) detected:");
        for (e, name) in &candidates {
            println!("  - {} ({}) at BUSID {}", e.vidpid, name, e.busid);
        }
        println!();
        println!("Run with --probe-esp to query each via espflash board-info.");
        println!("Note: probing toggles DTR/RTS and resets the chip — do NOT use while");
        println!("      a 3D-printer or other live firmware is communicating.");
    }
    Ok(())
}

fn probe_esp(candidates: &[(&Entry, &'static str)]) {
    println!("=== ESP32 Probe (via espflash) ===");
    let serial_ports = serialport::available_ports().unwrap_or_default();

    for (entry, bridge_name) in candidates {
        let (vid, pid) = entry.vidpid_pair();
        let port_name = serial_ports.iter().find_map(|p| match &p.port_type {
            serialport::SerialPortType::UsbPort(info)
                if info.vid == vid && info.pid == pid =>
            {
                Some(p.port_name.clone())
            }
            _ => None,
        });

        match port_name {
            Some(port) => {
                println!("\n[{}  {}  {}]", entry.busid, bridge_name, port);
                match run_espflash_board_info(&port) {
                    Ok(info) if !info.is_empty() => print_esp_info(&info),
                    Ok(_) => println!("  (no chip info parsed from espflash output)"),
                    Err(e) => println!("  espflash error: {}", e),
                }
            }
            None => {
                println!(
                    "\n[{}  {}  {}]  no COM port found (attached to WSL? — `usbipd detach --busid {}` first)",
                    entry.busid, bridge_name, entry.vidpid, entry.busid
                );
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
