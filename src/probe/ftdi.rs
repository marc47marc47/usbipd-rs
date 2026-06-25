use crate::*;

/// FTDI bridge descriptor info read read-only from nusb's cached enumeration.
#[derive(Default)]
pub(crate) struct FtdiInfo {
    pub(crate) vidpid: Option<String>,
    pub(crate) manufacturer: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) serial_number: Option<String>,
    pub(crate) chip_variant: Option<String>,
    pub(crate) interfaces: Option<String>,
    pub(crate) driver: Option<String>,
}

impl FtdiInfo {
    pub(crate) fn is_empty(&self) -> bool {
        self.vidpid.is_none() && self.chip_variant.is_none()
    }

    pub(crate) fn print(&self, board_name: &str) {
        println!("  {:<20} {}", "Board:", board_name);
        let rows: [(&str, &Option<String>); 7] = [
            ("Manufacturer", &self.manufacturer),
            ("Product", &self.product),
            ("Serial number", &self.serial_number),
            ("Chip variant", &self.chip_variant),
            ("Interfaces", &self.interfaces),
            ("Driver", &self.driver),
            ("VID:PID", &self.vidpid),
        ];
        for (k, v) in rows {
            if let Some(v) = v {
                println!("  {:<20} {}", format!("{k}:"), v);
            }
        }
    }
}

pub(crate) fn run_ftdi_info(vid: u16, pid: u16) -> Result<FtdiInfo> {
    // Pull cached descriptor info from nusb's enumeration — no device IO,
    // no chip reset, safe to chain before any serial probe. Windows caches
    // product/serial strings via setupapi, but not the manufacturer string
    // (that field will be None on Win regardless of descriptor contents).
    let dev = nusb::list_devices()
        .wait()
        .context("nusb::list_devices() failed")?
        .find(|d| d.vendor_id() == vid && d.product_id() == pid)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "nusb could not find {:04x}:{:04x} (device replugged between listing and probe?)",
                vid, pid
            )
        })?;

    let mut info = FtdiInfo {
        vidpid: Some(format!("{:04x}:{:04x}", vid, pid)),
        manufacturer: dev.manufacturer_string().map(Into::into),
        product: dev.product_string().map(Into::into),
        serial_number: dev.serial_number().map(Into::into),
        ..Default::default()
    };

    let bcd_dev = dev.device_version();
    let variant = ftdi_chip_variant(pid, bcd_dev);
    let bcd_str = format!("0x{bcd_dev:04x}");
    info.chip_variant = Some(if variant.is_empty() {
        format!("bcdDevice {bcd_str}")
    } else {
        format!("{variant} (bcdDevice {bcd_str})")
    });

    // Windows: only populated for composite devices bound to usbccgp. An
    // FT232R bound straight to ftdibus reports zero interfaces here — skip
    // the field rather than print a misleading "0".
    let ifaces: Vec<String> = dev
        .interfaces()
        .map(|i| {
            let cls = i.class();
            let label = match cls {
                0xff => "Vendor-specific",
                0x02 => "CDC Communications",
                0x0a => "CDC Data",
                0x03 => "HID",
                _    => "Other",
            };
            format!("#{} {label} (0x{cls:02x})", i.interface_number())
        })
        .collect();
    if !ifaces.is_empty() {
        info.interfaces = Some(ifaces.join(", "));
    }

    // Windows-only: which kernel driver currently owns the device. For FTDI
    // boards the expected values are "FTDIBUS" (VCP unloaded) or "ftser2k" /
    // "ftdibus + Serial" (VCP loaded → COMx visible). Reveals when the driver
    // is missing without having to open Device Manager.
    #[cfg(windows)]
    if let Some(drv) = dev.driver() {
        if !drv.is_empty() {
            info.driver = Some(drv.into());
        }
    }

    Ok(info)
}

pub(crate) fn ftdi_chip_variant(pid: u16, bcd_device: u16) -> &'static str {
    // bcdDevice encodes the FTDI silicon revision; for PID 0x6001 (FT232x
    // family) the meaningful values are 0x0200/0x0400/0x0600. The R and RL
    // share 0x0600 — they're the same die in different packages, so listing
    // them together is correct.
    match (pid, bcd_device) {
        (0x6001, 0x0200) => "FT8U232AM",
        (0x6001, 0x0400) => "FT232BM",
        (0x6001, 0x0600) => "FT232R / FT232RL",
        _ => "",
    }
}

