
/// Where a given STM32 family keeps its flash-size word, 96-bit UID, and
/// read-out-protection option register. DBGMCU_IDCODE (0xE0042000) and CPUID
/// (0xE000ED00) are at fixed addresses across families, but these three move,
/// so we can only read them once DEV_ID tells us the family.
#[derive(Clone, Copy)]
pub(crate) struct StmFamily {
    name: &'static str,
    flash_size_addr: u32,
    uid_addr: u32,
    rdp_addr: u32,
    rdp_kind: RdpKind,
}

#[derive(Clone, Copy)]
pub(crate) enum RdpKind {
    /// FLASH_OBR with RDPRT in bit 1 (STM32F0/F1/F3).
    Obr,
    /// FLASH_OPTCR-style register with the RDP level byte in bits [15:8]
    /// (STM32F2/F4/F7): 0xAA = Level 0, 0xCC = Level 2, anything else = Level 1.
    OptByte,
}

// Per-family register address groups.
pub(crate) const ADDRS_F1:   (u32, u32, u32, RdpKind) = (0x1FFFF7E0, 0x1FFFF7E8, 0x4002201C, RdpKind::Obr);
pub(crate) const ADDRS_F0F3: (u32, u32, u32, RdpKind) = (0x1FFFF7CC, 0x1FFFF7AC, 0x4002201C, RdpKind::Obr);
pub(crate) const ADDRS_F2F4: (u32, u32, u32, RdpKind) = (0x1FFF7A22, 0x1FFF7A10, 0x40023C14, RdpKind::OptByte);
pub(crate) const ADDRS_F7:   (u32, u32, u32, RdpKind) = (0x1FF0F442, 0x1FF0F420, 0x40023C14, RdpKind::OptByte);

/// Map a 12-bit DBGMCU DEV_ID to its family name + register addresses.
pub(crate) fn stm_family(dev_id: u16) -> Option<StmFamily> {
    let (name, (flash_size_addr, uid_addr, rdp_addr, rdp_kind)) = match dev_id {
        // ── STM32F0 (Cortex-M0) ──
        0x440 => ("STM32F030x8 / F05x — Cortex-M0", ADDRS_F0F3),
        0x442 => ("STM32F030xC / F09x — Cortex-M0", ADDRS_F0F3),
        0x444 => ("STM32F03x — Cortex-M0", ADDRS_F0F3),
        0x445 => ("STM32F04x — Cortex-M0", ADDRS_F0F3),
        0x448 => ("STM32F07x — Cortex-M0", ADDRS_F0F3),
        // ── STM32F1 (Cortex-M3) ──
        0x412 => ("STM32F1 low-density (F101/102/103) — Cortex-M3", ADDRS_F1),
        0x410 => ("STM32F1 medium-density (F101/102/103) — Cortex-M3", ADDRS_F1),
        0x414 => ("STM32F1 high-density (F101/103) — Cortex-M3", ADDRS_F1),
        0x418 => ("STM32F1 connectivity line (F105/107) — Cortex-M3", ADDRS_F1),
        0x420 => ("STM32F100 medium-density value line — Cortex-M3", ADDRS_F1),
        0x428 => ("STM32F100 high-density value line — Cortex-M3", ADDRS_F1),
        0x430 => ("STM32F1 XL-density (F101/103) — Cortex-M3", ADDRS_F1),
        // ── STM32F2 (Cortex-M3) ──
        0x411 => ("STM32F2 — Cortex-M3", ADDRS_F2F4),
        // ── STM32F3 (Cortex-M4F) ──
        0x422 => ("STM32F302xB/C / F303xB/C / F358 — Cortex-M4F", ADDRS_F0F3),
        0x432 => ("STM32F373 / F378 — Cortex-M4F", ADDRS_F0F3),
        0x438 => ("STM32F303x4/6/8 / F334 / F328 — Cortex-M4F", ADDRS_F0F3),
        0x439 => ("STM32F301 / F302x6/8 / F318 — Cortex-M4F", ADDRS_F0F3),
        0x446 => ("STM32F302xD/E / F303xD/E / F398 — Cortex-M4F", ADDRS_F0F3),
        // ── STM32F4 (Cortex-M4F) ──
        0x413 => ("STM32F405/407/415/417 — Cortex-M4F", ADDRS_F2F4),
        0x419 => ("STM32F42x/43x — Cortex-M4F", ADDRS_F2F4),
        0x423 => ("STM32F401xB/C — Cortex-M4F", ADDRS_F2F4),
        0x431 => ("STM32F411 — Cortex-M4F", ADDRS_F2F4),
        0x433 => ("STM32F401xD/E — Cortex-M4F", ADDRS_F2F4),
        0x441 => ("STM32F412 — Cortex-M4F", ADDRS_F2F4),
        0x421 => ("STM32F446 — Cortex-M4F", ADDRS_F2F4),
        0x434 => ("STM32F469/479 — Cortex-M4F", ADDRS_F2F4),
        0x458 => ("STM32F410 — Cortex-M4F", ADDRS_F2F4),
        0x463 => ("STM32F413/423 — Cortex-M4F", ADDRS_F2F4),
        // ── STM32F7 (Cortex-M7) ──
        0x449 => ("STM32F74x/75x — Cortex-M7", ADDRS_F7),
        0x451 => ("STM32F76x/77x — Cortex-M7", ADDRS_F7),
        0x452 => ("STM32F72x/73x — Cortex-M7", ADDRS_F7),
        _ => return None,
    };
    Some(StmFamily { name, flash_size_addr, uid_addr, rdp_addr, rdp_kind })
}

/// Documented genuine-ST silicon REV_IDs (DBGMCU IDCODE bits [31:16]) for the
/// families most often cloned. STM32-compatible parts from GigaDevice (GD32),
/// CKS, APM32 (Geehy), MindMotion, etc. mirror ST's DEV_ID but report a REV_ID
/// outside ST's published set — so a known DEV_ID with an unknown REV_ID is a
/// strong "not genuine ST" signal. Returns `None` for families we don't track
/// closely enough to second-guess (then we make no clone claim).
pub(crate) fn stm32_known_revs(dev_id: u16) -> Option<&'static [u16]> {
    Some(match dev_id {
        0x412 => &[0x1000],                          // F1 low-density
        0x410 => &[0x0000, 0x2000, 0x2001, 0x2003],  // F1 medium-density
        0x414 => &[0x1000, 0x1001, 0x1003],          // F1 high-density
        0x418 => &[0x1000, 0x1001],                  // F1 connectivity line
        0x420 => &[0x1000, 0x1001],                  // F100 value line, MD
        0x428 => &[0x1000, 0x1001],                  // F100 value line, HD
        0x430 => &[0x1000],                          // F1 XL-density
        _ => return None,
    })
}

/// GigaDevice GD32 lines that mirror an ST DBGMCU DEV_ID (so they would decode
/// as STM32 by IDCODE alone). Keyed by the mirrored DEV_ID → (family, core, max
/// MHz). GD32 reports the SAME DEV_ID as the ST part it replaces, but a REV_ID
/// outside ST's published set (see `stm32_known_revs`) and a faster top clock
/// (108 MHz on the F1 line vs ST's 72 MHz). The package/pin letter (C=48, R=64,
/// V=100, Z=144) and temperature grade are NOT exposed over SWD, so the density
/// from the flash-size word is the finest model the silicon will tell us.
pub(crate) const GD32_FAMILIES: &[(u16, &str, &str, u16)] = &[
    (0x410, "GD32F103", "Cortex-M3", 108), // medium-density mirror (≤128 KB)
    (0x414, "GD32F103", "Cortex-M3", 108), // high-density mirror (256–512 KB)
    (0x418, "GD32F105", "Cortex-M3", 108), // connectivity-line mirror
    (0x430, "GD32F103", "Cortex-M3", 108), // XL-density mirror
];

/// Density letter for a GD32F10x from its flash size in KB (x4=16 … xE=512).
pub(crate) fn gd32_density(flash_kb: Option<u32>) -> &'static str {
    match flash_kb {
        Some(k) if k >= 512 => "xE",
        Some(k) if k >= 384 => "xD",
        Some(k) if k >= 256 => "xC",
        Some(k) if k >= 128 => "xB",
        Some(k) if k >= 64 => "x8",
        Some(k) if k >= 32 => "x6",
        Some(k) if k >= 16 => "x4",
        _ => "",
    }
}

/// Resolve a GigaDevice GD32 model from the read-only signals: a known ST DEV_ID
/// reporting a non-ST REV_ID (the clone signature) selects the family, and the
/// flash size selects the density. Returns `(display_name, core, max_mhz)`, or
/// `None` when this isn't a recognized GD32 (then the STM32 name/clone-flag path
/// runs). The canonical 64-pin part is cited as an example for the F103 line;
/// the exact package can't be read over SWD.
pub(crate) fn gd32_identify(dev_id: u16, rev_id: u16, flash_kb: Option<u32>) -> Option<(String, &'static str, u16)> {
    // Must look like a clone first: ST DEV_ID we know, REV_ID ST never shipped.
    match stm32_known_revs(dev_id) {
        Some(revs) if !revs.contains(&rev_id) => {}
        _ => return None,
    }
    let &(_, family, core, max_mhz) = GD32_FAMILIES.iter().find(|&&(id, ..)| id == dev_id)?;
    let density = gd32_density(flash_kb);
    let example = if family == "GD32F103" {
        match density {
            "xE" => " — e.g. GD32F103RET6 (64-pin, 512 KB)",
            "xD" => " — e.g. GD32F103RDT6 (384 KB)",
            "xC" => " — e.g. GD32F103RCT6 (256 KB)",
            "xB" => " — e.g. GD32F103CBT6 (128 KB)",
            "x8" => " — e.g. GD32F103C8T6 (64 KB)",
            "x6" => " — e.g. GD32F103C6T6 (32 KB)",
            "x4" => " — e.g. GD32F103C4T6 (16 KB)",
            _ => "",
        }
    } else {
        ""
    };
    Some((format!("{family}{density} (GigaDevice){example} — {core}"), core, max_mhz))
}


pub mod chipdb;
pub mod decode;
pub(crate) use chipdb::*;
pub(crate) use decode::*;

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn gd32_density_maps_flash_to_letter() {
        assert_eq!(gd32_density(Some(512)), "xE");
        assert_eq!(gd32_density(Some(256)), "xC");
        assert_eq!(gd32_density(Some(64)), "x8");
        assert_eq!(gd32_density(None), "");
        // Non-clone REV_ID → no GD32 identity even on a GD32 DEV_ID.
        assert!(gd32_identify(0x414, 0x1000, Some(512)).is_none());
        // Clone REV_ID on an untracked DEV_ID → no model (generic clone flag only).
        assert!(gd32_identify(0x412, 0x9999, Some(64)).is_none());
    }
}
