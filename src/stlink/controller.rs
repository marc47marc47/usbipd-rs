use crate::*;

pub(crate) struct StlinkControllerProfile {
    pub(crate) generation: &'static str,
    pub(crate) controller_mcu: &'static str,
    pub(crate) core: &'static str,
    pub(crate) architecture: &'static str,
    pub(crate) max_clock: &'static str,
    pub(crate) flash: &'static str,
    pub(crate) sram: &'static str,
    pub(crate) package: &'static str,
    pub(crate) supply: &'static str,
    pub(crate) flash_map: &'static str,
    pub(crate) sram_map: &'static str,
    pub(crate) system_memory: &'static str,
    pub(crate) option_bytes: &'static str,
    pub(crate) unique_id: &'static str,
    pub(crate) self_debug: &'static str,
    pub(crate) upstream: &'static str,
    pub(crate) downstream: &'static str,
    pub(crate) confidence: &'static str,
}

pub(crate) fn stlink_controller_profile(pid: u16) -> Option<StlinkControllerProfile> {
    Some(match pid {
        0x3748 => StlinkControllerProfile {
            generation: "ST-Link/V2",
            controller_mcu: "STM32F103C8T6/CBT6 (implementation-dependent)",
            core: "Arm Cortex-M3",
            architecture: "Armv7-M, Thumb/Thumb-2",
            max_clock: "72 MHz",
            flash: "64/128 KB (implementation-dependent)",
            sram: "20 KB",
            package: "Implementation-dependent",
            supply: "2.0-3.6 V",
            flash_map: "0x08000000 (size depends on controller variant)",
            sram_map: "0x20000000-0x20004FFF",
            system_memory: "0x1FFFF000-0x1FFFF7FF",
            option_bytes: "0x1FFFF800-0x1FFFF80F",
            unique_id: "96-bit UID at 0x1FFFF7E8",
            self_debug: "PA13/SWDIO, PA14/SWCLK, NRST; external probe required",
            upstream: "USB 2.0 Full Speed",
            downstream: "SWD/JTAG",
            confidence: "VID:PID identifies ST-Link/V2; exact MCU varies on clones",
        },
        0x374b => StlinkControllerProfile {
            generation: "ST-Link/V2-1",
            controller_mcu: "STM32F103CBT6",
            core: "Arm Cortex-M3",
            architecture: "Armv7-M, Thumb/Thumb-2",
            max_clock: "72 MHz",
            flash: "128 KB",
            sram: "20 KB",
            package: "LQFP48",
            supply: "2.0-3.6 V",
            flash_map: "0x08000000-0x0801FFFF",
            sram_map: "0x20000000-0x20004FFF",
            system_memory: "0x1FFFF000-0x1FFFF7FF",
            option_bytes: "0x1FFFF800-0x1FFFF80F",
            unique_id: "96-bit UID at 0x1FFFF7E8",
            self_debug: "PA13/SWDIO, PA14/SWCLK, NRST; external probe required",
            upstream: "USB 2.0 Full Speed composite device",
            downstream: "SWD (Nucleo on-board target link)",
            confidence: "Known ST-Link/V2-1 hardware profile",
        },
        0x374e | 0x374f => StlinkControllerProfile {
            generation: "ST-Link/V3",
            controller_mcu: "Not determined from VID:PID alone",
            core: "Not determined from VID:PID alone",
            architecture: "Implementation-dependent",
            max_clock: "Implementation-dependent",
            flash: "Implementation-dependent",
            sram: "Implementation-dependent",
            package: "Implementation-dependent",
            supply: "Implementation-dependent",
            flash_map: "Implementation-dependent",
            sram_map: "Implementation-dependent",
            system_memory: "Implementation-dependent",
            option_bytes: "Implementation-dependent",
            unique_id: "Implementation-dependent",
            self_debug: "Implementation-dependent; external probe required",
            upstream: "USB 2.0 High Speed capable",
            downstream: "SWD/JTAG",
            confidence: "VID:PID identifies ST-Link/V3 generation only",
        },
        _ => return None,
    })
}

/// A layer-1 debug-controller report (ST-Link or CMSIS-DAP probe). Every row is
/// optional because the two producers fill different subsets; `print` emits the
/// present rows in a fixed order.
#[derive(Default)]
pub(crate) struct StlinkControllerInfo {
    pub(crate) layer: Option<String>,
    pub(crate) generation: Option<String>,
    pub(crate) controller_mcu: Option<String>,
    pub(crate) core: Option<String>,
    pub(crate) architecture: Option<String>,
    pub(crate) max_clock: Option<String>,
    pub(crate) flash: Option<String>,
    pub(crate) sram: Option<String>,
    pub(crate) package: Option<String>,
    pub(crate) supply: Option<String>,
    pub(crate) flash_map: Option<String>,
    pub(crate) sram_map: Option<String>,
    pub(crate) system_memory: Option<String>,
    pub(crate) option_bytes: Option<String>,
    pub(crate) unique_id: Option<String>,
    pub(crate) controller_debug: Option<String>,
    pub(crate) upstream: Option<String>,
    pub(crate) downstream: Option<String>,
    pub(crate) usb_identity: Option<String>,
    pub(crate) descriptor_access: Option<String>,
    pub(crate) usb_product: Option<String>,
    pub(crate) usb_serial: Option<String>,
    pub(crate) usb_device_version: Option<String>,
    pub(crate) debug_interface_driver: Option<String>,
    pub(crate) identification: Option<String>,
    pub(crate) firmware_access: Option<String>,
    pub(crate) layer2: Option<String>,
}

impl StlinkControllerInfo {
    pub(crate) fn is_empty(&self) -> bool {
        self.layer.is_none() && self.generation.is_none() && self.usb_identity.is_none()
    }

    pub(crate) fn print(&self) {
        println!("  Architecture:");
        let rows: [(&str, &Option<String>); 27] = [
            ("Layer", &self.layer),
            ("Generation", &self.generation),
            ("Controller MCU", &self.controller_mcu),
            ("Core", &self.core),
            ("Architecture", &self.architecture),
            ("Max clock", &self.max_clock),
            ("Flash", &self.flash),
            ("SRAM", &self.sram),
            ("Package", &self.package),
            ("Supply", &self.supply),
            ("Flash map", &self.flash_map),
            ("SRAM map", &self.sram_map),
            ("System memory", &self.system_memory),
            ("Option bytes", &self.option_bytes),
            ("Unique ID", &self.unique_id),
            ("Controller debug", &self.controller_debug),
            ("Upstream", &self.upstream),
            ("Downstream", &self.downstream),
            ("USB identity", &self.usb_identity),
            ("Descriptor access", &self.descriptor_access),
            ("USB product", &self.usb_product),
            ("USB serial", &self.usb_serial),
            ("USB device version", &self.usb_device_version),
            ("Debug interface driver", &self.debug_interface_driver),
            ("Identification", &self.identification),
            ("Firmware access", &self.firmware_access),
            ("Layer 2", &self.layer2),
        ];
        for (key, val) in rows {
            if let Some(value) = val {
                println!("  {:<24} {}", format!("{key}:"), value);
            }
        }
    }
}

/// Return only the USB-visible debug-controller architecture (layer 1).
/// This function enumerates cached descriptors and never opens the debug
/// interface, so it cannot issue an SWD/JTAG command or touch layer 2.
pub(crate) fn run_stlink_controller_query(vid: u16, pid: u16) -> Result<StlinkControllerInfo> {
    let profile = stlink_controller_profile(pid)
        .ok_or_else(|| anyhow::anyhow!("no layer-1 ST-Link profile for {vid:04x}:{pid:04x}"))?;
    // Descriptor details are optional; the architecture profile remains valid
    // from the VID:PID already present in the native USB list.
    let dev = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut devices| devices.find(|d| d.vendor_id() == vid && d.product_id() == pid));

    let mut info = StlinkControllerInfo {
        layer: Some("1 - USB debug controller".into()),
        generation: Some(profile.generation.into()),
        controller_mcu: Some(profile.controller_mcu.into()),
        core: Some(profile.core.into()),
        architecture: Some(profile.architecture.into()),
        max_clock: Some(profile.max_clock.into()),
        flash: Some(profile.flash.into()),
        sram: Some(profile.sram.into()),
        package: Some(profile.package.into()),
        supply: Some(profile.supply.into()),
        flash_map: Some(profile.flash_map.into()),
        sram_map: Some(profile.sram_map.into()),
        system_memory: Some(profile.system_memory.into()),
        option_bytes: Some(profile.option_bytes.into()),
        unique_id: Some(profile.unique_id.into()),
        controller_debug: Some(profile.self_debug.into()),
        upstream: Some(profile.upstream.into()),
        downstream: Some(profile.downstream.into()),
        usb_identity: Some(format!("{vid:04x}:{pid:04x}")),
        identification: Some(profile.confidence.into()),
        firmware_access: Some("Not attempted; read-protection status unknown".into()),
        layer2: Some("Auto-probed read-only below (native SWD)".into()),
        ..Default::default()
    };
    if let Some(dev) = dev {
        info.descriptor_access = Some("Available through nusb".into());
        if let Some(product) = dev.product_string() {
            info.usb_product = Some(product.into());
        }
        if let Some(serial) = dev.serial_number() {
            info.usb_serial = Some(serial.into());
        }
        info.usb_device_version = Some(format!("0x{:04x}", dev.device_version()));
    } else {
        info.descriptor_access =
            Some("Unavailable through nusb; VID:PID profile still shown".into());
    }
    if let Some(driver) = stlink_driver_check(pid) {
        info.debug_interface_driver = Some(match driver {
            StlinkDriver::WinUsb => "WinUSB".to_string(),
            StlinkDriver::Other(service) => service,
        });
    }
    Ok(info)
}

