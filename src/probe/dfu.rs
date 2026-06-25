use crate::*;

/// Aggregated `dfu-util -l` listing for one DFU device (all alt settings folded
/// into one report). `memory_map` holds one `label: detail` line per region.
#[derive(Default)]
pub(crate) struct DfuInfo {
    pub(crate) flash_size: Option<String>,
    pub(crate) bcd_device: Option<String>,
    pub(crate) serial_number: Option<String>,
    pub(crate) alt_settings: Option<String>,
    pub(crate) vidpid: Option<String>,
    pub(crate) memory_map: Option<String>,
}

impl DfuInfo {
    pub(crate) fn is_empty(&self) -> bool {
        // `vidpid` is set only once at least one matching alt setting was seen.
        self.vidpid.is_none()
    }

    pub(crate) fn print(&self, board_name: &str) {
        println!("  {:<20} {}", "Board:", board_name);
        let rows: [(&str, &Option<String>); 5] = [
            ("Flash size", &self.flash_size),
            ("bcdDevice", &self.bcd_device),
            ("Serial number", &self.serial_number),
            ("Alt settings", &self.alt_settings),
            ("VID:PID", &self.vidpid),
        ];
        for (label, v) in rows {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{label}:"), v);
            }
        }
        if let Some(mm) = &self.memory_map {
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
}

pub(crate) fn run_dfu_info(vid: u16, pid: u16) -> Result<DfuInfo> {
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

pub(crate) fn parse_dfu_output(s: &str, vid: u16, pid: u16) -> DfuInfo {
    // Each matching line looks like:
    //   Found DFU: [0483:df11] ver=2200, devnum=5, cfg=1, intf=0, path="20-1.4",
    //              alt=0, name="@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg",
    //              serial="3576345C3137"
    // One line per alt setting; we aggregate them into a single report.
    let target = format!("[{:04x}:{:04x}]", vid, pid);
    let mut info = DfuInfo::default();
    let mut alt_count = 0u32;
    let mut regions: Vec<String> = Vec::new();

    for line in s.lines() {
        let line = line.trim();
        if !line.starts_with("Found DFU:") || !line.contains(&target) {
            continue;
        }
        alt_count += 1;

        if let Some(v) = dfu_field(line, "ver") {
            if info.bcd_device.is_none() {
                info.bcd_device = Some(format!("0x{v}"));
            }
        }
        if let Some(v) = dfu_field(line, "serial") {
            if info.serial_number.is_none() {
                info.serial_number = Some(v);
            }
        }
        if let Some(name) = dfu_field(line, "name") {
            if let Some((label, detail, flash_kb)) = format_dfu_region(&name) {
                regions.push(format!("{label}: {detail}"));
                if let Some(kb) = flash_kb {
                    info.flash_size = Some(format!("{kb} KB"));
                }
            }
        }
    }

    if alt_count == 0 {
        return info;
    }
    info.vidpid = Some(format!("{:04x}:{:04x}", vid, pid));
    info.alt_settings = Some(alt_count.to_string());
    if !regions.is_empty() {
        info.memory_map = Some(regions.join("\n"));
    }
    info
}

/// Pull the value of `key=` from a dfu-util listing line. Quoted values
/// (`name="..."`, `serial="..."`) run to the closing quote; bare values
/// (`ver=2200`) run to the next comma.
pub(crate) fn dfu_field(line: &str, key: &str) -> Option<String> {
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
pub(crate) fn format_dfu_region(name: &str) -> Option<(String, String, Option<u64>)> {
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
pub(crate) fn dfu_region_size_kb(layout: &str) -> Option<u64> {
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
