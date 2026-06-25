use crate::*;

/// Decode an Arm Cortex-M `CPUID` (0xE000ED00) into a "core rNpM (CPUID …)"
/// string. Used by both the pyocd-backed and native SWD paths.
pub(crate) fn cortex_core(cpuid: u32) -> String {
    let partno = (cpuid >> 4) & 0xFFF;
    let variant = (cpuid >> 20) & 0xF;
    let revision = cpuid & 0xF;
    let core = match partno {
        0xC20 => "Cortex-M0",
        0xC60 => "Cortex-M0+",
        0xC21 => "Cortex-M1",
        0xC23 => "Cortex-M3",
        0xC24 => "Cortex-M4",
        0xC27 => "Cortex-M7",
        0xD20 => "Cortex-M23",
        0xD21 => "Cortex-M33",
        _ => "Cortex-M (unknown)",
    };
    format!("{core} r{variant}p{revision} (CPUID 0x{cpuid:08X})")
}

/// Decode a flash read-protection register value into a human description.
pub(crate) fn decode_rdp(opt: u32, kind: RdpKind) -> String {
    match kind {
        RdpKind::Obr => {
            let mut s = if (opt >> 1) & 1 == 1 {
                "Enabled — flash read-protected (RDP active)".to_string()
            } else {
                "Disabled — flash readable (RDP Level 0)".to_string()
            };
            if opt & 1 == 1 {
                s.push_str(", OPTERR set");
            }
            s
        }
        RdpKind::OptByte => {
            let rdp = (opt >> 8) & 0xFF;
            match rdp {
                0xAA => "Disabled — flash readable (RDP Level 0)".to_string(),
                0xCC => "Enabled — RDP Level 2 (permanent, debug locked)".to_string(),
                _ => format!("Enabled — RDP Level 1 (RDP byte 0x{rdp:02X})"),
            }
        }
    }
}

pub(crate) fn decode_stlink_regs(regs: &HashMap<u32, Vec<u32>>, dev_id: Option<u16>) -> HashMap<String, String> {
    let mut info = HashMap::new();
    let first = |addr: u32| regs.get(&addr).and_then(|w| w.first()).copied();

    // ── Core (CPUID is universal across ARMv6-M/ARMv7-M) ──
    if let Some(cpuid) = first(0xE000ED00) {
        info.insert("Core".to_string(), cortex_core(cpuid));
    }

    let fam = dev_id.and_then(stm_family);
    if dev_id == Some(0x421) {
        info.insert(
            "Architecture".to_string(),
            "Armv7E-M, Thumb-2, DSP, single-precision FPU".to_string(),
        );
        info.insert("Max clock".to_string(), "180 MHz".to_string());
        info.insert("SRAM".to_string(), "128 KB".to_string());
        info.insert(
            "Identification".to_string(),
            "DBGMCU DEV_ID identifies STM32F446 family; package suffix not readable over SWD"
                .to_string(),
        );
    }
    info.insert("Access".to_string(), "Read-only identity registers".to_string());
    info.insert("Transport".to_string(), "SWD at 100 kHz".to_string());

    // ── Device ID + revision ──
    if let Some(idcode) = first(0xE0042000) {
        let did = (idcode & 0xFFF) as u16;
        let rev_id = ((idcode >> 16) & 0xFFFF) as u16;
        // Flash size (KB) is decoded again below for the Flash rows, but GD32
        // model resolution needs it now to pick the density letter.
        let flash_kb = fam
            .and_then(|f| regs.get(&f.flash_size_addr))
            .and_then(|w| w.first().copied())
            .map(|v| v & 0xFFFF)
            .filter(|&kb| kb != 0 && kb != 0xFFFF);

        // GigaDevice GD32 mirrors ST's DEV_ID; when the REV_ID proves it's a
        // clone we lead with the GD32 model, not the ST family name.
        let gd32 = gd32_identify(did, rev_id, flash_kb);
        if let Some((name, _core, max_mhz)) = &gd32 {
            info.insert("Device ID".to_string(), format!("0x{did:03X} — {name}"));
            info.insert(
                "Max clock".to_string(),
                format!("{max_mhz} MHz (GigaDevice GD32; ST's F103 tops out at 72 MHz)"),
            );
        } else {
            let name = fam.map(|f| f.name).unwrap_or("unknown (unrecognized STM32 DEV_ID)");
            info.insert("Device ID".to_string(), format!("0x{did:03X} — {name}"));
        }

        // REV_ID → silicon-revision letter is only well-defined for genuine ST
        // parts (GD32 REV_IDs don't follow ST's scheme); cover ST's 0x410 here.
        let rev = if gd32.is_none() && did == 0x410 {
            match rev_id {
                0x0000 => " (rev A)",
                0x2000 => " (rev B)",
                0x2001 => " (rev Z)",
                0x2003 => " (rev 1/2/3/X/Y)",
                _ => "",
            }
        } else {
            ""
        };
        info.insert("Revision".to_string(), format!("0x{rev_id:04X}{rev}"));

        // Provenance note: firm for a recognized GD32, generic for any other
        // STM32-compatible clone (known DEV_ID, REV_ID outside ST's set).
        if gd32.is_some() {
            info.insert(
                "Vendor".to_string(),
                format!(
                    "GigaDevice GD32 — pin-compatible with ST's STM32F103 but NOT genuine ST silicon. \
                     DEV_ID 0x{did:03X} mirrors ST; REV_ID 0x{rev_id:04X} (outside ST's set) confirms GD32. \
                     Package/pin (C=48 R=64 V=100 Z=144) + temp grade are not SWD-readable."
                ),
            );
        } else if let Some(revs) = stm32_known_revs(did) {
            if !revs.contains(&rev_id) {
                info.insert(
                    "Vendor".to_string(),
                    format!(
                        "likely an STM32-compatible clone (GigaDevice GD32 / CKS / APM32 / MindMotion) — \
                         REV_ID 0x{rev_id:04X} is not a documented ST revision for DEV_ID 0x{did:03X}"
                    ),
                );
            }
        }
    }

    // ── Flash size / UID / RDP — only readable once we know the family ──
    if let Some(fam) = fam {
        if let Some(words) = regs.get(&fam.flash_size_addr) {
            let kb = words[0] & 0xFFFF;
            // 0x0000 / 0xFFFF mean the field is unprogrammed or unreadable.
            if kb != 0 && kb != 0xFFFF {
                info.insert("Flash size".to_string(), format!("{kb} KB"));
                info.insert(
                    "Flash map".to_string(),
                    format!("0x08000000-0x{:08X}", 0x08000000u32 + kb * 1024 - 1),
                );
            }
        }

        if let Some(w) = regs.get(&fam.uid_addr) {
            // All-0xFF means the read faulted (wrong address / locked), not a
            // real UID — skip it rather than print a bogus serial.
            let blank = w.iter().take(3).all(|&x| x == 0xFFFFFFFF);
            if w.len() >= 3 && !blank {
                // 96-bit UID, printed most-significant word first.
                info.insert(
                    "Unique ID".to_string(),
                    format!("{:08X} {:08X} {:08X}", w[2], w[1], w[0]),
                );
            }
        }

        if let Some(opt) = first(fam.rdp_addr) {
            info.insert("Read protection".to_string(), decode_rdp(opt, fam.rdp_kind));
        }
    }

    info
}

pub(crate) fn print_stlink_target_info(info: &HashMap<String, String>, board: &str) {
    // Probe-level keys (always present once the debug interface opens).
    for key in ["Probe", "DP IDCODE"] {
        if let Some(value) = info.get(key) {
            println!("  {:<16} {value}", format!("{key}:"));
        }
    }

    // No downstream target: one-layer result. Show the reason and stop.
    if !info.contains_key("Device ID") {
        if let Some(state) = info.get("Layer 2") {
            println!("  Layer 2:         {state}");
        } else {
            println!("  Layer 2:         none - no SWD/JTAG target detected");
        }
        if let Some(fix) = info.get("Fix") {
            println!("  Fix:             {fix}");
        }
        if let Some(hint) = info.get("Hint") {
            println!("  Hint:            {hint}");
        }
        return;
    }

    // Two-layer result: a target answered. Print its read-only identity.
    println!("  Layer 2 - downstream SWD target (read-only) via {board}:");
    for key in [
        "Device ID", "Revision", "Vendor", "Core", "Architecture", "Max clock", "Flash size",
        "Flash map", "SRAM", "Unique ID", "Read protection", "Identification",
        "Transport", "Access", "Source",
    ] {
        if let Some(v) = info.get(key) {
            println!("    {:<16} {v}", format!("{key}:"));
        }
    }
}

pub(crate) fn microbit_identify(unique_id: &str) -> Option<(&'static str, &'static str)> {
    // First 4 hex digits of the Unique ID identify the board hardware revision
    // (this prefix is assigned by Microbit Foundation / DAPLink board database).
    let prefix = unique_id.get(..4)?;
    let nrf51 = "nRF51822 — Cortex-M0, 16 KB SRAM, 256 KB flash, 16 MHz, BLE 4.0";
    let nrf52 = "nRF52833 — Cortex-M4F, 128 KB SRAM, 512 KB flash, 64 MHz, BLE 5.x";
    Some(match prefix {
        "9900" => ("BBC micro:bit V1.3",          nrf51),
        "9901" => ("BBC micro:bit V1.5",          nrf51),
        "9903" => ("BBC micro:bit V2.0",          nrf52),
        "9904" => ("BBC micro:bit V2.21",         nrf52),
        "9905" => ("BBC micro:bit V2.21 (later)", nrf52),
        "9906" => ("BBC micro:bit V2.x",          nrf52),
        _ => return None,
    })
}

