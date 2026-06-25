use crate::*;

/// Layer-2 probe: read the downstream SWD target's identity natively (read-only,
/// via `nusb` — no probe-rs/pyocd, no halt/reset). Returns a `HashMap` keyed for
/// `print_stlink_target_info`. Auto-detects whether a target is present: with no
/// SWD response only the probe-level keys are returned (one-layer result).
pub(crate) fn run_stlink_target_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let mut info = HashMap::new();

    let probe = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid));
    let Some(probe) = probe else {
        info.insert("Layer 2".into(), "SKIP - ST-Link USB device not found".into());
        return Ok(info);
    };

    let mut link = match StlinkLink::open_device(&probe) {
        Ok(l) => l,
        Err(e) => {
            info.insert("Layer 2".into(), format!("SKIP - cannot open debug interface: {e}"));
            #[cfg(windows)]
            info.insert(
                "Fix".into(),
                "bind WinUSB to MI_00: usbipd-rs --install-driver --confirm (Administrator)".into(),
            );
            return Ok(info);
        }
    };

    if let Ok(v) = link.get_version() {
        let volt = link
            .get_voltage_mv()
            .map(|mv| format!(", target {:.2} V", mv as f64 / 1000.0))
            .unwrap_or_default();
        info.insert("Probe".into(), format!("{v}{volt}"));
    }

    let status = link.enter_swd().unwrap_or(0);
    match link.read_idcode() {
        Ok(dpidr) => {
            info.insert("DP IDCODE".into(), format!("0x{dpidr:08X}"));
        }
        Err(_) => {
            info.insert(
                "Layer 2".into(),
                format!("none - SWD target did not answer (enter-SWD status 0x{status:02X})"),
            );
            info.insert("Hint".into(), swd_no_target_hint(&mut link));
            return Ok(info);
        }
    }

    let db = load_chip_db();
    let (regs, dev_id, resolved) = stlink_read_regs(&mut link, &db);
    for (k, v) in format_target_rows(&regs, dev_id, resolved.as_ref()) {
        info.insert(k.to_string(), v);
    }
    Ok(info)
}

// ============================================================================
// Optional external chip database (stlink-style `etc/chips/*.chip` files)
//
// The built-in `stm_family` table is the always-available base library. A
// `.chip` file lets a user add or override a model WITHOUT recompiling: a model
// with a matching `.chip` takes priority; otherwise the built-in table is used.
// The format is a subset of stlink-org/stlink's, so real stlink chip files drop
// in unmodified (unknown keys are ignored).
// ============================================================================

