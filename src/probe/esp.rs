use crate::*;

/// Parsed `espflash board-info` output. Every field is optional — espflash only
/// prints what it could read from the chip.
#[derive(Default)]
pub(crate) struct EspInfo {
    pub(crate) chip_type: Option<String>,
    pub(crate) chip_revision: Option<String>,
    pub(crate) crystal_frequency: Option<String>,
    pub(crate) flash_size: Option<String>,
    pub(crate) features: Option<String>,
    pub(crate) mac_address: Option<String>,
    pub(crate) mac: Option<String>,
}

impl EspInfo {
    pub(crate) fn is_empty(&self) -> bool {
        self.chip_type.is_none()
            && self.chip_revision.is_none()
            && self.crystal_frequency.is_none()
            && self.flash_size.is_none()
            && self.features.is_none()
            && self.mac_address.is_none()
            && self.mac.is_none()
    }

    pub(crate) fn print(&self) {
        let rows: [(&str, &Option<String>); 7] = [
            ("Chip type", &self.chip_type),
            ("Chip revision", &self.chip_revision),
            ("Crystal frequency", &self.crystal_frequency),
            ("Flash size", &self.flash_size),
            ("Features", &self.features),
            ("MAC address", &self.mac_address),
            ("MAC", &self.mac),
        ];
        for (k, v) in rows {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{k}:"), v);
            }
        }
    }
}

pub(crate) fn run_espflash_board_info(port: &str) -> Result<EspInfo> {
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

pub(crate) fn parse_espflash_output(s: &str) -> EspInfo {
    let mut info = EspInfo::default();
    for raw in s.lines() {
        let line = strip_log_prefix(raw).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let val = v.trim().to_string();
            match k.trim() {
                "Chip type" => info.chip_type = Some(val),
                "Chip revision" => info.chip_revision = Some(val),
                "Crystal frequency" => info.crystal_frequency = Some(val),
                "Flash size" => info.flash_size = Some(val),
                "Features" => info.features = Some(val),
                "MAC address" => info.mac_address = Some(val),
                "MAC" => info.mac = Some(val),
                _ => {}
            }
        }
    }
    info
}

pub(crate) fn strip_log_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(close) = rest.find(']') {
            return rest[close + 1..].trim_start();
        }
    }
    line
}
