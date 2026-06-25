use crate::*;

pub(crate) fn probe_boards(candidates: &[(&Entry, &'static KnownBoard)]) {
    println!("=== Board Probe ===");
    let serial_ports = serialport::available_ports().unwrap_or_default();

    for (entry, board) in candidates {
        let (vid, pid) = entry.vidpid_pair();
        // A single COM port carries exactly one chip: once any serial probe
        // (espflash / stm32flash / avrdude) has identified it, the remaining
        // serial probes in this board's pipeline can only fail — skip them
        // silently.
        let mut serial_chip_found = false;

        for &probe in board.probes {
            let needs_port = probe.needs_serial_port();

            if needs_port && serial_chip_found {
                continue;
            }

            let port_name = if needs_port {
                serial_ports.iter().find_map(|p| match &p.port_type {
                    serialport::SerialPortType::UsbPort(info)
                        if info.vid == vid && info.pid == pid =>
                    {
                        Some(p.port_name.clone())
                    }
                    _ => None,
                })
            } else {
                None
            };

            if needs_port && port_name.is_none() {
                println!(
                    "\n[{}  {}  {}  via {}]  no COM port found",
                    entry.busid, board.name, entry.vidpid, probe.tool_name()
                );
                continue;
            }

            // Serial probes show the COM port; USB-direct probes (FTDI / DFU /
            // pyocd / picotool / ST-Link …) identify the device by VID:PID.
            // (This used to print "(via libusb)", which confused users into
            // thinking the ST-Link wasn't on WinUSB — but libusb on Windows *is*
            // the WinUSB access path. VID:PID is unambiguous.)
            let header_port: &str = match port_name.as_deref() {
                Some(p) => p,
                None => entry.vidpid.as_str(),
            };
            println!(
                "\n[{}  {}  {}  via {}]",
                entry.busid, board.name, header_port, probe.tool_name()
            );

            let result = match probe {
                ProbeKind::Espflash => run_espflash_board_info(port_name.as_deref().unwrap()),
                ProbeKind::Avrdude { targets } => {
                    run_avrdude_query(port_name.as_deref().unwrap(), targets)
                }
                ProbeKind::Stm32Flash => run_stm32flash_query(port_name.as_deref().unwrap()),
                ProbeKind::Dfu => run_dfu_info(vid, pid),
                ProbeKind::Ftdi => run_ftdi_info(vid, pid),
                ProbeKind::Picotool => run_picotool_info(vid, pid),
                ProbeKind::Daplink => run_daplink_query(),
                ProbeKind::Pyocd => run_pyocd_query(vid, pid),
                ProbeKind::Stlink => run_stlink_controller_query(vid, pid),
                ProbeKind::StlinkTarget => run_stlink_target_query(vid, pid),
                ProbeKind::CmsisDap => run_cmsisdap_controller_query(vid, pid),
                ProbeKind::CmsisDapTarget => run_cmsisdap_target_query(vid, pid),
            };

            match result {
                Ok(info) if !info.is_empty() => {
                    if needs_port {
                        serial_chip_found = true;
                    }
                    match probe {
                        ProbeKind::Espflash => print_esp_info(&info),
                        ProbeKind::Avrdude { .. } => print_avr_info(&info, board.name),
                        ProbeKind::Stm32Flash => print_stm32_info(&info, board.name),
                        ProbeKind::Dfu => print_dfu_info(&info, board.name),
                        ProbeKind::Ftdi => print_ftdi_info(&info, board.name),
                        ProbeKind::Picotool => print_pico_info(&info, board.name),
                        ProbeKind::Daplink => print_daplink_info(&info, board.name),
                        ProbeKind::Pyocd => print_pyocd_info(&info),
                        ProbeKind::Stlink => print_stlink_controller_info(&info),
                        ProbeKind::StlinkTarget => print_stlink_target_info(&info, board.name),
                        ProbeKind::CmsisDap => print_stlink_controller_info(&info),
                        ProbeKind::CmsisDapTarget => print_stlink_target_info(&info, board.name),
                    }
                }
                Ok(_) => println!("  (no info parsed from output)"),
                Err(e) => {
                    let msg = e.to_string();
                    let mut lines = msg.lines();
                    if let Some(first) = lines.next() {
                        println!("  Error: {first}");
                        for line in lines {
                            println!("         {line}");
                        }
                    }
                }
            }
        }
    }

    print_probe_install_suggestions(candidates);
}

/// The driver a detected board needs so the host can talk to it (Windows only;
/// other OSes ship kernel modules / use udev). Returns `(what, install command)`.
pub(crate) fn driver_suggestion(vid: u16, pid: u16) -> Option<(&'static str, String)> {
    match vid {
        0x0483 if (0x3748..=0x3757).contains(&pid) => match stlink_driver_check(pid) {
            // Already bound to WinUSB — nothing to install.
            Some(StlinkDriver::WinUsb) => None,
            _ => Some((
                "WinUSB on the ST-Link MI_00 debug interface",
                "usbipd-rs --install-driver --confirm   (run in an Administrator shell)".into(),
            )),
        },
        0x0483 if pid == 0xdf11 => Some((
            "WinUSB for the STM32 DFU bootloader (Zadig)",
            "usbipd-rs --install zadig".into(),
        )),
        0x1a86 => Some(("WCH CH340 / CH9102 USB-serial driver", "usbipd-rs --install ch340".into())),
        0x10c4 => Some(("Silicon Labs CP210x VCP driver", "usbipd-rs --install cp210x".into())),
        0x0403 => Some(("FTDI VCP driver", "usbipd-rs --install ftdi".into())),
        0x2e8a => Some(("WinUSB on the RP2 BOOTSEL interface (Zadig)", "usbipd-rs --install zadig".into())),
        _ => None,
    }
}

/// The Rust-based flashing tool that fits a detected board, chosen by its first
/// probe kind. Returns `(what, cargo install command)`.
pub(crate) fn flasher_suggestion(board: &KnownBoard) -> Option<(&'static str, String)> {
    for p in board.probes {
        return Some(match p {
            ProbeKind::Espflash => ("espflash — ESP flashing & board-info (Rust)", "cargo install espflash".into()),
            ProbeKind::Avrdude { .. } => ("ravedude — AVR `cargo run` flasher (Rust)", "cargo install ravedude".into()),
            ProbeKind::Stlink
            | ProbeKind::StlinkTarget
            | ProbeKind::CmsisDap
            | ProbeKind::CmsisDapTarget
            | ProbeKind::Pyocd
            | ProbeKind::Daplink
            | ProbeKind::Stm32Flash
            | ProbeKind::Dfu
            | ProbeKind::Picotool => ("probe-rs — SWD/JTAG flash & debug (Rust)", "cargo install probe-rs-tools".into()),
            // FTDI is a descriptor-only probe; keep looking for a flashable kind.
            ProbeKind::Ftdi => continue,
        });
    }
    None
}

/// Print, at the end of `--probe`, the two things a user typically still needs:
/// (1) the driver so the host can reach the board, and (2) a Rust flashing tool.
pub(crate) fn print_probe_install_suggestions(candidates: &[(&Entry, &'static KnownBoard)]) {
    let mut flashers: Vec<(&str, String)> = Vec::new();
    for (_, b) in candidates {
        if let Some(s) = flasher_suggestion(b) {
            if !flashers.iter().any(|(_, c)| *c == s.1) {
                flashers.push(s);
            }
        }
    }

    println!("\n=== Suggested installs ===");

    println!("1. Driver — let the host talk to the board:");
    if current_os() == Os::Windows {
        let mut drivers: Vec<(&str, String)> = Vec::new();
        for (e, _) in candidates {
            let (vid, pid) = e.vidpid_pair();
            if let Some(s) = driver_suggestion(vid, pid) {
                if !drivers.iter().any(|(_, c)| *c == s.1) {
                    drivers.push(s);
                }
            }
        }
        if drivers.is_empty() {
            println!("   - detected board(s) already have a working driver.");
        } else {
            for (what, cmd) in drivers {
                println!("   - {what}");
                println!("       {cmd}");
            }
        }
    } else {
        println!("   - no Windows-style driver needed on this OS (kernel modules are built in).");
        println!("     For SWD probes, ensure udev rules / group permissions allow USB access.");
    }

    println!("2. Rust flashing tool:");
    if flashers.is_empty() {
        println!("   - no Rust flasher mapping for the detected board(s).");
    } else {
        for (what, cmd) in flashers {
            println!("   - {what}");
            println!("       {cmd}");
        }
    }
}

pub mod esp;
pub mod avr;
pub mod stm32flash;
pub mod ftdi;
pub mod dfu;
pub mod pico;
pub mod daplink;
pub(crate) use esp::*;
pub(crate) use avr::*;
pub(crate) use stm32flash::*;
pub(crate) use ftdi::*;
pub(crate) use dfu::*;
pub(crate) use pico::*;
pub(crate) use daplink::*;
