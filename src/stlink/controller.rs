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

/// Return only the USB-visible debug-controller architecture (layer 1).
/// This function enumerates cached descriptors and never opens the debug
/// interface, so it cannot issue an SWD/JTAG command or touch layer 2.
pub(crate) fn run_stlink_controller_query(vid: u16, pid: u16) -> Result<HashMap<String, String>> {
    let profile = stlink_controller_profile(pid)
        .ok_or_else(|| anyhow::anyhow!("no layer-1 ST-Link profile for {vid:04x}:{pid:04x}"))?;
    // Descriptor details are optional; the architecture profile remains valid
    // from the VID:PID already present in the native USB list.
    let dev = nusb::list_devices()
        .wait()
        .ok()
        .and_then(|mut devices| devices.find(|d| d.vendor_id() == vid && d.product_id() == pid));

    let mut info = HashMap::new();
    info.insert("Layer".into(), "1 - USB debug controller".into());
    info.insert("Generation".into(), profile.generation.into());
    info.insert("Controller MCU".into(), profile.controller_mcu.into());
    info.insert("Core".into(), profile.core.into());
    info.insert("Architecture".into(), profile.architecture.into());
    info.insert("Max clock".into(), profile.max_clock.into());
    info.insert("Flash".into(), profile.flash.into());
    info.insert("SRAM".into(), profile.sram.into());
    info.insert("Package".into(), profile.package.into());
    info.insert("Supply".into(), profile.supply.into());
    info.insert("Flash map".into(), profile.flash_map.into());
    info.insert("SRAM map".into(), profile.sram_map.into());
    info.insert("System memory".into(), profile.system_memory.into());
    info.insert("Option bytes".into(), profile.option_bytes.into());
    info.insert("Unique ID".into(), profile.unique_id.into());
    info.insert("Controller debug".into(), profile.self_debug.into());
    info.insert("Upstream".into(), profile.upstream.into());
    info.insert("Downstream".into(), profile.downstream.into());
    info.insert("USB identity".into(), format!("{vid:04x}:{pid:04x}"));
    info.insert("Identification".into(), profile.confidence.into());
    info.insert(
        "Firmware access".into(),
        "Not attempted; read-protection status unknown".into(),
    );
    info.insert(
        "Layer 2".into(),
        "Auto-probed read-only below (native SWD)".into(),
    );
    if let Some(dev) = dev {
        info.insert("Descriptor access".into(), "Available through nusb".into());
        if let Some(product) = dev.product_string() {
            info.insert("USB product".into(), product.into());
        }
        if let Some(serial) = dev.serial_number() {
            info.insert("USB serial".into(), serial.into());
        }
        info.insert(
            "USB device version".into(),
            format!("0x{:04x}", dev.device_version()),
        );
    } else {
        info.insert(
            "Descriptor access".into(),
            "Unavailable through nusb; VID:PID profile still shown".into(),
        );
    }
    if let Some(driver) = stlink_driver_check(pid) {
        let driver = match driver {
            StlinkDriver::WinUsb => "WinUSB".to_string(),
            StlinkDriver::Other(service) => service,
        };
        info.insert("Debug interface driver".into(), driver);
    }
    Ok(info)
}

pub(crate) fn print_stlink_controller_info(info: &HashMap<String, String>) {
    println!("  Architecture:");
    let order = [
        "Layer",
        "Generation",
        "Controller MCU",
        "Core",
        "Architecture",
        "Max clock",
        "Flash",
        "SRAM",
        "Package",
        "Supply",
        "Flash map",
        "SRAM map",
        "System memory",
        "Option bytes",
        "Unique ID",
        "Controller debug",
        "Upstream",
        "Downstream",
        "USB identity",
        "Descriptor access",
        "USB product",
        "USB serial",
        "USB device version",
        "Debug interface driver",
        "Identification",
        "Firmware access",
        "Layer 2",
    ];
    for key in order {
        if let Some(value) = info.get(key) {
            println!("  {:<24} {}", format!("{key}:"), value);
        }
    }
}

