use crate::*;

pub(crate) fn cmd_driver_status() -> Result<()> {
    #[cfg(not(windows))]
    {
        println!("--driver-status currently reports Windows Plug and Play driver bindings only.");
        return Ok(());
    }

    #[cfg(windows)]
    {
        let devices: Vec<nusb::DeviceInfo> = nusb::list_devices()
            .wait()
            .context("failed to enumerate USB hardware through nusb")?
            .collect();
        let nodes = windows_usb_driver_nodes()?;

        println!("=== USB Hardware Evidence ===");
        println!("PASS - Windows USB hub enumeration returned {} physical USB device(s).", devices.len());
        println!("This proves USB signaling/enumeration works; it does not prove every function driver works.");

        println!("\n=== Windows USB Interface Drivers ===");
        let mut failures = 0usize;
        for node in &nodes {
            let health = if node.is_healthy() { "OK" } else { failures += 1; "NEEDS ATTENTION" };
            println!("\n[{health}] {}", if node.name.is_empty() { "(unnamed USB interface)" } else { &node.name });
            println!("  Instance: {}", node.instance_id);
            println!("  Class/service: {}/{}", value_or_dash(&node.class), value_or_dash(&node.service));
            println!("  Driver: {} / {} / {}", value_or_dash(&node.provider), value_or_dash(&node.inf), value_or_dash(&node.version));
            println!("  PnP status/problem: {} / {}", value_or_dash(&node.status), value_or_dash(&node.problem));
            if !node.is_healthy() {
                if let Some(issue) = classify_driver_issue(node) {
                    println!("  Classification: {} - {}", issue.label(), issue.meaning());
                    println!("  Safe next action: {}", issue.next_action());
                }
                if let Some((vid, pid)) = instance_vidpid(&node.instance_id) {
                    if let Some(device) = devices
                        .iter()
                        .find(|device| device.vendor_id() == vid && device.product_id() == pid)
                    {
                        println!(
                            "  Hardware evidence: PASS - nusb sees {:04x}:{:04x}, product {:?}, serial {:?}, speed {:?}",
                            vid,
                            pid,
                            device.product_string(),
                            device.serial_number(),
                            device.speed()
                        );
                        let interfaces = device
                            .interfaces()
                            .map(|interface| {
                                format!(
                                    "MI_{:02} class {:02x}",
                                    interface.interface_number(),
                                    interface.class()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("  Descriptor interfaces: {interfaces}");
                    }
                }
                if let Some(advice) = known_driver_advice(&node.instance_id) {
                    println!("  Known official driver: {}", advice.name);
                    println!("  Install: {}", advice.install);
                    report_local_driver_availability(&advice);
                } else {
                    println!("  Next step: search the exact VID:PID on the hardware vendor's support site or Microsoft Update Catalog.");
                }
            }
        }

        println!("\n=== Diagnosis ===");
        if failures == 0 {
            println!("All {} present USB PnP function(s) report healthy.", nodes.len());
        } else {
            println!("{failures} of {} present USB PnP function(s) need attention.", nodes.len());
            println!("A device listed under Hardware Evidence but failing here is primarily a Windows driver/binding problem, not proof of faulty hardware.");
            println!("This still cannot prove that every downstream MCU, sensor, or external circuit behind the USB controller is healthy.");
        }
        Ok(())
    }
}

/// `--install-driver`: dry-run by default, executing only with `--confirm`.
/// Restricted to the ST-Link `MI_00` debug interface (the only binding this
/// repo ships an INF for) so it can never touch the healthy Mass Storage /
/// COM / composite functions of the same device.
pub(crate) fn cmd_install_driver(confirm: bool) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = confirm;
        println!("--install-driver binds Windows USB function drivers and runs on Windows only.");
        return Ok(());
    }

    #[cfg(windows)]
    {
        println!("=== ST-Link MI_00 Driver Install ({}) ===", if confirm { "EXECUTE" } else { "DRY RUN" });
        println!("Scope: only the ST-Link Debug MI_00 interface is touched; Mass Storage, COM, and the composite parent are left alone.\n");

        let nodes = windows_usb_driver_nodes()?;
        let candidates: Vec<&WindowsUsbDriverNode> = nodes
            .iter()
            .filter(|n| n.instance_id.to_ascii_uppercase().contains("&MI_00"))
            .filter(|n| !n.is_healthy())
            .filter(|n| known_driver_advice(&n.instance_id).is_some_and(|a| a.local_inf.is_some()))
            .collect();

        if candidates.is_empty() {
            println!("No unhealthy MI_00 interface with a bundled INF was found.");
            println!("Run `usbipd-rs --driver-status` to inspect current bindings.");
            return Ok(());
        }

        let mut executed = 0usize;
        for node in candidates {
            let advice = known_driver_advice(&node.instance_id).expect("filtered to Some above");
            let rel = advice.local_inf.expect("filtered to Some above");
            let inf = match find_local_inf(rel) {
                Some(path) => path,
                None => {
                    println!("[SKIP] {}", node.instance_id);
                    println!("  Bundled INF missing at {rel}; cannot install offline.");
                    continue;
                }
            };

            println!("Interface that would be modified:");
            println!("  Instance: {}", node.instance_id);
            if let Some(issue) = classify_driver_issue(node) {
                println!("  Current state: {} (status {}, problem {})", issue.label(), value_or_dash(&node.status), value_or_dash(&node.problem));
            }
            println!("  Driver package: {}", advice.name);
            println!("  INF file: {}", inf.display());
            let inf_arg = inf.to_string_lossy();
            println!("  Command: pnputil /add-driver \"{inf_arg}\" /install");

            if !confirm {
                println!("  Action: none (dry run). Re-run with --confirm to apply.\n");
                continue;
            }

            println!("  Action: running pnputil ...");
            let output = Command::new("pnputil")
                .args(["/add-driver", inf_arg.as_ref(), "/install"])
                .output()
                .context("failed to launch pnputil (driver install needs an elevated/admin shell)")?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stdout.lines().chain(stderr.lines()).map(str::trim).filter(|l| !l.is_empty()) {
                println!("    {line}");
            }
            if !output.status.success() {
                println!("  Result: FAIL - pnputil exited with {}. An Administrator shell is required.\n", output.status);
                continue;
            }
            executed += 1;

            // Recovery check: re-query the same instance and confirm it is now
            // healthy *without* a reboot.
            let after = windows_usb_driver_nodes()?;
            match after.iter().find(|n| n.instance_id == node.instance_id) {
                Some(n) if n.is_healthy() => {
                    println!("  Result: PASS - {} is now {} / problem {} (recovered, no reboot).", n.instance_id, value_or_dash(&n.status), value_or_dash(&n.problem));
                }
                Some(n) => {
                    println!("  Result: PARTIAL - still {} / problem {}. Unplug and replug the device, or check it reports NEED_RESTART.", value_or_dash(&n.status), value_or_dash(&n.problem));
                }
                None => println!("  Result: node re-enumerated under a new instance id; re-run --driver-status to confirm."),
            }
            println!();
        }

        if confirm {
            println!("Installed {executed} driver binding(s). Verify with `usbipd-rs --driver-status` and `usbipd-rs --mcu-alive`.");
        } else {
            println!("Dry run complete. No changes were made. Re-run with --confirm in an Administrator shell to apply.");
        }
        Ok(())
    }
}

