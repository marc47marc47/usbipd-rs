use crate::*;

pub(crate) fn is_stlink_device(device: &nusb::DeviceInfo) -> bool {
    device.vendor_id() == 0x0483 && (0x3748..=0x3757).contains(&device.product_id())
}
pub(crate) fn cmd_mcu_alive() -> Result<()> {
    let probes: Vec<nusb::DeviceInfo> = nusb::list_devices()
        .wait()
        .context("failed to enumerate USB hardware through nusb")?
        .filter(is_stlink_device)
        .collect();

    println!("=== Minimal MCU Alive Test ===");
    println!("Safety: no target reset, halt, flash read/write, erase, or unlock is requested.");
    println!("The SWD discovery scan may reset the SWD debug link state.");

    if probes.is_empty() {
        println!("Step 1 - USB probe: FAIL - no ST-Link USB device found");
        println!("Step 2 - Debug interface: SKIP");
        println!("Step 3 - Target SWD response: SKIP");
        return Ok(());
    }

    for probe in probes {
        let vid = probe.vendor_id();
        let pid = probe.product_id();
        let serial = probe.serial_number().unwrap_or("");
        let selector = if serial.is_empty() {
            format!("{vid:04x}:{pid:04x}")
        } else {
            format!("{vid:04x}:{pid:04x}:{serial}")
        };

        println!("\n[{selector}]");
        println!(
            "Step 1 - USB probe: PASS - product {:?}, serial {:?}, speed {:?}",
            probe.product_string(),
            probe.serial_number(),
            probe.speed()
        );

        if let Some(StlinkDriver::Other(state)) = stlink_driver_check(pid) {
            println!("Step 2 - Debug interface: FAIL - {state}");
            println!("Step 3 - Target SWD response: SKIP - no host-to-probe command path");
            if let Some(advice) =
                known_driver_advice(&format!("USB\\VID_{vid:04X}&PID_{pid:04X}&MI_00"))
            {
                println!("Required driver binding: {}", advice.name);
                println!("Install: {}", advice.install);
                report_local_driver_availability(&advice);
            }
            continue;
        }

        println!("Step 2 - Debug interface: attempting read-only open through probe-rs");
        let output = Command::new("probe-rs")
            .args([
                "info",
                "--probe",
                &selector,
                "--protocol",
                "swd",
                "--speed",
                "100",
                "--non-interactive",
            ])
            .output()
            .context("probe-rs not found on PATH (install with: cargo install probe-rs-tools)")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            println!("Step 2 - Debug interface: PASS - ST-Link accepted commands");
            println!("Step 3 - Target SWD response: PASS - probe-rs discovered a target at 100 kHz");
            for line in stdout
                .lines()
                .chain(stderr.lines())
                .map(str::trim)
                .filter(|line| {
                    !line.is_empty()
                        && !line.starts_with('-')
                        && !line.eq_ignore_ascii_case("Probing target via SWD")
                })
                .take(12)
            {
                println!("  {line}");
            }
        } else {
            let error = stderr
                .lines()
                .chain(stdout.lines())
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('-'))
                .last()
                .unwrap_or("probe-rs could not complete SWD discovery");
            let opened = !error.to_ascii_lowercase().contains("driver")
                && !error.to_ascii_lowercase().contains("open the debug probe")
                && !error.to_ascii_lowercase().contains("usb error");
            println!(
                "Step 2 - Debug interface: {}",
                if opened {
                    "PASS - probe opened; SWD stage failed"
                } else {
                    "FAIL - probe could not be opened"
                }
            );
            println!("Step 3 - Target SWD response: FAIL - {error}");
        }
    }
    Ok(())
}
pub(crate) fn cmd_mcu_alive_native() -> Result<()> {
    let probes: Vec<nusb::DeviceInfo> = nusb::list_devices()
        .wait()
        .context("failed to enumerate USB hardware through nusb")?
        .filter(is_stlink_device)
        .collect();

    println!("=== Native ST-Link SWD Probe (no probe-rs / pyocd / stlink) ===");
    println!("Transport: raw USB bulk to MI_00 via nusb. On Windows this needs WinUSB on MI_00 (see --install-driver).");
    println!("Safety: only ENTER_SWD + read-only ID-register reads are issued — no halt, reset, erase, or memory write.");

    let chip_db = load_chip_db();
    if chip_db.is_empty() {
        println!("Chip data: built-in family table only (drop etc/chips/*.chip files to extend/override).");
    } else {
        println!("Chip data: {} external .chip definition(s) loaded; they take priority over the built-in table.", chip_db.len());
    }

    if probes.is_empty() {
        println!("Step 1 - USB probe: FAIL - no ST-Link USB device found");
        return Ok(());
    }

    for probe in probes {
        let vid = probe.vendor_id();
        let pid = probe.product_id();
        let serial = probe.serial_number().unwrap_or("");
        println!(
            "\n[{vid:04x}:{pid:04x}{}] {}",
            if serial.is_empty() { String::new() } else { format!(":{serial}") },
            probe.product_string().unwrap_or("ST-Link")
        );

        let mut link = match StlinkLink::open_device(&probe) {
            Ok(l) => {
                println!("Step 1/2 - open + claim MI_00: PASS - bulk transfers available");
                l
            }
            Err(e) => {
                println!("Step 1/2 - open + claim MI_00: FAIL - {e}");
                println!("  No-driver boundary: descriptors are readable, transfers are not.");
                println!("  Minimal fix (WinUSB on MI_00 only — no vendor driver / no probe-rs):");
                println!("    usbipd-rs --install-driver --confirm   (run in an Administrator shell)");
                continue;
            }
        };

        match link.get_version() {
            Ok(v) => println!("Step 3 - probe firmware: PASS - {v}"),
            Err(e) => {
                println!("Step 3 - probe firmware: FAIL - {e}");
                continue;
            }
        }

        match link.get_voltage_mv() {
            Ok(mv) => println!("Step 4 - target voltage: {:.2} V", mv as f64 / 1000.0),
            Err(e) if e.to_string().contains("stall") => {
                println!("Step 4 - target voltage: n/a (not reported by this ST-Link)")
            }
            Err(e) => println!("Step 4 - target voltage: FAIL - {e}"),
        }

        let status = match link.enter_swd() {
            Ok(s) => s,
            Err(e) => {
                println!("Step 5 - enter SWD: FAIL - {e}");
                continue;
            }
        };
        if status == STLINK_JTAG_OK {
            println!("Step 5 - enter SWD: PASS (status 0x80, core not halted)");
        } else {
            println!("Step 5 - enter SWD: WARN - status 0x{status:02X} (no target on SWD / wrong wiring?); continuing read attempts");
        }

        // ── Layer 2: read-only target identity ──
        match link.read_idcode() {
            Ok(dpidr) => println!("Step 6 - DP IDCODE (DPIDR): 0x{dpidr:08X}"),
            Err(e) => println!("Step 6 - DP IDCODE: FAIL - {e}"),
        }

        let (regs, dev_id, resolved) = stlink_read_regs(&mut link, &chip_db);
        let rows = format_target_rows(&regs, dev_id, resolved.as_ref());
        println!("Step 7 - target identity (read-only):");
        if rows.is_empty() {
            println!("  (no identity registers read — no SWD target answered)");
            println!("  Hint: {}", swd_no_target_hint(&mut link));
        } else {
            for (k, v) in rows {
                println!("  {:<18} {v}", format!("{k}:"));
            }
        }
    }
    Ok(())
}

// ============================================================================
// Tool installer (--install / --list-tools)
// ============================================================================

