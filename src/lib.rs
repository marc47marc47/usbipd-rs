// The Windows USB driver-binding diagnostics (--driver-status, --install-driver,
// stlink_driver_check, …) are compiled on every OS but only *used* behind
// #[cfg(windows)]. On other targets they are legitimately dead code, so relax
// the lint there; Windows (the primary platform) still enforces -D dead_code.
#![cfg_attr(not(windows), allow(dead_code))]

pub(crate) use anyhow::{Context, Result};
pub(crate) use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
pub(crate) use nusb::{Endpoint, MaybeFuture};
pub(crate) use std::collections::HashMap;
pub(crate) use std::io::{Read, Write};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::Command;
pub(crate) use unicode_width::UnicodeWidthStr;

mod boards;
mod cli;
mod usb;
mod probe;
mod stm32;
mod stlink;
mod windows;
mod cmd;
mod swd;
mod installer;

pub(crate) use boards::*;
pub(crate) use cli::*;
pub(crate) use usb::*;
pub(crate) use probe::*;
pub(crate) use stm32::*;
pub(crate) use stlink::*;
pub(crate) use windows::*;
pub(crate) use cmd::*;
pub(crate) use swd::*;
pub(crate) use installer::*;

pub fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        print_help();
        return Ok(());
    }
    if args.iter().any(|a| matches!(a.as_str(), "-V" | "--version")) {
        println!("usbipd-rs {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--list-tools") {
        return cmd_list_tools();
    }
    if args.iter().any(|a| a == "--driver-status") {
        return cmd_driver_status();
    }
    if args.iter().any(|a| a == "--install-driver") {
        let confirm = args.iter().any(|a| matches!(a.as_str(), "--confirm" | "--yes" | "-y"));
        return cmd_install_driver(confirm);
    }
    if args.iter().any(|a| a == "--mcu-alive-native") {
        return cmd_mcu_alive_native();
    }
    if args.iter().any(|a| a == "--mcu-alive") {
        return cmd_mcu_alive();
    }
    if let Some(idx) = args.iter().position(|a| a == "--install") {
        let tool = args.get(idx + 1).map(String::as_str).unwrap_or("");
        if tool.is_empty() {
            anyhow::bail!("--install requires a tool name. Try `--list-tools`.");
        }
        return cmd_install(tool);
    }

    cmd_list_usb()
}

pub(crate) fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stlink_v2_1_profile_describes_the_usb_controller_not_the_target() {
        let profile = stlink_controller_profile(0x374b).expect("known ST-Link/V2-1 PID");

        assert_eq!(profile.controller_mcu, "STM32F103CBT6");
        assert_eq!(profile.core, "Arm Cortex-M3");
        assert_eq!(profile.flash, "128 KB");
        assert_eq!(profile.sram, "20 KB");
        assert_eq!(profile.flash_map, "0x08000000-0x0801FFFF");
        assert!(profile.self_debug.contains("external probe required"));
        assert!(profile.downstream.starts_with("SWD"));
    }

    #[test]
    fn layer2_decoder_identifies_stm32f446_without_writing_target() {
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x410FC241]);
        regs.insert(0xE0042000, vec![0x10000421]);
        regs.insert(0x1FFF7A22, vec![512]);
        regs.insert(0x1FFF7A10, vec![0x11223344, 0x55667788, 0x99AABBCC]);
        regs.insert(0x40023C14, vec![0x0000AA00]);

        let info = decode_stlink_regs(&regs, Some(0x421));

        assert!(info["Device ID"].contains("STM32F446"));
        assert!(info["Core"].contains("Cortex-M4"));
        assert_eq!(info["Flash size"], "512 KB");
        assert_eq!(info["Flash map"], "0x08000000-0x0807FFFF");
        assert!(info["Read protection"].contains("Disabled"));
        assert_eq!(info["Access"], "Read-only identity registers");
    }

    fn node(status: &str, problem: &str, service: &str) -> WindowsUsbDriverNode {
        let mut n = WindowsUsbDriverNode::default();
        n.set_field("STATUS", status);
        n.set_field("PROBLEM", problem);
        n.set_field("SERVICE", service);
        n
    }

    #[test]
    fn healthy_node_has_no_issue() {
        assert_eq!(classify_driver_issue(&node("OK", "0", "WINUSB")), None);
        assert_eq!(classify_driver_issue(&node("OK", "", "usbccgp")), None);
    }

    #[test]
    fn problem_28_without_service_is_a_missing_driver() {
        // The real ST-Link MI_00 case: enumerated, code 28, no bound service.
        assert_eq!(
            classify_driver_issue(&node("Error", "28", "")),
            Some(DriverIssue::MissingDriver)
        );
    }

    #[test]
    fn problem_28_with_service_is_a_load_failure_not_a_gap() {
        assert_eq!(
            classify_driver_issue(&node("Error", "28", "WINUSB")),
            Some(DriverIssue::DriverLoadFailure)
        );
    }

    #[test]
    fn problem_codes_map_to_distinct_categories() {
        assert_eq!(classify_driver_issue(&node("Error", "22", "x")), Some(DriverIssue::StoppedNode));
        assert_eq!(classify_driver_issue(&node("Error", "39", "x")), Some(DriverIssue::DriverLoadFailure));
        assert_eq!(classify_driver_issue(&node("Error", "52", "x")), Some(DriverIssue::SignatureFailure));
        assert_eq!(classify_driver_issue(&node("Error", "43", "x")), Some(DriverIssue::Blocked));
        assert_eq!(classify_driver_issue(&node("Error", "12", "x")), Some(DriverIssue::ResourceConflict));
        assert_eq!(classify_driver_issue(&node("Error", "99", "x")), Some(DriverIssue::Unknown));
    }

    #[test]
    fn stlink_mi00_advice_points_at_a_bundled_inf() {
        let advice = known_driver_advice("USB\\VID_0483&PID_374B&MI_00\\7&abc&0&0000")
            .expect("ST-Link MI_00 has known advice");
        assert!(advice.name.contains("STSW-LINK009"));
        assert_eq!(advice.local_inf, Some("windows-driver/stsw-link009/stlink_dbg_winusb.inf"));
        // The healthy sibling interfaces must not match this binding.
        assert!(known_driver_advice("USB\\VID_0483&PID_374B&MI_01\\x").is_none());
    }

    #[test]
    fn instance_vidpid_parses_uppercase_and_lowercase() {
        assert_eq!(instance_vidpid("USB\\VID_0483&PID_374B&MI_00\\x"), Some((0x0483, 0x374b)));
        assert_eq!(instance_vidpid("usb\\vid_10c4&pid_ea60"), Some((0x10c4, 0xea60)));
        assert_eq!(instance_vidpid("not-a-usb-id"), None);
    }

    #[test]
    fn driverstore_parser_extracts_oem_name_for_matching_block() {
        // Two blocks separated by a blank line; only the second mentions our INF.
        let text = "Published Name: oem10.inf\r\nOriginal Name: usbser.inf\r\n\r\nPublished Name: oem42.inf\r\nOriginal Name: stlink_dbg_winusb.inf\r\nProvider: STMicroelectronics";
        assert_eq!(parse_driverstore_oem(text, "stlink_dbg_winusb.inf"), vec!["oem42.inf"]);
        assert!(parse_driverstore_oem(text, "absent.inf").is_empty());
    }

    #[test]
    fn driverstore_parser_handles_lf_only_and_trailing_punctuation() {
        let text = "Published Name: oem7.inf,\nOriginal Name: stlink_dbg_winusb.inf";
        assert_eq!(parse_driverstore_oem(text, "STLINK_DBG_WINUSB.INF"), vec!["oem7.inf"]);
    }

    #[test]
    fn le_u32_reads_little_endian_at_offset() {
        // ST-Link debug-reg / idcode responses place the 32-bit value at offset 4.
        let resp = [0x80, 0x00, 0x00, 0x00, 0x41, 0x10, 0x00, 0x10];
        assert_eq!(le_u32(&resp, 4), Some(0x10001041));
        assert_eq!(le_u32(&resp, 0), Some(0x0000_0080));
        assert_eq!(le_u32(&resp, 6), None); // out of bounds, no panic
    }

    #[test]
    fn chip_file_parses_stlink_format() {
        let text = "\
# comment line
dev_type STM32F446
chip_id 0x421                // STM32_CHIPID_F446
flash_type F2_F4
flash_size_reg 0x1fff7a22
sram_size 0x20000            // 128 KB
option_base 0x40023c14
";
        let def = parse_chip_file(text).expect("has chip_id");
        assert_eq!(def.dev_id, 0x421);
        assert_eq!(def.name, "STM32F446");
        assert_eq!(def.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(def.sram_kb, Some(128));
        assert!(parse_chip_file("dev_type Foo\n").is_none()); // no chip_id
    }

    #[test]
    fn chip_int_accepts_hex_and_decimal() {
        assert_eq!(parse_chip_int("0x20000"), Some(0x20000));
        assert_eq!(parse_chip_int("0X10"), Some(16));
        assert_eq!(parse_chip_int("512"), Some(512));
        assert_eq!(parse_chip_int("0x421,"), Some(0x421)); // trailing punctuation
    }

    #[test]
    fn resolve_chip_falls_back_to_builtin_when_no_file() {
        let r = resolve_chip(0x421, &[]).expect("F446 is built-in");
        assert!(r.name.contains("STM32F446"));
        assert_eq!(r.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(r.uid_addr, Some(0x1FFF7A10)); // verified built-in UID addr
        assert_eq!(r.sram_kb, Some(128));
        assert_eq!(r.source, "built-in");
    }

    #[test]
    fn resolve_chip_file_overrides_builtin_but_keeps_verified_addrs() {
        let db = vec![ChipDef {
            dev_id: 0x421,
            name: "My Custom F446 Board".to_string(),
            flash_size_addr: Some(0x1FFF7A22),
            sram_kb: Some(128),
            source: "etc/chips/F446.chip".to_string(),
        }];
        let r = resolve_chip(0x421, &db).unwrap();
        assert_eq!(r.name, "My Custom F446 Board"); // file name wins
        assert_eq!(r.uid_addr, Some(0x1FFF7A10)); // UID still from built-in
        assert_eq!(r.source, "etc/chips/F446.chip");
    }

    #[test]
    fn resolve_chip_file_adds_a_part_unknown_to_builtin() {
        let db = vec![ChipDef {
            dev_id: 0x999,
            name: "STM32 Experimental".to_string(),
            flash_size_addr: Some(0x1FFF7A22),
            sram_kb: Some(64),
            source: "etc/chips/X.chip".to_string(),
        }];
        let r = resolve_chip(0x999, &db).expect("file-defined part resolves");
        assert_eq!(r.name, "STM32 Experimental");
        assert_eq!(r.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(r.sram_kb, Some(64));
        assert_eq!(r.uid_addr, None); // not in built-in → no UID/RDP
        assert_eq!(r.rdp_addr, None);
        assert!(resolve_chip(0x999, &[]).is_none()); // unknown without a file
    }

    #[test]
    fn native_dbgmcu_idcode_decodes_to_stm32_family() {
        // What --mcu-alive-native feeds decode_stlink_regs after reading DBGMCU.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x410FC241]); // CPUID: Cortex-M4
        regs.insert(0xE0042000, vec![0x10010421]); // DBGMCU: DEV_ID 0x421 = F446
        let info = decode_stlink_regs(&regs, Some(0x421));
        assert!(info["Device ID"].contains("STM32F446"));
        assert!(info["Core"].contains("Cortex-M4"));
    }

    #[test]
    fn native_stm32f103_target_decodes_as_layer2() {
        // A "Blue Pill" STM32F103C8 hanging off the probe (ST-Link or the RP2040
        // CMSIS-DAP single-drop path): DEV_ID 0x410, Cortex-M3, 64 KB flash.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]); // CPUID: Cortex-M3
        regs.insert(0xE0042000, vec![0x20036410]); // DBGMCU: DEV_ID 0x410, REV_ID 0x2003
        regs.insert(0x1FFFF7E0, vec![64]); // F1 flash-size register: 64 KB
        let info = decode_stlink_regs(&regs, Some(0x410));
        assert!(info["Device ID"].contains("STM32F1 medium-density"));
        assert!(info["Core"].contains("Cortex-M3"));
        assert_eq!(info["Flash size"], "64 KB");
        assert!(info["Revision"].contains("rev 1/2/3/X/Y"));
        assert!(!info.contains_key("Vendor")); // genuine ST REV_ID → no clone flag
    }

    #[test]
    fn gd32f103ret6_identified_as_gigadevice_not_stm32() {
        // GD32F103RET6 mirrors ST's high-density DEV_ID 0x414 but reports a
        // REV_ID (0x1309) outside ST's {0x1000,0x1001,0x1003} set; with 512 KB
        // flash it resolves to the GD32F103xE density, named as GigaDevice.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]); // Cortex-M3 r2p1
        regs.insert(0xE0042000, vec![0x13090414]); // DBGMCU: DEV_ID 0x414, REV_ID 0x1309
        regs.insert(0x1FFFF7E0, vec![512]); // F1 high-density flash-size word: 512 KB
        let info = decode_stlink_regs(&regs, Some(0x414));
        // Leads with the GD32 model, NOT the ST family name.
        assert!(info["Device ID"].contains("GD32F103xE"));
        assert!(info["Device ID"].contains("GD32F103RET6"));
        assert!(!info["Device ID"].contains("STM32"));
        assert!(info["Max clock"].contains("108 MHz"));
        let vendor = info.get("Vendor").expect("GD32 provenance noted");
        assert!(vendor.contains("GigaDevice") && vendor.contains("0x1309"));

        // A genuine high-density REV_ID must stay STM32 with no GD32 claim.
        regs.insert(0xE0042000, vec![0x10000414]);
        let genuine = decode_stlink_regs(&regs, Some(0x414));
        assert!(genuine["Device ID"].contains("STM32F1 high-density"));
        assert!(!genuine.contains_key("Vendor"));
    }

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

    #[test]
    fn format_target_rows_keeps_gd32_name_over_builtin_stm32() {
        // The full layer-2 path (ST-Link / CMSIS-DAP): decode names the GD32, and
        // the resolved built-in STM32 high-density entry must NOT clobber it.
        let mut regs = HashMap::new();
        regs.insert(0xE000ED00, vec![0x412FC231]);
        regs.insert(0xE0042000, vec![0x13090414]); // DEV_ID 0x414, clone REV_ID 0x1309
        regs.insert(0x1FFFF7E0, vec![512]); // 512 KB → xE
        let resolved = resolve_chip(0x414, &[]).expect("0x414 resolves to built-in STM32");
        let rows = format_target_rows(&regs, Some(0x414), Some(&resolved));
        let dev = &rows.iter().find(|(k, _)| *k == "Device ID").unwrap().1;
        assert!(dev.contains("GD32F103xE") && dev.contains("GD32F103RET6"));
        assert!(!dev.contains("STM32"));
        assert!(rows.iter().any(|(k, v)| *k == "Vendor" && v.contains("GigaDevice")));
    }

    #[test]
    fn cmsisdap_rejects_non_rp2040_multidrop_dpidr() {
        // A bus-contention / STM32-ignored-TARGETSEL read can have version >= 2
        // set; only the exact RP2040 DPIDR must be treated as multidrop so an
        // STM32F103 reaches the single-drop decode path instead of RP2040 SYSINFO.
        assert_eq!(RP2040_DPIDR, 0x0BC12477);
        assert!((RP2040_DPIDR >> 12) & 0xF >= 2); // it IS DPv2…
        let bogus = 0x1BA02477u32; // …but a different DPIDR (also version 2) is not RP2040
        assert!((bogus >> 12) & 0xF >= 2);
        assert_ne!(bogus, RP2040_DPIDR);
    }
}
