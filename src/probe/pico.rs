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

/// Parsed `picotool info -a` output. picotool emits a wide, open-ended set of
/// `key: value` rows; we keep the recognized ones (others are ignored, as the
/// original printer did) and render them in a fixed grouped order.
#[derive(Default)]
pub(crate) struct PicoInfo {
    pub(crate) chip: Option<String>,
    pub(crate) chip_revision: Option<String>,
    pub(crate) package: Option<String>,
    pub(crate) chip_id: Option<String>,
    pub(crate) unique_id: Option<String>,
    pub(crate) flash_size: Option<String>,
    pub(crate) flash_devinfo: Option<String>,
    pub(crate) rom_version: Option<String>,
    pub(crate) rom_gitrev: Option<String>,
    pub(crate) default_cpu: Option<String>,
    pub(crate) current_cpu: Option<String>,
    pub(crate) available_cpus: Option<String>,
    pub(crate) boot_type: Option<String>,
    pub(crate) last_booted_partition: Option<String>,
    pub(crate) secure_boot: Option<String>,
    pub(crate) debug_enable: Option<String>,
    pub(crate) secure_debug_enable: Option<String>,
    pub(crate) boot_random: Option<String>,
    pub(crate) boot2_name: Option<String>,
    pub(crate) program_name: Option<String>,
    pub(crate) program_desc: Option<String>,
    pub(crate) features: Option<String>,
    pub(crate) sdk_version: Option<String>,
    pub(crate) pico_board: Option<String>,
    pub(crate) binary_start: Option<String>,
    pub(crate) binary_end: Option<String>,
    pub(crate) build_date: Option<String>,
    pub(crate) build_attributes: Option<String>,
    pub(crate) build_id: Option<String>,
    pub(crate) embedded_drive: Option<String>,
}

impl PicoInfo {
    /// `(label, value, decode-as-flash-devinfo)` rows in display order.
    fn rows(&self) -> [(&'static str, &Option<String>, bool); 30] {
        [
            ("Chip", &self.chip, false),
            ("Chip revision", &self.chip_revision, false),
            ("Package", &self.package, false),
            ("Chip ID", &self.chip_id, false),
            ("Unique ID", &self.unique_id, false),
            ("Flash size", &self.flash_size, false),
            ("Flash devinfo", &self.flash_devinfo, true),
            ("ROM version", &self.rom_version, false),
            ("ROM gitrev", &self.rom_gitrev, false),
            ("Default CPU", &self.default_cpu, false),
            ("Current CPU", &self.current_cpu, false),
            ("Available CPUs", &self.available_cpus, false),
            ("Boot type", &self.boot_type, false),
            ("Last booted part", &self.last_booted_partition, false),
            ("Secure boot", &self.secure_boot, false),
            ("Debug enable", &self.debug_enable, false),
            ("Secure debug", &self.secure_debug_enable, false),
            ("Boot random", &self.boot_random, false),
            ("Boot2 stage", &self.boot2_name, false),
            ("Program name", &self.program_name, false),
            ("Program desc", &self.program_desc, false),
            ("Features", &self.features, false),
            ("SDK version", &self.sdk_version, false),
            ("pico_board", &self.pico_board, false),
            ("Binary start", &self.binary_start, false),
            ("Binary end", &self.binary_end, false),
            ("Build date", &self.build_date, false),
            ("Build attrs", &self.build_attributes, false),
            ("Build ID", &self.build_id, false),
            ("Embedded drive", &self.embedded_drive, false),
        ]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows().iter().all(|(_, v, _)| v.is_none())
    }

    pub(crate) fn print(&self, board_name: &str) {
        println!("  {:<20} {}", "Board:", board_name);
        for (label, val, is_devinfo) in self.rows() {
            if let Some(v) = val {
                let display_value = if is_devinfo {
                    format!("{} ({})", v, decode_flash_devinfo(v))
                } else {
                    v.clone()
                };
                println!("  {:<20} {}", format!("{label}:"), display_value);
            }
        }
    }
}

pub(crate) fn run_picotool_info(vid: u16, pid: u16) -> Result<PicoInfo> {
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

pub(crate) fn parse_picotool_output(s: &str) -> PicoInfo {
    let mut info = PicoInfo::default();
    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Lines like "Device Information" / "Program Information" have no ':' → skip.
        let Some(idx) = line.find(':') else { continue };
        let key = line[..idx].trim();
        let value = line[idx + 1..].trim();
        if key.is_empty() || value.is_empty() || value == "none" {
            continue;
        }
        let value = value.to_string();
        match key {
            "type" => info.chip = Some(value),
            "revision" => info.chip_revision = Some(value),
            "package" => info.package = Some(value),
            "chipid" => info.chip_id = Some(value),
            "unique id" => info.unique_id = Some(value),
            "flash size" => info.flash_size = Some(value),
            "flash devinfo" => info.flash_devinfo = Some(value),
            "ROM version" => info.rom_version = Some(value),
            "rom gitrev" => info.rom_gitrev = Some(value),
            "default cpu" => info.default_cpu = Some(value),
            "current cpu" => info.current_cpu = Some(value),
            "available cpus" => info.available_cpus = Some(value),
            "boot type" => info.boot_type = Some(value),
            "last booted partition" => info.last_booted_partition = Some(value),
            "secure boot" => info.secure_boot = Some(value),
            "debug enable" => info.debug_enable = Some(value),
            "secure debug enable" => info.secure_debug_enable = Some(value),
            "boot_random" => info.boot_random = Some(value),
            "boot2 name" => info.boot2_name = Some(value),
            "name" => info.program_name = Some(value),
            "description" => info.program_desc = Some(value),
            "features" => info.features = Some(value),
            "sdk version" => info.sdk_version = Some(value),
            "pico_board" => info.pico_board = Some(value),
            "binary start" => info.binary_start = Some(value),
            "binary end" => info.binary_end = Some(value),
            "build date" => info.build_date = Some(value),
            "build attributes" => info.build_attributes = Some(value),
            "build id" => info.build_id = Some(value),
            "embedded drive" => info.embedded_drive = Some(value),
            _ => {}
        }
    }
    info
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

