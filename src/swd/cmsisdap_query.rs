use crate::*;

/// Layer 1: identify an RP2040-based CMSIS-DAP debug probe from VID:PID + USB
/// descriptors (no SWD command issued). Returns a `StlinkControllerInfo`.
pub(crate) fn run_cmsisdap_controller_query(vid: u16, pid: u16) -> Result<StlinkControllerInfo> {
    let generation = match pid {
        0x000c => "Raspberry Pi Debug Probe (debugprobe firmware)",
        0x0004 => "picoprobe (Pico-as-probe firmware)",
        _ => "RP2040 CMSIS-DAP probe",
    };
    let mut info = StlinkControllerInfo {
        layer: Some("1 - USB debug probe (CMSIS-DAP)".into()),
        generation: Some(generation.into()),
        controller_mcu: Some("RP2040 (dual Arm Cortex-M0+)".into()),
        core: Some("2x Arm Cortex-M0+".into()),
        architecture: Some("Armv6-M, Thumb/Thumb-2 subset".into()),
        max_clock: Some("133 MHz".into()),
        flash: Some("External QSPI (e.g. 2 MB on a Pico)".into()),
        sram: Some("264 KB".into()),
        upstream: Some("USB 2.0 Full Speed (CMSIS-DAP v2 bulk + CDC UART)".into()),
        downstream: Some("SWD (+ UART bridge)".into()),
        usb_identity: Some(format!("{vid:04x}:{pid:04x}")),
        identification: Some("VID:PID identifies an RP2040 CMSIS-DAP probe".into()),
        layer2: Some("Auto-probed read-only below (native CMSIS-DAP)".into()),
        ..Default::default()
    };
    if let Some(dev) = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid))
    {
        info.descriptor_access = Some("Available through nusb".into());
        if let Some(p) = dev.product_string() {
            info.usb_product = Some(p.to_string());
        }
        if let Some(s) = dev.serial_number() {
            info.usb_serial = Some(s.to_string());
        }
    }
    Ok(info)
}

/// Layer 2: read the downstream SWD target through the CMSIS-DAP probe natively.
/// Returns a `TargetReport`; auto-detects target presence.
pub(crate) fn run_cmsisdap_target_query(vid: u16, pid: u16) -> Result<TargetReport> {
    let mut report = TargetReport::default();
    let probe = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut it| it.find(|d| d.vendor_id() == vid && d.product_id() == pid));
    let Some(probe) = probe else {
        report.layer2 = Some("SKIP - CMSIS-DAP USB device not found".into());
        return Ok(report);
    };

    let mut link = match CmsisDapLink::open_device(&probe) {
        Ok(l) => l,
        Err(e) => {
            report.layer2 = Some(format!("SKIP - cannot open CMSIS-DAP interface: {e}"));
            #[cfg(windows)]
            {
                report.fix = Some(
                    "bind WinUSB to the 'CMSIS-DAP v2' interface with Zadig (usbipd-rs --install zadig)".into(),
                );
            }
            return Ok(report);
        }
    };
    report.probe = Some("CMSIS-DAP v2 (native nusb)".into());

    let multidrop = match link.bringup() {
        Ok((dpidr, multidrop)) => {
            report.dp_idcode = Some(format!(
                "0x{dpidr:08X}{}",
                if multidrop { " (SWD multidrop)" } else { "" }
            ));
            multidrop
        }
        Err(e) => {
            report.layer2 = Some(format!("none - SWD bring-up failed: {e}"));
            report.hint = Some(
                "CMSIS-DAP probe present but no target answered. Check: target powered; SWDIO/SWCLK/GND wired to the probe; correct SWD pins; NRST not held low.".into(),
            );
            return Ok(report);
        }
    };
    if let Err(e) = link.open_mem_ap() {
        report.layer2 = Some(format!("partial - DP up but MEM-AP failed: {e}"));
        return Ok(report);
    }

    // A multidrop DP is an RP-class part (RP2040 DPIDR 0x0BC12477): identify it
    // from SYSINFO CHIP_ID + CPUID rather than the STM32 DBGMCU path.
    if multidrop {
        match link.read_mem32(RP2040_SYSINFO_CHIP_ID) {
            Ok(chip_id) => {
                let identity = rp2040_rows(&mut link, chip_id, multidrop);
                report = TargetReport {
                    probe: report.probe,
                    dp_idcode: report.dp_idcode,
                    ..identity
                };
            }
            Err(e) => {
                report.layer2 = Some(format!("partial - DP/AP up but MEM read failed: {e}"));
            }
        }
        return Ok(report);
    }

    let db = load_chip_db();
    let (regs, dev_id, resolved) = collect_target_regs(&mut link, &db);
    if dev_id.is_none() {
        // The DP answered (we printed a DP IDCODE) but no identity register could
        // be read — the AHB-AP faulted every access. Report this distinctly
        // instead of falling through to the generic "no target detected": a debug
        // port DID respond, the memory bus just didn't. Capture the live fault.
        let why = link
            .read_mem32(0xE000ED00)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "AP read returned no usable value".into());
        report.layer2 = Some(format!(
            "target present but unreadable — Arm SW-DP answered, memory/AP access faulted ({why})"
        ));
        report.hint = Some(
            "An Arm debug port responded but its memory bus did not (SWD ACK=FAULT). Likely causes: NRST held low \
             (target stuck in reset); target not fully powered (wire VTREF/3V3 + GND); on a Nucleo driven by an \
             EXTERNAL probe, remove the two CN2 (ST-LINK) jumpers so the on-board ST-LINK stops contending, then power \
             the board (USB/E5V); SWDIO/SWCLK swapped; or the chip has debug permanently disabled (STM32 RDP level 2)."
                .into(),
        );
        return Ok(report);
    }
    let identity = format_target_rows(&regs, dev_id, resolved.as_ref());
    Ok(TargetReport {
        probe: report.probe,
        dp_idcode: report.dp_idcode,
        ..identity
    })
}

/// Format an RP2040 downstream target's read-only identity from its SYSINFO
/// CHIP_ID (manufacturer 0x927, part 0x0002) + GITREF + CPUID.
pub(crate) fn rp2040_rows(link: &mut CmsisDapLink, chip_id: u32, multidrop: bool) -> TargetReport {
    let part = (chip_id >> 12) & 0xFFFF;
    let revision = (chip_id >> 28) & 0xF;
    let name = if part == 0x0002 { "RP2040" } else { "Raspberry Pi silicon" };
    let stepping = match (part, revision) {
        (0x0002, 1) => " (B0/B1)",
        (0x0002, 2) => " (B2)",
        _ => "",
    };
    let mfr = chip_id & 0xFFF;
    let mut report = TargetReport {
        device_id: Some(format!(
            "{name}{stepping} — Raspberry Pi (mfr 0x{mfr:03X}, part 0x{part:04X})"
        )),
        revision: Some(format!("0x{revision:X}")),
        flash_size: Some("external QSPI (not read over SWD)".to_string()),
        sram: Some("264 KB".to_string()),
        transport: Some(format!(
            "SWD{} (native CMSIS-DAP, read-only)",
            if multidrop { " multidrop" } else { "" }
        )),
        access: Some("Read-only identity registers".to_string()),
        ..Default::default()
    };
    if let Ok(cpuid) = link.read_mem32(0xE000ED00) {
        report.core = Some(format!("{} (dual-core; core 0 selected)", cortex_core(cpuid)));
    }
    if let Ok(gitref) = link.read_mem32(RP2040_SYSINFO_GITREF) {
        report.identification = Some(format!(
            "bootrom GITREF 0x{gitref:08X}, CHIP_ID 0x{chip_id:08X}"
        ));
    }
    report
}
