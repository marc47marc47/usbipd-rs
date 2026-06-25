use crate::*;


// ============================================================================
// Native CMSIS-DAP v2 reader (RP2040 Debug Probe / picoprobe — layer 2)
//
// Speaks the CMSIS-DAP v2 bulk protocol directly over nusb to read the
// downstream SWD target's identity read-only (DPIDR, then CPUID / DBGMCU /
// flash / UID / RDP via the MEM-AP). No probe-rs/pyocd. Brings up SWD and reads
// ID registers only — no halt, reset, erase, or write. The DAP_Transfer request
// byte is APnDP(bit0) | RnW(bit1) | (regaddr & 0x0C); reads pull the result on
// the next transfer (DRW then RDBUFF), per the SWD posted-read pipeline.
// ============================================================================

pub(crate) const DAP_CONNECT: u8 = 0x02;
pub(crate) const DAP_TRANSFER_CONFIGURE: u8 = 0x04;
pub(crate) const DAP_TRANSFER: u8 = 0x05;
pub(crate) const DAP_SWJ_CLOCK: u8 = 0x11;
pub(crate) const DAP_SWJ_SEQUENCE: u8 = 0x12;
pub(crate) const DAP_SWD_CONFIGURE: u8 = 0x13;

// RP2040 is a SWD multidrop (DPv2) target with two cores. TARGETSEL for core 0;
// SYSINFO CHIP_ID/GITREF identify the chip + stepping over the MEM-AP.
pub(crate) const RP2040_TARGETSEL_CORE0: u32 = 0x01002927;
pub(crate) const RP2040_TARGETSEL_CORE1: u32 = 0x11002927;
pub(crate) const RP2040_SYSINFO_CHIP_ID: u32 = 0x40000000;
pub(crate) const RP2040_SYSINFO_GITREF: u32 = 0x40000014;
// The RP2040's fixed DPIDR (DPv2, designer 0x927, partno 0xC1). Used to accept a
// multidrop bring-up by exact match — see `bringup()` for why a version-only test
// is unsafe when a single-drop STM32 (F103 etc.) hangs off the same probe.
pub(crate) const RP2040_DPIDR: u32 = 0x0BC12477;

pub(crate) struct CmsisDapLink {
    _device: nusb::Device,
    _iface: nusb::Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
}

impl CmsisDapLink {
    /// Open the probe's CMSIS-DAP v2 vendor interface and its bulk endpoints.
    fn open_device(probe: &nusb::DeviceInfo) -> Result<Self> {
        // Prefer the vendor interface whose string names CMSIS-DAP (the v2 one
        // with bulk endpoints); fall back to the first vendor (0xFF) interface.
        let iface_num = probe
            .interfaces()
            .find(|i| {
                i.class() == 0xff
                    && i.interface_string()
                        .is_some_and(|s| s.to_ascii_uppercase().contains("CMSIS-DAP"))
            })
            .or_else(|| probe.interfaces().find(|i| i.class() == 0xff))
            .map(|i| i.interface_number())
            .context("no CMSIS-DAP vendor interface found")?;

        let device = probe.open().wait().context("open USB device")?;
        let iface = device
            .claim_interface(iface_num)
            .wait()
            .with_context(|| format!("claim CMSIS-DAP interface MI_{iface_num:02} (needs WinUSB on Windows)"))?;

        let config = device.active_configuration().context("read active configuration")?;
        let alt = config
            .interface_alt_settings()
            .find(|a| a.interface_number() == iface_num && a.alternate_setting() == 0)
            .context("interface alt setting 0 not found")?;
        // The CMSIS-DAP v2 interface carries only bulk endpoints (OUT, IN, and
        // optionally a SWO IN). Take the first OUT and first IN by address.
        let mut ep_out_addr = None;
        let mut ep_in_addr = None;
        for ep in alt.endpoints() {
            let a = ep.address();
            if a & 0x80 == 0 {
                ep_out_addr.get_or_insert(a);
            } else {
                ep_in_addr.get_or_insert(a);
            }
        }
        let ep_out_addr = ep_out_addr.context("no bulk OUT endpoint")?;
        let ep_in_addr = ep_in_addr.context("no bulk IN endpoint")?;
        let out = iface
            .endpoint::<Bulk, Out>(ep_out_addr)
            .map_err(|e| anyhow::anyhow!("open OUT endpoint 0x{ep_out_addr:02x}: {e}"))?;
        let inp = iface
            .endpoint::<Bulk, In>(ep_in_addr)
            .map_err(|e| anyhow::anyhow!("open IN endpoint 0x{ep_in_addr:02x}: {e}"))?;
        Ok(Self { _device: device, _iface: iface, ep_out: out, ep_in: inp })
    }

    /// Send a CMSIS-DAP v2 command (raw bytes, no report id) and read the reply.
    /// Validates the echoed command id in byte 0.
    fn command(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let timeout = std::time::Duration::from_millis(1000);
        self.ep_out
            .transfer_blocking(Buffer::from(payload.to_vec()), timeout)
            .into_result()
            .map_err(|e| anyhow::anyhow!("DAP OUT failed: {e}"))?;
        let resp = self
            .ep_in
            .transfer_blocking(Buffer::new(64), timeout)
            .into_result()
            .map_err(|e| anyhow::anyhow!("DAP IN failed: {e}"))?
            .into_vec();
        if resp.first().copied() != payload.first().copied() {
            anyhow::bail!(
                "DAP response id 0x{:02X} != command 0x{:02X}",
                resp.first().copied().unwrap_or(0),
                payload.first().copied().unwrap_or(0)
            );
        }
        Ok(resp)
    }

    fn connect_swd(&mut self) -> Result<()> {
        let r = self.command(&[DAP_CONNECT, 0x01])?;
        if r.get(1).copied() != Some(0x01) {
            anyhow::bail!("DAP_Connect SWD not accepted (resp {:?})", r.get(1));
        }
        Ok(())
    }

    fn swj_clock(&mut self, hz: u32) -> Result<()> {
        let b = hz.to_le_bytes();
        self.command(&[DAP_SWJ_CLOCK, b[0], b[1], b[2], b[3]])?;
        Ok(())
    }

    fn swj_sequence(&mut self, bits: u8, data: &[u8]) -> Result<()> {
        let mut payload = vec![DAP_SWJ_SEQUENCE, bits];
        payload.extend_from_slice(data);
        self.command(&payload)?;
        Ok(())
    }

    /// SWD line reset + JTAG-to-SWD switch + line reset + idle. Read-only.
    fn line_reset_and_switch(&mut self) -> Result<()> {
        self.swj_sequence(51, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07])?; // >=50 clocks high
        self.swj_sequence(16, &[0x9E, 0xE7])?; // JTAG-to-SWD magic 0xE79E
        self.swj_sequence(51, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07])?; // line reset
        self.swj_sequence(8, &[0x00])?; // idle
        Ok(())
    }

    /// One single-word DAP_Transfer. `req` is the request byte; `write` carries
    /// data for writes. Returns the 32-bit value for reads.
    fn transfer(&mut self, req: u8, write: Option<u32>) -> Result<u32> {
        let mut payload = vec![DAP_TRANSFER, 0x00, 0x01, req]; // DAP index 0, count 1
        if let Some(v) = write {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let resp = self.command(&payload)?;
        let count = resp.get(1).copied().unwrap_or(0);
        let ack = resp.get(2).copied().unwrap_or(0) & 0x07;
        if count != 1 || ack != 1 {
            anyhow::bail!("DAP_Transfer ack 0x{ack:02X} (count {count})");
        }
        if write.is_some() {
            Ok(0)
        } else {
            le_u32(&resp, 3).context("short DAP_Transfer read")
        }
    }

    fn dp_read(&mut self, addr: u8) -> Result<u32> {
        self.transfer(0x02 | (addr & 0x0C), None)
    }
    fn dp_write(&mut self, addr: u8, v: u32) -> Result<u32> {
        self.transfer(addr & 0x0C, Some(v))
    }
    fn ap_read(&mut self, addr: u8) -> Result<u32> {
        self.transfer(0x03 | (addr & 0x0C), None)
    }
    fn ap_write(&mut self, addr: u8, v: u32) -> Result<u32> {
        self.transfer(0x01 | (addr & 0x0C), Some(v))
    }

    /// Put the DP into dormant then wake it to SWD, matching probe-rs's RP2040
    /// path: line reset, JTAG-to-dormant (0x33BBBBBA), the 128-bit leave-dormant
    /// selection alert, then the 8-bit SWD activation code. All raw SWJ bit
    /// sequences — no DP access, no reset/halt of the core. The caller does one
    /// more line reset right before TARGETSEL.
    fn dormant_to_swd(&mut self) -> Result<()> {
        self.swj_sequence(54, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07])?; // line reset 51 high + 3 idle
        self.swj_sequence(31, &0x33BB_BBBAu32.to_le_bytes())?; // JTAG-to-dormant
        self.swj_sequence(8, &[0xFF])?; // >=8 cycles high
        self.swj_sequence(64, &[0x92, 0xF3, 0x09, 0x62, 0x95, 0x2D, 0x85, 0x86])?; // alert [0:63]
        self.swj_sequence(64, &[0xE9, 0xAF, 0xDD, 0xE3, 0xA2, 0x0E, 0xBC, 0x19])?; // alert [64:127]
        self.swj_sequence(12, &[0xA0, 0x01])?; // 4 low cycles + 8-bit SWD activation code 0x1A
        Ok(())
    }

    /// Line reset (51 high + 3 idle) immediately followed by the TARGETSEL
    /// packet, emitted as ONE DAP_SWJ_Sequence so no probe-side gap can fall
    /// between them (TARGETSEL must be the first packet after a line reset).
    fn line_reset_then_targetsel(&mut self, targetsel: u32) -> Result<()> {
        let parity = (targetsel.count_ones() % 2) as u128;
        let ts = (parity << 45) | ((targetsel as u128) << 13) | 0x1f99;
        let line_reset: u128 = 0x0007_FFFF_FFFF_FFFF; // 51 ones in a 54-bit field
        let combined = (ts << 54) | line_reset; // 54-bit reset, then 48-bit TARGETSEL
        self.swj_sequence(102, &combined.to_le_bytes()[..13])
    }

    /// One multidrop attempt: reset, select `targetsel`, read DPIDR. `dormant`
    /// chooses the dormant-to-SWD alert vs a plain line reset for the reset step.
    fn try_multidrop(&mut self, dormant: bool, targetsel: u32) -> Result<u32> {
        if dormant {
            self.dormant_to_swd()?;
        }
        self.line_reset_then_targetsel(targetsel)?; // contiguous reset + TARGETSEL
        let dpidr = self.dp_read(0x0)?;
        let _ = self.dp_write(0x0, 0x1E); // clear ABORT sticky (ORUNERR after reset)
        Ok(dpidr)
    }

    /// Bring up SWD and return `(DPIDR, multidrop)`. A DPv1 part that answers a
    /// plain DPIDR read is single-drop (STM32/GD32). The RP2040 is a DPv2 multidrop
    /// part that shares the bus with its sibling core and needs a TARGETSEL to
    /// route AP accesses. Tries the single-drop path FIRST (with a few retries, so a
    /// flaky dupont link gets more than one shot) — it sends no TARGETSEL and so
    /// can't disturb a DPv2 target — and accepts only a DPv1 (version < 2) DPIDR.
    /// Only if that fails does it try line-reset+TARGETSEL (then dormant->SWD),
    /// accepting multidrop only on an EXACT RP2040 DPIDR match. A version-only
    /// (>= 2) test is unsafe: a contended/floating bus — or an STM32 that ignores
    /// the TARGETSEL write — can set the version field, which would wrongly send a
    /// single-drop STM32 (F103 etc.) down the RP2040 SYSINFO path.
    fn bringup(&mut self) -> Result<(u32, bool)> {
        self.connect_swd()?;
        self.swj_clock(100_000)?;
        self.command(&[DAP_SWD_CONFIGURE, 0x00])?;
        // idle 0, wait_retry 128 (LE), match_retry 0 — let the probe retry WAITs.
        self.command(&[DAP_TRANSFER_CONFIGURE, 0x00, 0x80, 0x00, 0x00, 0x00])?;

        // Single-drop first (STM32 / GD32 / most DPv1 targets — the common case).
        // It sends NO TARGETSEL, so it can never disturb a DPv2 target, and it's
        // the cheapest path. Retry the line-reset + DPIDR read several times: on
        // dupont wiring the first read after a re-plug often returns ack 0x07
        // (floating bus) before a clean DPIDR settles — one-shot here is exactly
        // what made detection "sometimes works, sometimes not". Mirrors the probe
        // firmware's 12x swd_read_dpidr. Accept only a DPv1 (version < 2) DPIDR.
        const SINGLE_TRIES: usize = 10;
        let mut last = 0u32;
        for _ in 0..SINGLE_TRIES {
            self.line_reset_and_switch()?;
            if let Ok(dpidr) = self.dp_read(0x0) {
                if (dpidr >> 12) & 0xF < 2 {
                    let _ = self.dp_write(0x0, 0x1E); // clear sticky
                    return Ok((dpidr, false));
                }
                last = dpidr; // version >= 2: not single-drop — try multidrop below
            }
        }

        // Not a DPv1 single-drop target → try SWD multidrop (RP2040). Accept only
        // the EXACT RP2040 DPIDR — never bus-contention garbage a non-selected read
        // returns, nor a single-drop STM32 that ignored the TARGETSEL. Resets first.
        let mut errs = Vec::new();
        for &dormant in &[true, false] {
            for &targetsel in &[RP2040_TARGETSEL_CORE0, RP2040_TARGETSEL_CORE1] {
                match self.try_multidrop(dormant, targetsel) {
                    Ok(dpidr) if dpidr == RP2040_DPIDR => return Ok((dpidr, true)),
                    Ok(dpidr) => errs.push(format!(
                        "{}/{targetsel:07x}: DPIDR 0x{dpidr:08X} != RP2040",
                        if dormant { "dormant" } else { "reset" }
                    )),
                    Err(e) => errs.push(format!(
                        "{}/{targetsel:07x}: {e}",
                        if dormant { "dormant" } else { "reset" }
                    )),
                }
            }
        }
        anyhow::bail!(
            "no SWD target answered after {SINGLE_TRIES} single-drop tries (last DPIDR 0x{last:08X}); multidrop: {}",
            errs.join("; ")
        )
    }

    /// Clear all sticky error flags via the DP ABORT register (STKCMP/STKERR/
    /// WDERR/ORUNERR, no DAPABORT). A single faulted AP access latches STKERR and
    /// then every later AP transfer FAULTs until this clears it.
    fn clear_sticky(&mut self) -> Result<()> {
        self.dp_write(0x0, 0x1E)?;
        Ok(())
    }

    /// Power up the debug/system domains and select AP0 for 32-bit MEM-AP reads.
    fn open_mem_ap(&mut self) -> Result<()> {
        self.dp_write(0x4, 0x5000_0000)?; // CTRL/STAT: CSYSPWRUPREQ | CDBGPWRUPREQ
        let mut ok = false;
        for _ in 0..50 {
            if self.dp_read(0x4)? & 0xA000_0000 == 0xA000_0000 {
                ok = true;
                break;
            }
        }
        if !ok {
            anyhow::bail!("DP power-up not acknowledged");
        }
        self.clear_sticky()?; // a prior multidrop/dormant probe can leave STKERR set
        self.dp_write(0x8, 0x0000_0000)?; // SELECT: AP 0, bank 0
        self.ap_write(0x0, 0x2300_0052)?; // CSW: 32-bit, debug enable
        Ok(())
    }

    /// Read one 32-bit word from the target's memory map (read-only). For a
    /// single CMSIS-DAP transfer the firmware completes the posted AP read and
    /// returns the data directly, so use the DRW read result. On a FAULT (sticky
    /// STKERR latched by an earlier access), clear it and retry once.
    fn read_mem32(&mut self, addr: u32) -> Result<u32> {
        match self.read_mem32_once(addr) {
            Ok(v) => Ok(v),
            Err(e) => {
                let _ = self.clear_sticky();
                self.read_mem32_once(addr).map_err(|_| e)
            }
        }
    }

    fn read_mem32_once(&mut self, addr: u32) -> Result<u32> {
        self.ap_write(0x4, addr)?; // TAR
        self.ap_read(0xC) // DRW — data returned by the probe
    }
}

impl TargetLink for CmsisDapLink {
    fn read_word(&mut self, addr: u32) -> Result<u32> {
        self.read_mem32(addr)
    }
}

/// Layer 1: identify an RP2040-based CMSIS-DAP debug probe from VID:PID + USB
/// descriptors (no SWD command issued). Keys match `print_stlink_controller_info`.
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
/// Returns keys for `print_stlink_target_info`; auto-detects target presence.
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
