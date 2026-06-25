use crate::*;

// ============================================================================
// Native ST-Link SWD reader (--mcu-alive-native)
//
// Speaks the ST-Link bulk command protocol directly over `nusb`, so no
// external tool (probe-rs / pyocd / stlink) is needed. The only thing it
// requires on Windows is a WinUSB binding on the MI_00 vendor interface — the
// irreducible minimum, since user-mode cannot issue bulk transfers to a
// driverless interface (see `--install-driver`). On Linux/macOS nusb's libusb
// / IOKit backend can claim the interface without a manual driver step.
//
// Command/response layout cross-checked against probe-rs, stlink-org/stlink,
// and OpenOCD. The sequence is read-only: it enters SWD and reads ID registers
// only — no halt, reset, erase, or memory write is ever issued.
// ============================================================================

pub(crate) const STLINK_CMD_SIZE: usize = 16;
pub(crate) const STLINK_GET_VERSION: u8 = 0xF1;
pub(crate) const STLINK_DFU_COMMAND: u8 = 0xF3;
pub(crate) const STLINK_DFU_EXIT: u8 = 0x07;
pub(crate) const STLINK_SWIM_COMMAND: u8 = 0xF4;
pub(crate) const STLINK_SWIM_EXIT: u8 = 0x01;
pub(crate) const STLINK_GET_CURRENT_MODE: u8 = 0xF5;
pub(crate) const STLINK_GET_TARGET_VOLTAGE: u8 = 0xF7;
pub(crate) const STLINK_DEBUG_COMMAND: u8 = 0xF2;
pub(crate) const STLINK_DEBUG_APIV2_ENTER: u8 = 0x30;
pub(crate) const STLINK_DEBUG_EXIT: u8 = 0x21;
pub(crate) const STLINK_DEBUG_ENTER_SWD: u8 = 0xA3;
pub(crate) const STLINK_DEBUG_APIV2_READ_IDCODES: u8 = 0x31;
pub(crate) const STLINK_DEBUG_APIV2_READDEBUGREG: u8 = 0x36;
pub(crate) const STLINK_JTAG_OK: u8 = 0x80;

pub(crate) fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// An opened ST-Link debug interface. Owns the device/interface handles so the
/// endpoints stay valid for the link's lifetime.
pub(crate) struct StlinkLink {
    _device: nusb::Device,
    _iface: nusb::Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    in_max: usize,
}

impl StlinkLink {
    /// Open a probe end to end: open the USB device, claim its `MI_00`
    /// vendor/debug interface, open the bulk endpoints, and resync the pipes.
    /// Returns an error whose message names the failing stage — notably
    /// `claim_interface` failing is the Windows "no WinUSB on MI_00" boundary.
    pub(crate) fn open_device(probe: &nusb::DeviceInfo) -> Result<Self> {
        let device = probe.open().wait().context("open USB device")?;
        let iface_num = probe
            .interfaces()
            .find(|i| i.class() == 0xff)
            .map(|i| i.interface_number())
            .unwrap_or(0);
        let iface = device
            .claim_interface(iface_num)
            .wait()
            .with_context(|| format!("claim MI_{iface_num:02} (needs WinUSB on MI_00)"))?;
        // RX is 0x81 on every ST-Link; TX is 0x02 on the original V2, else 0x01.
        let ep_out_addr = if probe.product_id() == 0x3748 { 0x02 } else { 0x01 };
        let out = iface
            .endpoint::<Bulk, Out>(ep_out_addr)
            .map_err(|e| anyhow::anyhow!("open OUT endpoint 0x{ep_out_addr:02x}: {e}"))?;
        let mut inp = iface
            .endpoint::<Bulk, In>(0x81)
            .map_err(|e| anyhow::anyhow!("open IN endpoint 0x81: {e}"))?;
        let in_max = inp.max_packet_size().max(1);
        // Reset both pipes' halt/toggle state and drain any stale response left
        // by a previously interrupted command, so the firmware command FSM
        // starts in sync. Best-effort: ignore errors on a clean device.
        let mut out = out;
        let _ = out.clear_halt().wait();
        let _ = inp.clear_halt().wait();
        let mut link = Self { _device: device, _iface: iface, ep_out: out, ep_in: inp, in_max };
        link.drain_stale();
        Ok(link)
    }

    /// Best-effort flush of any response the probe is still waiting to send
    /// (from an earlier aborted command). Short timeout; stops at the first
    /// empty/failed read so a clean device costs only one quick poll.
    pub(crate) fn drain_stale(&mut self) {
        let timeout = std::time::Duration::from_millis(60);
        for _ in 0..3 {
            let completion = self.ep_in.transfer_blocking(Buffer::new(self.in_max), timeout);
            match completion.status {
                Ok(()) if !completion.buffer.is_empty() => continue,
                _ => break,
            }
        }
    }

    /// Send a 16-byte (zero-padded) command, then read up to `read_len` bytes.
    ///
    /// The bulk IN request length is rounded up to a multiple of the endpoint's
    /// max packet size: WinUSB rejects a non-multiple read with
    /// ERROR_INVALID_PARAMETER ("invalid or unsupported argument"). The device
    /// returns a short packet, so the actual response may be shorter.
    pub(crate) fn cmd(&mut self, bytes: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let timeout = std::time::Duration::from_millis(1000);
        let mut out = vec![0u8; STLINK_CMD_SIZE];
        out[..bytes.len()].copy_from_slice(bytes);
        if let Err(e) = self
            .ep_out
            .transfer_blocking(Buffer::from(out), timeout)
            .into_result()
        {
            self.recover_from(e);
            return Err(anyhow::anyhow!("bulk OUT failed: {e}"));
        }
        if read_len == 0 {
            return Ok(Vec::new());
        }
        let request = read_len.div_ceil(self.in_max) * self.in_max;
        match self
            .ep_in
            .transfer_blocking(Buffer::new(request), timeout)
            .into_result()
        {
            Ok(resp) => Ok(resp.into_vec()),
            Err(e) => {
                self.recover_from(e);
                Err(anyhow::anyhow!("bulk IN failed: {e}"))
            }
        }
    }

    /// Recover the pipes after a failed transfer. A STALL (the probe rejecting an
    /// unsupported command, e.g. some ST-Link/V2 clones STALL GET_TARGET_VOLTAGE)
    /// halts the endpoint; without a CLEAR_FEATURE every later command also fails
    /// (the OUT side then just times out). Clearing both halts lets the next
    /// command — e.g. ENTER_SWD — proceed.
    pub(crate) fn recover_from(&mut self, err: TransferError) {
        if matches!(err, TransferError::Stall) {
            let _ = self.ep_out.clear_halt().wait();
            let _ = self.ep_in.clear_halt().wait();
        }
    }

    /// Probe firmware version (read-only, valid in any mode).
    pub(crate) fn get_version(&mut self) -> Result<String> {
        let r = self.cmd(&[STLINK_GET_VERSION], 6)?;
        if r.len() < 2 {
            anyhow::bail!("short version response ({} bytes)", r.len());
        }
        let v = ((r[0] as u16) << 8) | r[1] as u16;
        Ok(format!(
            "ST-Link v{} JTAG/SWD v{} SWIM v{}",
            (v >> 12) & 0x0F,
            (v >> 6) & 0x3F,
            v & 0x3F
        ))
    }

    /// Target reference voltage in millivolts (read-only ADC sample).
    pub(crate) fn get_voltage_mv(&mut self) -> Result<u32> {
        let r = self.cmd(&[STLINK_GET_TARGET_VOLTAGE], 8)?;
        let factor = le_u32(&r, 0).context("short voltage response")?;
        let reading = le_u32(&r, 4).context("short voltage response")?;
        if factor == 0 {
            anyhow::bail!("voltage divisor is zero (probe not ready)");
        }
        Ok((2400u64 * reading as u64 / factor as u64) as u32)
    }

    /// Current probe mode (0=DFU, 1=mass storage, 2=debug/JTAG, 3=SWIM).
    pub(crate) fn current_mode(&mut self) -> u8 {
        self.cmd(&[STLINK_GET_CURRENT_MODE], 2)
            .ok()
            .and_then(|r| r.first().copied())
            .unwrap_or(0xFF)
    }

    /// Leave whatever mode the probe powered up in, so a fresh ENTER_SWD is
    /// accepted. A bare ST-Link/V2 often enumerates in DFU mode; entering SWD
    /// without leaving it first silently fails (status 0x00). Best-effort.
    pub(crate) fn leave_current_mode(&mut self) {
        let _ = match self.current_mode() {
            0 => self.cmd(&[STLINK_DFU_COMMAND, STLINK_DFU_EXIT], 0),
            2 => self.cmd(&[STLINK_DEBUG_COMMAND, STLINK_DEBUG_EXIT], 0),
            3 => self.cmd(&[STLINK_SWIM_COMMAND, STLINK_SWIM_EXIT], 0),
            _ => Ok(Vec::new()),
        };
    }

    /// Enter SWD mode. Returns the probe status byte (0x80 = OK). Leaves the
    /// power-up mode first. This does not halt or reset the core — it only
    /// initializes the debug link.
    pub(crate) fn enter_swd(&mut self) -> Result<u8> {
        self.leave_current_mode();
        let r = self.cmd(
            &[STLINK_DEBUG_COMMAND, STLINK_DEBUG_APIV2_ENTER, STLINK_DEBUG_ENTER_SWD],
            2,
        )?;
        Ok(r.first().copied().unwrap_or(0))
    }

    /// Read the DP IDCODE (DPIDR) — the first read-only target ID.
    pub(crate) fn read_idcode(&mut self) -> Result<u32> {
        let r = self.cmd(&[STLINK_DEBUG_COMMAND, STLINK_DEBUG_APIV2_READ_IDCODES], 12)?;
        le_u32(&r, 4).context("short idcode response")
    }

    /// Read a 32-bit debug/AP register at `addr` (read-only). The ST-Link
    /// firmware drives the AHB-AP for this command, so no AP setup is needed.
    pub(crate) fn read_debug_reg(&mut self, addr: u32) -> Result<u32> {
        let a = addr.to_le_bytes();
        let r = self.cmd(
            &[STLINK_DEBUG_COMMAND, STLINK_DEBUG_APIV2_READDEBUGREG, a[0], a[1], a[2], a[3]],
            8,
        )?;
        if r.first().copied().unwrap_or(0) != STLINK_JTAG_OK {
            anyhow::bail!("debug-reg read 0x{addr:08X} status 0x{:02X}", r.first().copied().unwrap_or(0));
        }
        le_u32(&r, 4).context("short debug-reg response")
    }
}

impl TargetLink for StlinkLink {
    fn read_word(&mut self, addr: u32) -> Result<u32> {
        self.read_debug_reg(addr)
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn le_u32_reads_little_endian_at_offset() {
        // ST-Link debug-reg / idcode responses place the 32-bit value at offset 4.
        let resp = [0x80, 0x00, 0x00, 0x00, 0x41, 0x10, 0x00, 0x10];
        assert_eq!(le_u32(&resp, 4), Some(0x10001041));
        assert_eq!(le_u32(&resp, 0), Some(0x0000_0080));
        assert_eq!(le_u32(&resp, 6), None); // out of bounds, no panic
    }
}
