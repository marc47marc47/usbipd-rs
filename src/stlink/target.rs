use crate::*;

/// Layer-2 probe: read the downstream SWD target's identity natively (read-only,
/// via `nusb` — no probe-rs/pyocd, no halt/reset). Returns a `HashMap` keyed for
/// `print_stlink_target_info`. Auto-detects whether a target is present: with no
/// SWD response only the probe-level keys are returned (one-layer result).
pub(crate) fn run_stlink_target_query(vid: u16, pid: u16) -> Result<TargetReport> {
    let mut report = TargetReport::default();

    let probe = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid));
    let Some(probe) = probe else {
        report.layer2 = Some("SKIP - ST-Link USB device not found".into());
        return Ok(report);
    };

    let mut link = match StlinkLink::open_device(&probe) {
        Ok(l) => l,
        Err(e) => {
            report.layer2 = Some(format!("SKIP - cannot open debug interface: {e}"));
            #[cfg(windows)]
            {
                report.fix = Some(
                    "bind WinUSB to MI_00: usbipd-rs --install-driver --confirm (Administrator)"
                        .into(),
                );
            }
            return Ok(report);
        }
    };

    if let Ok(v) = link.get_version() {
        let volt = link
            .get_voltage_mv()
            .map(|mv| format!(", target {:.2} V", mv as f64 / 1000.0))
            .unwrap_or_default();
        report.probe = Some(format!("{v}{volt}"));
    }

    let status = link.enter_swd().unwrap_or(0);
    match link.read_idcode() {
        Ok(dpidr) => {
            report.dp_idcode = Some(format!("0x{dpidr:08X}"));
        }
        Err(_) => {
            report.layer2 = Some(format!(
                "none - SWD target did not answer (enter-SWD status 0x{status:02X})"
            ));
            report.hint = Some(swd_no_target_hint(&mut link));
            return Ok(report);
        }
    }

    let db = load_chip_db();
    let (regs, dev_id, resolved) = stlink_read_regs(&mut link, &db);
    let identity = format_target_rows(&regs, dev_id, resolved.as_ref());
    Ok(TargetReport {
        probe: report.probe,
        dp_idcode: report.dp_idcode,
        ..identity
    })
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

