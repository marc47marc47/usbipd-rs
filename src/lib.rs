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
    match cli::parse(&args)? {
        Cli::Help => {
            print_help();
            Ok(())
        }
        Cli::Version => {
            println!("usbipd-rs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Cli::ListTools => cmd_list_tools(),
        Cli::DriverStatus => cmd_driver_status(),
        Cli::InstallDriver { confirm } => cmd_install_driver(confirm),
        Cli::McuAliveNative => cmd_mcu_alive_native(),
        Cli::McuAlive => cmd_mcu_alive(),
        Cli::Install { tool } => cmd_install(&tool),
        Cli::ListUsb { probe } => cmd_list_usb(probe),
    }
}

pub(crate) fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

