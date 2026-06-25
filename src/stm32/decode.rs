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

/// A downstream SWD target report (ST-Link or CMSIS-DAP layer 2). All rows are
/// optional: the probe-level rows describe the debug port, the identity rows the
/// decoded MCU. `device_id.is_none()` marks a one-layer (no-target) result.
#[derive(Default)]
pub(crate) struct TargetReport {
    // layer-1 probe rows
    pub(crate) probe: Option<String>,
    pub(crate) dp_idcode: Option<String>,
    pub(crate) layer2: Option<String>,
    pub(crate) fix: Option<String>,
    pub(crate) hint: Option<String>,
    // layer-2 identity rows
    pub(crate) device_id: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) vendor: Option<String>,
    pub(crate) core: Option<String>,
    pub(crate) architecture: Option<String>,
    pub(crate) max_clock: Option<String>,
    pub(crate) flash_size: Option<String>,
    pub(crate) flash_map: Option<String>,
    pub(crate) sram: Option<String>,
    pub(crate) unique_id: Option<String>,
    pub(crate) read_protection: Option<String>,
    pub(crate) identification: Option<String>,
    pub(crate) transport: Option<String>,
    pub(crate) access: Option<String>,
    pub(crate) source: Option<String>,
}

impl TargetReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.probe.is_none()
            && self.dp_idcode.is_none()
            && self.layer2.is_none()
            && self.fix.is_none()
            && self.hint.is_none()
            && self.device_id.is_none()
    }

    pub(crate) fn print(&self, board: &str) {
        // Probe-level rows (present once the debug interface opens).
        if let Some(v) = &self.probe {
            println!("  {:<16} {v}", "Probe:");
        }
        if let Some(v) = &self.dp_idcode {
            println!("  {:<16} {v}", "DP IDCODE:");
        }

        // No downstream target: one-layer result. Show the reason and stop.
        if self.device_id.is_none() {
            match &self.layer2 {
                Some(state) => println!("  Layer 2:         {state}"),
                None => println!("  Layer 2:         none - no SWD/JTAG target detected"),
            }
            if let Some(fix) = &self.fix {
                println!("  Fix:             {fix}");
            }
            if let Some(hint) = &self.hint {
                println!("  Hint:            {hint}");
            }
            return;
        }

        // Two-layer result: a target answered. Print its read-only identity.
        println!("  Layer 2 - downstream SWD target (read-only) via {board}:");
        for (key, val) in self.identity_fields() {
            if let Some(v) = val {
                println!("    {:<16} {v}", format!("{key}:"));
            }
        }
    }

    /// The layer-2 identity rows paired with their labels, in display order.
    fn identity_fields(&self) -> [(&'static str, &Option<String>); 15] {
        [
            ("Device ID", &self.device_id),
            ("Revision", &self.revision),
            ("Vendor", &self.vendor),
            ("Core", &self.core),
            ("Architecture", &self.architecture),
            ("Max clock", &self.max_clock),
            ("Flash size", &self.flash_size),
            ("Flash map", &self.flash_map),
            ("SRAM", &self.sram),
            ("Unique ID", &self.unique_id),
            ("Read protection", &self.read_protection),
            ("Identification", &self.identification),
            ("Transport", &self.transport),
            ("Access", &self.access),
            ("Source", &self.source),
        ]
    }

    /// Present identity rows as `(label, value)` in display order. Used by the
    /// `--mcu-alive-native` step printer, which renders them in its own format.
    pub(crate) fn identity_rows(&self) -> Vec<(&'static str, &str)> {
        self.identity_fields()
            .into_iter()
            .filter_map(|(k, v)| v.as_deref().map(|s| (k, s)))
            .collect()
    }
}

/// Decode the read-only identity registers into a `TargetReport` (layer-2 fields
/// only; the caller fills probe-level rows). The shape `format_target_rows`
/// augments and `TargetReport::print` renders.
pub(crate) fn decode_stlink_regs(regs: &HashMap<u32, Vec<u32>>, dev_id: Option<u16>) -> TargetReport {
    let mut info = TargetReport::default();
    let first = |addr: u32| regs.get(&addr).and_then(|w| w.first()).copied();

    // ── Core (CPUID is universal across ARMv6-M/ARMv7-M) ──
    if let Some(cpuid) = first(0xE000ED00) {
        info.core = Some(cortex_core(cpuid));
    }

    let fam = dev_id.and_then(stm_family);
    if dev_id == Some(0x421) {
        info.architecture = Some("Armv7E-M, Thumb-2, DSP, single-precision FPU".to_string());
        info.max_clock = Some("180 MHz".to_string());
        info.sram = Some("128 KB".to_string());
        info.identification = Some(
            "DBGMCU DEV_ID identifies STM32F446 family; package suffix not readable over SWD"
                .to_string(),
        );
    }
    info.access = Some("Read-only identity registers".to_string());
    info.transport = Some("SWD at 100 kHz".to_string());

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
            info.device_id = Some(format!("0x{did:03X} — {name}"));
            info.max_clock = Some(format!(
                "{max_mhz} MHz (GigaDevice GD32; ST's F103 tops out at 72 MHz)"
            ));
        } else {
            let name = fam.map(|f| f.name).unwrap_or("unknown (unrecognized STM32 DEV_ID)");
            info.device_id = Some(format!("0x{did:03X} — {name}"));
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
        info.revision = Some(format!("0x{rev_id:04X}{rev}"));

        // Provenance note: firm for a recognized GD32, generic for any other
        // STM32-compatible clone (known DEV_ID, REV_ID outside ST's set).
        if gd32.is_some() {
            info.vendor = Some(format!(
                "GigaDevice GD32 — pin-compatible with ST's STM32F103 but NOT genuine ST silicon. \
                 DEV_ID 0x{did:03X} mirrors ST; REV_ID 0x{rev_id:04X} (outside ST's set) confirms GD32. \
                 Package/pin (C=48 R=64 V=100 Z=144) + temp grade are not SWD-readable."
            ));
        } else if let Some(revs) = stm32_known_revs(did) {
            if !revs.contains(&rev_id) {
                info.vendor = Some(format!(
                    "likely an STM32-compatible clone (GigaDevice GD32 / CKS / APM32 / MindMotion) — \
                     REV_ID 0x{rev_id:04X} is not a documented ST revision for DEV_ID 0x{did:03X}"
                ));
            }
        }
    }

    // ── Flash size / UID / RDP — only readable once we know the family ──
    if let Some(fam) = fam {
        if let Some(words) = regs.get(&fam.flash_size_addr) {
            let kb = words[0] & 0xFFFF;
            // 0x0000 / 0xFFFF mean the field is unprogrammed or unreadable.
            if kb != 0 && kb != 0xFFFF {
                info.flash_size = Some(format!("{kb} KB"));
                info.flash_map =
                    Some(format!("0x08000000-0x{:08X}", 0x08000000u32 + kb * 1024 - 1));
            }
        }

        if let Some(w) = regs.get(&fam.uid_addr) {
            // All-0xFF means the read faulted (wrong address / locked), not a
            // real UID — skip it rather than print a bogus serial.
            let blank = w.iter().take(3).all(|&x| x == 0xFFFFFFFF);
            if w.len() >= 3 && !blank {
                // 96-bit UID, printed most-significant word first.
                info.unique_id = Some(format!("{:08X} {:08X} {:08X}", w[2], w[1], w[0]));
            }
        }

        if let Some(opt) = first(fam.rdp_addr) {
            info.read_protection = Some(decode_rdp(opt, fam.rdp_kind));
        }
    }

    info
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


#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn layer2_decoder_identifies_stm32f446_without_writing_target() {
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x410FC241]);
        regs.insert(0xE0042000, vec![0x10000421]);
        regs.insert(0x1FFF7A22, vec![512]);
        regs.insert(0x1FFF7A10, vec![0x11223344, 0x55667788, 0x99AABBCC]);
        regs.insert(0x40023C14, vec![0x0000AA00]);

        let info = decode_stlink_regs(&regs, Some(0x421));

        assert!(info.device_id.as_deref().unwrap().contains("STM32F446"));
        assert!(info.core.as_deref().unwrap().contains("Cortex-M4"));
        assert_eq!(info.flash_size.as_deref(), Some("512 KB"));
        assert_eq!(info.flash_map.as_deref(), Some("0x08000000-0x0807FFFF"));
        assert!(info.read_protection.as_deref().unwrap().contains("Disabled"));
        assert_eq!(info.access.as_deref(), Some("Read-only identity registers"));
    }

    #[test]
    fn native_dbgmcu_idcode_decodes_to_stm32_family() {
        // What --mcu-alive-native feeds decode_stlink_regs after reading DBGMCU.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x410FC241]); // CPUID: Cortex-M4
        regs.insert(0xE0042000, vec![0x10010421]); // DBGMCU: DEV_ID 0x421 = F446
        let info = decode_stlink_regs(&regs, Some(0x421));
        assert!(info.device_id.as_deref().unwrap().contains("STM32F446"));
        assert!(info.core.as_deref().unwrap().contains("Cortex-M4"));
    }

    #[test]
    fn native_stm32f103_target_decodes_as_layer2() {
        // A "Blue Pill" STM32F103C8 hanging off the probe (ST-Link or the RP2040
        // CMSIS-DAP single-drop path): DEV_ID 0x410, Cortex-M3, 64 KB flash.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]); // CPUID: Cortex-M3
        regs.insert(0xE0042000, vec![0x20036410]); // DBGMCU: DEV_ID 0x410, REV_ID 0x2003
        regs.insert(0x1FFFF7E0, vec![64]); // F1 flash-size register: 64 KB
        let info = decode_stlink_regs(&regs, Some(0x410));
        assert!(info.device_id.as_deref().unwrap().contains("STM32F1 medium-density"));
        assert!(info.core.as_deref().unwrap().contains("Cortex-M3"));
        assert_eq!(info.flash_size.as_deref(), Some("64 KB"));
        assert!(info.revision.as_deref().unwrap().contains("rev 1/2/3/X/Y"));
        assert!(info.vendor.is_none()); // genuine ST REV_ID → no clone flag
    }

    #[test]
    fn gd32f103ret6_identified_as_gigadevice_not_stm32() {
        // GD32F103RET6 mirrors ST's high-density DEV_ID 0x414 but reports a
        // REV_ID (0x1309) outside ST's {0x1000,0x1001,0x1003} set; with 512 KB
        // flash it resolves to the GD32F103xE density, named as GigaDevice.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]); // Cortex-M3 r2p1
        regs.insert(0xE0042000, vec![0x13090414]); // DBGMCU: DEV_ID 0x414, REV_ID 0x1309
        regs.insert(0x1FFFF7E0, vec![512]); // F1 high-density flash-size word: 512 KB
        let info = decode_stlink_regs(&regs, Some(0x414));
        // Leads with the GD32 model, NOT the ST family name.
        let device_id = info.device_id.as_deref().unwrap();
        assert!(device_id.contains("GD32F103xE"));
        assert!(device_id.contains("GD32F103RET6"));
        assert!(!device_id.contains("STM32"));
        assert!(info.max_clock.as_deref().unwrap().contains("108 MHz"));
        let vendor = info.vendor.as_deref().expect("GD32 provenance noted");
        assert!(vendor.contains("GigaDevice") && vendor.contains("0x1309"));

        // A genuine high-density REV_ID must stay STM32 with no GD32 claim.
        regs.insert(0xE0042000, vec![0x10000414]);
        let genuine = decode_stlink_regs(&regs, Some(0x414));
        assert!(genuine.device_id.as_deref().unwrap().contains("STM32F1 high-density"));
        assert!(genuine.vendor.is_none());
    }
}
