use crate::*;

/// DAPLink interface-firmware details read from the MSD `DETAILS.TXT`.
#[derive(Default)]
pub(crate) struct DaplinkInfo {
    pub(crate) drive_letter: Option<String>,
    pub(crate) unique_id: Option<String>,
    pub(crate) hic_id: Option<String>,
    pub(crate) daplink_mode: Option<String>,
    pub(crate) interface_version: Option<String>,
    pub(crate) bootloader_version: Option<String>,
    pub(crate) git_sha: Option<String>,
    pub(crate) local_mods: Option<String>,
    pub(crate) usb_interfaces: Option<String>,
    pub(crate) auto_reset: Option<String>,
    pub(crate) automation_allowed: Option<String>,
    pub(crate) overflow_detection: Option<String>,
    pub(crate) remount_count: Option<String>,
    pub(crate) url: Option<String>,
}

impl DaplinkInfo {
    fn rows(&self) -> [(&'static str, &Option<String>); 14] {
        [
            ("MSD mount", &self.drive_letter),
            ("Unique ID", &self.unique_id),
            ("HIC ID", &self.hic_id),
            ("DAPLink mode", &self.daplink_mode),
            ("Interface FW", &self.interface_version),
            ("Bootloader FW", &self.bootloader_version),
            ("DAPLink commit", &self.git_sha),
            ("Local mods", &self.local_mods),
            ("USB interfaces", &self.usb_interfaces),
            ("Auto reset", &self.auto_reset),
            ("Automation", &self.automation_allowed),
            ("Overflow det.", &self.overflow_detection),
            ("Remount count", &self.remount_count),
            ("Board URL", &self.url),
        ]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows().iter().all(|(_, v)| v.is_none())
    }

    pub(crate) fn print(&self, board_name: &str) {
        println!("  {:<20} {}", "Board:", board_name);
        for (label, v) in self.rows() {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{label}:"), v);
            }
        }

        // Identify the specific board variant from Unique ID prefix and surface
        // the underlying target chip, since DAPLink itself is just the interface.
        if let Some(uid) = &self.unique_id {
            if let Some((variant, target)) = microbit_identify(uid) {
                println!("  {:<20} {}", "Variant:", variant);
                println!("  {:<20} {}", "Target chip:", target);
            }
        }
    }
}

pub(crate) fn run_daplink_query() -> Result<DaplinkInfo> {
    let drive = find_daplink_drive().context(
        "DAPLink mass storage drive not found. \
         Check that the device shows up as a USB drive and DETAILS.TXT exists at its root.",
    )?;
    let path = drive.join("DETAILS.TXT");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Could not read {}", path.display()))?;
    let mut info = parse_daplink_details(&content);
    info.drive_letter = Some(drive.to_string_lossy().into_owned());
    Ok(info)
}

pub(crate) fn find_daplink_drive() -> Option<PathBuf> {
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
pub(crate) fn scan_mount_children(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("DETAILS.TXT").is_file() {
            return Some(path);
        }
    }
    None
}

pub(crate) fn parse_daplink_details(s: &str) -> DaplinkInfo {
    let mut info = DaplinkInfo::default();
    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim();
        let value = v.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        let value = value.to_string();
        match key {
            "Unique ID" => info.unique_id = Some(value),
            "HIC ID" => info.hic_id = Some(value),
            "Daplink Mode" => info.daplink_mode = Some(value),
            "Interface Version" => info.interface_version = Some(value),
            "Bootloader Version" => info.bootloader_version = Some(value),
            "Git SHA" => info.git_sha = Some(value),
            "Local Mods" => info.local_mods = Some(value),
            "USB Interfaces" => info.usb_interfaces = Some(value),
            "Auto Reset" => info.auto_reset = Some(value),
            "Automation allowed" => info.automation_allowed = Some(value),
            "Overflow detection" => info.overflow_detection = Some(value),
            "Remount count" => info.remount_count = Some(value),
            "URL" => info.url = Some(value),
            _ => {}
        }
    }
    info
}

/// pyocd probe/board identity from `pyocd json --probes` (no SWD connect/reset).
#[derive(Default)]
pub(crate) struct PyocdInfo {
    pub(crate) vendor_name: Option<String>,
    pub(crate) product_name: Option<String>,
    pub(crate) unique_id: Option<String>,
    pub(crate) board_vendor: Option<String>,
    pub(crate) board_name: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) info: Option<String>,
    pub(crate) usb_filter: Option<String>,
}

impl PyocdInfo {
    fn rows(&self) -> [(&'static str, &Option<String>); 8] {
        [
            ("Probe vendor", &self.vendor_name),
            ("Probe product", &self.product_name),
            ("Probe unique ID", &self.unique_id),
            ("Board vendor", &self.board_vendor),
            ("Board name", &self.board_name),
            ("Target chip", &self.target),
            ("Combined", &self.info),
            ("USB VID:PID", &self.usb_filter),
        ]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows().iter().all(|(_, v)| v.is_none())
    }

    pub(crate) fn print(&self) {
        let mut printed_any = false;
        for (label, v) in self.rows() {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{label}:"), v);
                printed_any = true;
            }
        }
        if !printed_any {
            println!("  (pyocd returned no probe info — is the device still in the same port?)");
        }
    }
}

pub(crate) fn run_pyocd_query(vid: u16, pid: u16) -> Result<PyocdInfo> {
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

pub(crate) fn parse_pyocd_output(s: &str, vid: u16, pid: u16) -> PyocdInfo {
    // pyocd JSON shape:
    //   { "boards": [ { "unique_id": "...", "info": "...",
    //                   "board_vendor": "...", "board_name": "...",
    //                   "target": "nrf52833", "vendor_name": "Arm",
    //                   "product_name": "BBC micro:bit CMSIS-DAP" } ] }
    //
    // We want fields from the first board entry. JSON parsing is small enough
    // here that adding serde_json isn't worth the compile-time cost — extract
    // each "key": "value" pair by string search.
    let mut info = PyocdInfo {
        usb_filter: Some(format!("{:04x}:{:04x}", vid, pid)),
        ..Default::default()
    };
    info.unique_id = extract_json_string(s, "unique_id");
    info.info = extract_json_string(s, "info");
    info.board_vendor = extract_json_string(s, "board_vendor");
    info.board_name = extract_json_string(s, "board_name");
    info.target = extract_json_string(s, "target");
    info.vendor_name = extract_json_string(s, "vendor_name");
    info.product_name = extract_json_string(s, "product_name");
    info
}

pub(crate) fn extract_json_string(json: &str, key: &str) -> Option<String> {
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
