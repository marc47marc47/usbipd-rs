use crate::*;

/// What a single probe invocation needs: the device's VID:PID and, for serial
/// probes, the COM port it was found on.
pub(crate) struct ProbeCtx<'a> {
    pub(crate) vid: u16,
    pub(crate) pid: u16,
    pub(crate) port: Option<&'a str>,
}

/// A probe's parsed result, tagged by which formatter renders it. Adding a new
/// probe means adding a variant here plus an arm in `ProbeKind::run` and
/// `ProbeReport::print` — no scattered match sites elsewhere.
pub(crate) enum ProbeReport {
    Esp(HashMap<String, String>),
    Avr(HashMap<String, String>),
    Stm32Flash(HashMap<String, String>),
    Dfu(HashMap<String, String>),
    Ftdi(HashMap<String, String>),
    Pico(HashMap<String, String>),
    Daplink(HashMap<String, String>),
    Pyocd(HashMap<String, String>),
    StlinkController(StlinkControllerInfo),
    Target(TargetReport),
}

impl ProbeReport {
    pub(crate) fn is_empty(&self) -> bool {
        use ProbeReport::*;
        match self {
            Esp(m) | Avr(m) | Stm32Flash(m) | Dfu(m) | Ftdi(m) | Pico(m) | Daplink(m) | Pyocd(m) => {
                m.is_empty()
            }
            StlinkController(i) => i.is_empty(),
            Target(t) => t.is_empty(),
        }
    }

    pub(crate) fn print(&self, board: &str) {
        match self {
            ProbeReport::Esp(m) => print_esp_info(m),
            ProbeReport::Avr(m) => print_avr_info(m, board),
            ProbeReport::Stm32Flash(m) => print_stm32_info(m, board),
            ProbeReport::Dfu(m) => print_dfu_info(m, board),
            ProbeReport::Ftdi(m) => print_ftdi_info(m, board),
            ProbeReport::Pico(m) => print_pico_info(m, board),
            ProbeReport::Daplink(m) => print_daplink_info(m, board),
            ProbeReport::Pyocd(m) => print_pyocd_info(m),
            ProbeReport::StlinkController(i) => i.print(),
            ProbeReport::Target(t) => t.print(board),
        }
    }
}

impl ProbeKind {
    /// Run this probe against a device and return its parsed report. Serial
    /// probes (`Espflash`/`Avrdude`/`Stm32Flash`) require `ctx.port`.
    pub(crate) fn run(&self, ctx: &ProbeCtx) -> Result<ProbeReport> {
        let (vid, pid) = (ctx.vid, ctx.pid);
        Ok(match self {
            ProbeKind::Espflash => ProbeReport::Esp(run_espflash_board_info(ctx.port.unwrap())?),
            ProbeKind::Avrdude { targets } => {
                ProbeReport::Avr(run_avrdude_query(ctx.port.unwrap(), targets)?)
            }
            ProbeKind::Stm32Flash => ProbeReport::Stm32Flash(run_stm32flash_query(ctx.port.unwrap())?),
            ProbeKind::Dfu => ProbeReport::Dfu(run_dfu_info(vid, pid)?),
            ProbeKind::Ftdi => ProbeReport::Ftdi(run_ftdi_info(vid, pid)?),
            ProbeKind::Picotool => ProbeReport::Pico(run_picotool_info(vid, pid)?),
            ProbeKind::Daplink => ProbeReport::Daplink(run_daplink_query()?),
            ProbeKind::Pyocd => ProbeReport::Pyocd(run_pyocd_query(vid, pid)?),
            ProbeKind::Stlink => ProbeReport::StlinkController(run_stlink_controller_query(vid, pid)?),
            ProbeKind::StlinkTarget => ProbeReport::Target(run_stlink_target_query(vid, pid)?),
            ProbeKind::CmsisDap => {
                ProbeReport::StlinkController(run_cmsisdap_controller_query(vid, pid)?)
            }
            ProbeKind::CmsisDapTarget => ProbeReport::Target(run_cmsisdap_target_query(vid, pid)?),
        })
    }
}

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

            let ctx = ProbeCtx { vid, pid, port: port_name.as_deref() };
            match probe.run(&ctx) {
                Ok(report) if !report.is_empty() => {
                    if needs_port {
                        serial_chip_found = true;
                    }
                    report.print(board.name);
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
