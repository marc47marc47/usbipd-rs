use crate::*;

/// A read-only downstream-target link: anything that can read a 32-bit word
/// from the target's address space over SWD. Implemented by both `StlinkLink`
/// (ST-Link READDEBUGREG) and `CmsisDapLink` (CMSIS-DAP MEM-AP), so the layer-2
/// register collection below is written once against the trait.
pub(crate) trait TargetLink {
    fn read_word(&mut self, addr: u32) -> Result<u32>;
}

/// After ENTER_SWD, read the read-only identity registers into a map keyed by
/// address (the shape `decode_stlink_regs` consumes), and resolve the chip
/// (a matching `.chip` file in `db` wins over the built-in table). Returns
/// `(regs, dev_id, resolved)`.
pub(crate) fn stlink_read_regs(
    link: &mut StlinkLink,
    db: &[ChipDef],
) -> (HashMap<u32, Vec<u32>>, Option<u16>, Option<ResolvedChip>) {
    collect_target_regs(link, db)
}

/// Read the read-only identity registers through any `TargetLink`, resolve the
/// chip (a matching `.chip` file wins), and return `(regs, dev_id, resolved)`.
pub(crate) fn collect_target_regs(
    link: &mut dyn TargetLink,
    db: &[ChipDef],
) -> (HashMap<u32, Vec<u32>>, Option<u16>, Option<ResolvedChip>) {
    let mut regs: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(cpuid) = link.read_word(0xE000ED00) {
        regs.insert(0xE000ED00, vec![cpuid]);
    }
    // DBGMCU_IDCODE: 0xE0042000 on Cortex-M3/M4/M7 STM32, 0x40015800 on M0.
    let mut idcode = None;
    for &addr in &[0xE0042000u32, 0x40015800u32] {
        if let Ok(v) = link.read_word(addr) {
            if (v & 0xFFF) != 0 && (v & 0xFFF) != 0xFFF {
                regs.insert(0xE0042000, vec![v]);
                idcode = Some(v);
                break;
            }
        }
    }
    let dev_id = idcode.map(|v| (v & 0xFFF) as u16);
    let resolved = dev_id.and_then(|d| resolve_chip(d, db));
    if let Some(r) = resolved.as_ref() {
        // Flash size is a 16-bit field that may be non-word-aligned (0x1FFF7A22
        // on F4/F7); 32-bit reads are word-aligned, so read the containing word
        // and keep the correct half in the low 16 bits.
        if let Some(fsa) = r.flash_size_addr {
            if let Ok(word) = link.read_word(fsa & !0x3) {
                regs.insert(fsa, vec![(word >> ((fsa & 0x3) * 8)) & 0xFFFF]);
            }
        }
        if let Some(uid_addr) = r.uid_addr {
            let mut uid = Vec::new();
            for k in 0..3 {
                match link.read_word(uid_addr + k * 4) {
                    Ok(v) => uid.push(v),
                    Err(_) => break,
                }
            }
            if uid.len() == 3 {
                regs.insert(uid_addr, uid);
            }
        }
        if let Some(rdp_addr) = r.rdp_addr {
            if let Ok(v) = link.read_word(rdp_addr) {
                regs.insert(rdp_addr, vec![v]);
            }
        }
    }
    (regs, dev_id, resolved)
}

/// Build ordered (key, value) identity rows from decoded registers, applying
/// `.chip` overrides (name, SRAM, source) and a flash fallback for parts the
/// built-in family table doesn't cover.
pub(crate) fn format_target_rows(
    regs: &HashMap<u32, Vec<u32>>,
    dev_id: Option<u16>,
    resolved: Option<&ResolvedChip>,
) -> TargetReport {
    let mut report = decode_stlink_regs(regs, dev_id);
    if let Some(r) = resolved {
        if let Some(did) = dev_id {
            // `decode_stlink_regs` already named GD32 clones (which mirror this
            // DEV_ID) as GigaDevice parts — don't clobber that with the generic
            // ST family name from the built-in table / .chip override.
            let decoded_is_gd32 = report.device_id.as_deref().is_some_and(|d| d.contains("GD32"));
            if !decoded_is_gd32 {
                report.device_id = Some(format!("0x{:03X} — {}", did, r.name));
            }
        }
        if let Some(kb) = r.sram_kb {
            report.sram = Some(format!("{kb} KB"));
        }
        // Flash + RDP fallback for a model the built-in table doesn't cover
        // (decode only reads those for built-in families).
        if report.flash_size.is_none() {
            if let Some(kb) = r.flash_size_addr.and_then(|a| regs.get(&a)).and_then(|w| w.first()).copied() {
                if kb != 0 && kb != 0xFFFF {
                    report.flash_size = Some(format!("{kb} KB"));
                    report.flash_map = Some(format!("0x08000000-0x{:08X}", 0x08000000u32 + kb * 1024 - 1));
                }
            }
        }
        if report.read_protection.is_none() {
            if let (Some(addr), Some(kind)) = (r.rdp_addr, r.rdp_kind) {
                if let Some(opt) = regs.get(&addr).and_then(|w| w.first()).copied() {
                    report.read_protection = Some(decode_rdp(opt, kind));
                }
            }
        }
        report.source = Some(r.source.clone());
    }
    // The native path doesn't set a fixed SWD clock; correct decode's default.
    if report.transport.is_some() {
        report.transport = Some("SWD (native nusb, read-only)".into());
    }
    // The rows path historically omits the Identification row (kept only on the
    // RP2040 path); preserve that.
    report.identification = None;
    report
}

/// Actionable checklist when SWD enters but no target answers (DPIDR fails).
/// Reads the probe's sense of target voltage — an unreadable / 0 V reading is a
/// strong sign the target isn't powered or VTREF isn't wired.
pub(crate) fn swd_no_target_hint(link: &mut StlinkLink) -> String {
    let volt = link
        .get_voltage_mv()
        .map(|mv| format!("{:.2} V", mv as f64 / 1000.0))
        .unwrap_or_else(|_| "unreadable".into());
    format!(
        "probe-sensed target voltage: {volt}. Check: target is powered; ST-Link VTREF(3V3)/SWDIO/SWCLK/GND all wired; \
         NRST not held low; using the SWD (not JTAG) header. On a Nucleo driven by an EXTERNAL ST-Link, remove the two \
         CN2 (ST-LINK) jumpers to disconnect the on-board ST-Link, and power the board (USB or E5V)."
    )
}

pub mod stlink;
pub mod cmsisdap;
pub(crate) use stlink::*;
pub(crate) use cmsisdap::*;
