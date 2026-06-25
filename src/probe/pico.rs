use crate::*;

pub(crate) fn find_picotool() -> PathBuf {
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

pub(crate) fn run_picotool_info(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
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

pub(crate) fn parse_picotool_output(s: &str) -> HashMap<String, String> {
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

pub(crate) fn print_pico_info(info: &HashMap<String, String>, board_name: &str) {
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

pub(crate) fn decode_flash_devinfo(v: &str) -> String {
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

