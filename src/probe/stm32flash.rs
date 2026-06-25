use crate::*;

pub(crate) fn find_stm32flash() -> PathBuf {
    if let Ok(p) = std::env::var("STM32FLASH_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    // Bundled v0.7 zip carries per-OS binaries with distinct names — pick the
    // matching one. (The zip extracts as windows-driver/stm32flash/stm32flash-
    // 0.7-binaries/<binary>.)
    let binary_name = match std::env::consts::OS {
        "windows" => "stm32flash.exe",
        "linux"   => "stm32flash_linux",
        "macos"   => "stm32flash_macos",
        _         => "stm32flash",
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Candidate paths relative to the binary:
            //   target/release/usbipd-rs.exe → ../../windows-driver/stm32flash/...
            //   target/debug/usbipd-rs.exe   → same shape
            //   plus a flat ./stm32flash{.exe,_linux,_macos} for portable layouts
            let inner = Path::new("windows-driver")
                .join("stm32flash")
                .join("stm32flash-0.7-binaries")
                .join(binary_name);
            let candidates = [
                dir.join(binary_name),
                dir.join("..").join("..").join(&inner),
                dir.join("..").join("..").join("..").join(&inner),
            ];
            for c in candidates {
                if c.exists() {
                    return c;
                }
            }
        }
    }
    // Fallback: bare name, resolved via PATH (covers users who ran
    // `scoop install stm32flash` / `brew install stm32flash` / `apt-get install
    // stm32flash` instead of the bundled extract).
    PathBuf::from("stm32flash")
}

/// Parsed `stm32flash` system-bootloader handshake output.
#[derive(Default)]
pub(crate) struct Stm32FlashInfo {
    pub(crate) version: Option<String>,
    pub(crate) option_1: Option<String>,
    pub(crate) option_2: Option<String>,
    pub(crate) device_id: Option<String>,
    pub(crate) ram: Option<String>,
    pub(crate) flash: Option<String>,
    pub(crate) option_ram: Option<String>,
    pub(crate) system_ram: Option<String>,
}

impl Stm32FlashInfo {
    pub(crate) fn is_empty(&self) -> bool {
        self.version.is_none()
            && self.option_1.is_none()
            && self.option_2.is_none()
            && self.device_id.is_none()
            && self.ram.is_none()
            && self.flash.is_none()
            && self.option_ram.is_none()
            && self.system_ram.is_none()
    }

    pub(crate) fn print(&self, board_name: &str) {
        println!("  {:<20} {}", "Bridge:", board_name);
        let rows: [(&str, &Option<String>); 8] = [
            ("Device ID", &self.device_id),
            ("Flash", &self.flash),
            ("RAM", &self.ram),
            ("System ROM", &self.system_ram),
            ("Option RAM", &self.option_ram),
            ("Option byte 1", &self.option_1),
            ("Option byte 2", &self.option_2),
            ("Bootloader ver", &self.version),
        ];
        for (label, v) in rows {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{label}:"), v);
            }
        }
    }
}

pub(crate) fn run_stm32flash_query(port: &str) -> Result<Stm32FlashInfo> {
    let stm32flash = find_stm32flash();
    let output = Command::new(&stm32flash)
        .arg(port)
        .output()
        .with_context(|| format!(
            "stm32flash not found (tried {} and PATH). Install with: usbipd-rs --install stm32flash",
            stm32flash.display()
        ))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        // stm32flash prints "Failed to init device, attempt N." when the chip
        // doesn't respond at 0x7F — that's the normal "no STM32 here" case,
        // not an install/usage error. Surface a one-line summary plus a hint
        // about BOOT0 (CH340 boards don't auto-trigger bootloader mode).
        let combined: Vec<&str> = stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| !l.starts_with("stm32flash "))
            .filter(|l| !l.starts_with("http"))
            .filter(|l| !l.starts_with("Using Parser"))
            .filter(|l| !l.starts_with("Interface"))
            .collect();
        let summary = combined.last().copied().unwrap_or("no response");
        anyhow::bail!(
            "{summary}\n\
             Hint: stm32flash needs the chip in system bootloader mode.\n\
             Pull BOOT0 HIGH and press RESET, then re-run --probe.\n\
             (Generic CH340 boards don't wire DTR/RTS to BOOT0/RESET, so this\n\
              cannot be triggered automatically.)"
        );
    }
    Ok(parse_stm32flash_output(&stdout))
}

pub(crate) fn parse_stm32flash_output(s: &str) -> Stm32FlashInfo {
    // Typical successful output (one field per line, ':' separator):
    //   Version      : 0x22
    //   Option 1     : 0x00
    //   Option 2     : 0x00
    //   Device ID    : 0x0414 (STM32F10xxx High-density)
    //   - RAM        : Up to 64KiB  (12288b reserved by bootloader)
    //   - Flash      : Up to 512KiB (size first sector: 4x2048)
    //   - Option RAM : 16b
    //   - System RAM : 2KiB
    let mut info = Stm32FlashInfo::default();
    for raw in s.lines() {
        // Strip leading '- ' (used for RAM/Flash/Option RAM/System RAM rows).
        let line = raw.trim().trim_start_matches('-').trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let val = v.trim().to_string();
            match k.trim() {
                "Version" => info.version = Some(val),
                "Option 1" => info.option_1 = Some(val),
                "Option 2" => info.option_2 = Some(val),
                "Device ID" => info.device_id = Some(val),
                "RAM" => info.ram = Some(val),
                "Flash" => info.flash = Some(val),
                "Option RAM" => info.option_ram = Some(val),
                "System RAM" => info.system_ram = Some(val),
                _ => {}
            }
        }
    }
    info
}
