use crate::*;

pub(crate) fn run_daplink_query() -> Result<HashMap<String, String>> {
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

pub(crate) fn parse_daplink_details(s: &str) -> HashMap<String, String> {
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

pub(crate) fn print_daplink_info(info: &HashMap<String, String>, board_name: &str) {
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

pub(crate) fn run_pyocd_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
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

pub(crate) fn parse_pyocd_output(s: &str, vid: u16, pid: u16) -> HashMap<String, String> {
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

pub(crate) fn print_pyocd_info(info: &HashMap<String, String>) {
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

