use crate::*;

pub(crate) const HEADERS: [&str; 4] = ["BUSID", "VID:PID", "DEVICE", "SPEED"];
pub(crate) fn cmd_list_usb(probe: bool) -> Result<()> {
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
            let labels: Vec<&str> = b.probes.iter().map(|p| p.label()).collect();
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
pub(crate) fn print_table(rows: &[[String; 4]]) {
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

pub(crate) fn pad_display(s: &str, width: usize) -> String {
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
pub(crate) struct Entry {
    pub(crate) busid: String,
    pub(crate) vidpid: String,
    pub(crate) device: String,
}

impl Entry {
    pub(crate) fn vidpid_pair(&self) -> (u16, u16) {
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

pub(crate) fn nusb_by_vidpid() -> HashMap<(u16, u16), nusb::DeviceInfo> {
    nusb::list_devices()
        .wait()
        .map(|it| it.map(|d| ((d.vendor_id(), d.product_id()), d)).collect())
        .unwrap_or_default()
}

/// Enumerate USB devices directly through the cross-platform `nusb` crate.
/// Listing local hardware must not depend on the optional usbipd-win service
/// or CLI.
pub(crate) fn list_entries() -> Result<Vec<Entry>> {
    Ok(nusb_entries())
}

pub(crate) fn nusb_bus_numbers() -> HashMap<String, u8> {
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

pub(crate) fn nusb_bus_number(device: &nusb::DeviceInfo, _windows_buses: &HashMap<String, u8>) -> u8 {
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

pub(crate) fn nusb_busid(device: &nusb::DeviceInfo, windows_buses: &HashMap<String, u8>) -> String {
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
pub(crate) fn nusb_entries() -> Vec<Entry> {
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
