use crate::*;

/// Parsed avrdude `-v` output for a synced AVR target. `requested_mcu` records
/// the `-p` guess (kept for context; not printed); the rest are read back.
#[derive(Default)]
pub(crate) struct AvrInfo {
    pub(crate) requested_mcu: Option<String>,
    pub(crate) detected_mcu: Option<String>,
    pub(crate) signature: Option<String>,
    pub(crate) sig_matches: Option<String>,
    pub(crate) programmer: Option<String>,
    pub(crate) bootloader: Option<String>,
    pub(crate) hw_version: Option<String>,
    pub(crate) fw_version: Option<String>,
    pub(crate) modes: Option<String>,
}

impl AvrInfo {
    pub(crate) fn is_empty(&self) -> bool {
        self.requested_mcu.is_none()
            && self.detected_mcu.is_none()
            && self.signature.is_none()
            && self.programmer.is_none()
            && self.bootloader.is_none()
    }

    pub(crate) fn print(&self, board_name: &str) {
        println!("  {:<20} {}", "Board:", board_name);
        let rows: [(&str, &Option<String>); 8] = [
            ("Detected MCU", &self.detected_mcu),
            ("Signature", &self.signature),
            ("Sig matches", &self.sig_matches),
            ("Programmer", &self.programmer),
            ("Bootloader", &self.bootloader),
            ("HW Version", &self.hw_version),
            ("FW Version", &self.fw_version),
            ("Modes", &self.modes),
        ];
        for (k, v) in rows {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{k}:"), v);
            }
        }
        if let Some(mcu) = &self.detected_mcu {
            let summary = avr_chip_summary(mcu.as_str());
            if !summary.is_empty() {
                println!("  {:<20} {}", "Summary:", summary);
            }
        }
        println!(
            "  {:<20} (fuses require an ISP programmer on the ICSP header)",
            "Fuses:"
        );
    }
}

pub(crate) fn run_avrdude_query(port: &str, targets: &[AvrTarget]) -> Result<AvrInfo> {
    // Serial bootloader programmers (arduino/wiring/avr109/stk500v1/stk500v2)
    // can read flash/eeprom/signature but NOT fuses — fuse reads return 0
    // silently. Skip fuse reads here; only an ISP programmer on the ICSP
    // header can read them.
    //
    // `targets` holds one or more (MCU, programmer, bauds) profiles to try in
    // order — the first that syncs wins. A board on a dedicated Arduino VID
    // passes exactly one; a board behind a generic USB-UART bridge passes
    // several, because the bridge can't reveal whether a Uno-class (STK500v1
    // "arduino") or a Mega-class (STK500v2 "wiring") MCU is on its lines.
    //
    // `-F` overrides avrdude's signature check: when the actual MCU differs
    // from `-p` (a 328PB / 168 / LGT8F328P clone), the run still succeeds and
    // reports the true `Device signature` instead of bailing. `-F` only
    // relaxes the post-connect check — a board that never syncs (wrong baud,
    // wrong bootloader dialect, or no Arduino at all) still fails cleanly, so
    // chaining mismatched profiles produces no false positives. Probing is
    // read-only here (no -U), so overriding the check has no side effects.
    let mut last_err = String::from("avrdude produced no output");

    for target in targets {
        for &baud in target.bauds {
            let baud_str = baud.to_string();
            let output = Command::new("avrdude")
                .args([
                    "-c", target.programmer,
                    "-p", target.mcu,
                    "-P", port,
                    "-b", &baud_str,
                    "-F",
                    "-v",
                ])
                .output()
                .context("avrdude not found on PATH (install via Arduino IDE, PlatformIO, or scoop install avrdude)")?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                return Ok(parse_avrdude_output(&stdout, &stderr, target.mcu));
            }
            last_err = summarize_avrdude_error(&stderr, target, baud);
        }
    }
    anyhow::bail!("{last_err}");
}

pub(crate) fn summarize_avrdude_error(stderr: &str, target: &AvrTarget, baud: u32) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let last = lines.last().copied().unwrap_or("unknown");
    // Name the attempt (programmer + baud) so a multi-target probe makes clear
    // which bootloader dialect failed. A signature mismatch (wrong -p) is the
    // most actionable failure, but the "Device signature = ..." line isn't the
    // last one avrdude prints — pull it out so the caller can tell a wrong MCU
    // guess from a baud/wiring problem.
    let attempt = format!("{} @ {baud} baud", target.programmer);
    match lines.iter().rev().find(|l| l.contains("Device signature")) {
        Some(sig) if *sig != last => format!("{attempt} — {sig}; {last}"),
        _ => format!("{attempt} — {last}"),
    }
}

pub(crate) fn parse_avrdude_output(_stdout: &str, stderr: &str, requested_mcu: &str) -> AvrInfo {
    let mut info = AvrInfo {
        requested_mcu: Some(requested_mcu.to_string()),
        ..Default::default()
    };

    for raw in stderr.lines() {
        let line = raw.trim_end();
        // Extract the value after a "Prefix ... :" label, if non-empty.
        let field = |prefix: &str| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.find(':').map(|idx| rest[idx + 1..].trim().to_string()))
                .filter(|v| !v.is_empty())
        };
        if let Some(v) = field("AVR part") {
            info.detected_mcu = Some(v);
        }
        if let Some(v) = field("Programmer type") {
            info.programmer = Some(v);
        }
        if let Some(v) = field("Description") {
            info.bootloader = Some(v);
        }
        if let Some(v) = field("HW Version") {
            info.hw_version = Some(v);
        }
        if let Some(v) = field("FW Version") {
            info.fw_version = Some(v);
        }
        if let Some(v) = field("Programming modes") {
            info.modes = Some(v);
        }

        // "Device signature = 1E 95 0F (ATmega328P, ATA6614Q, LGT8F328P)"
        if let Some(rest) = line.strip_prefix("Device signature = ") {
            let (sig, alt) = match rest.split_once(" (") {
                Some((s, a)) => (s.trim(), a.trim_end_matches(')').trim()),
                None => (rest.trim(), ""),
            };
            info.signature = Some(sig.to_string());
            if !alt.is_empty() {
                info.sig_matches = Some(alt.to_string());
            }
        }
    }
    info
}

pub(crate) fn avr_chip_summary(mcu: &str) -> &'static str {
    match mcu.to_ascii_lowercase().as_str() {
        "atmega328p" | "atmega328" => "32 KB flash, 2 KB SRAM, 1 KB EEPROM, 16 MHz",
        "atmega2560" => "256 KB flash, 8 KB SRAM, 4 KB EEPROM, 16 MHz",
        "atmega32u4" => "32 KB flash, 2.5 KB SRAM, 1 KB EEPROM, 16 MHz, native USB",
        "atmega168" => "16 KB flash, 1 KB SRAM, 512 B EEPROM, 16 MHz",
        _ => "",
    }
}
