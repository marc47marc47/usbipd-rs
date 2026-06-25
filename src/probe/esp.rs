use crate::*;

pub(crate) fn run_espflash_board_info(port: &str) -> Result<HashMap<String, String>> {
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

pub(crate) fn parse_espflash_output(s: &str) -> HashMap<String, String> {
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

pub(crate) fn strip_log_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(close) = rest.find(']') {
            return rest[close + 1..].trim_start();
        }
    }
    line
}

pub(crate) fn print_esp_info(info: &HashMap<String, String>) {
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
