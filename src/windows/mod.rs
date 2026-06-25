use crate::*;


#[derive(Default)]
pub(crate) struct WindowsUsbDriverNode {
    status: String,
    class: String,
    name: String,
    instance_id: String,
    service: String,
    problem: String,
    inf: String,
    provider: String,
    version: String,
}

impl WindowsUsbDriverNode {
    fn is_healthy(&self) -> bool {
        self.status.eq_ignore_ascii_case("OK")
            && (self.problem.is_empty() || self.problem == "0" || self.problem == "CM_PROB_NONE")
    }

    pub(crate) fn set_field(&mut self, key: &str, value: &str) {
        let target = match key {
            "STATUS" => &mut self.status,
            "CLASS" => &mut self.class,
            "NAME" => &mut self.name,
            "INSTANCE" => &mut self.instance_id,
            "SERVICE" => &mut self.service,
            "PROBLEM" => &mut self.problem,
            "INF" => &mut self.inf,
            "PROVIDER" => &mut self.provider,
            "VERSION" => &mut self.version,
            _ => return,
        };
        *target = value.trim().to_string();
    }
}

#[cfg(windows)]
pub(crate) fn windows_usb_driver_nodes() -> Result<Vec<WindowsUsbDriverNode>> {
    let script = r#"
$ErrorActionPreference='SilentlyContinue'
function Prop($d, $key) {
  $p = Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName $key
  if ($null -ne $p.Data) { return ($p.Data -join ',') }
  return ''
}
Get-PnpDevice -PresentOnly |
  Where-Object { $_.InstanceId -like 'USB\VID_*' } |
  Sort-Object InstanceId |
  ForEach-Object {
    Write-Output '@@NODE@@'
    Write-Output ('STATUS=' + $_.Status)
    Write-Output ('CLASS=' + $_.Class)
    Write-Output ('NAME=' + $_.FriendlyName)
    Write-Output ('INSTANCE=' + $_.InstanceId)
    Write-Output ('SERVICE=' + (Prop $_ 'DEVPKEY_Device_Service'))
    Write-Output ('PROBLEM=' + (Prop $_ 'DEVPKEY_Device_ProblemCode'))
    Write-Output ('INF=' + (Prop $_ 'DEVPKEY_Device_DriverInfPath'))
    Write-Output ('PROVIDER=' + (Prop $_ 'DEVPKEY_Device_DriverProvider'))
    Write-Output ('VERSION=' + (Prop $_ 'DEVPKEY_Device_DriverVersion'))
  }
"#;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .context("failed to query Windows Plug and Play devices")?;
    if !output.status.success() {
        anyhow::bail!(
            "Windows Plug and Play query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut nodes = Vec::new();
    let mut current: Option<WindowsUsbDriverNode> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim();
        if line == "@@NODE@@" {
            if let Some(node) = current.take() {
                nodes.push(node);
            }
            current = Some(WindowsUsbDriverNode::default());
        } else if let Some((key, value)) = line.split_once('=') {
            if let Some(node) = current.as_mut() {
                node.set_field(key, value);
            }
        }
    }
    if let Some(node) = current {
        nodes.push(node);
    }
    Ok(nodes)
}

#[cfg(not(windows))]
pub(crate) fn windows_usb_driver_nodes() -> Result<Vec<WindowsUsbDriverNode>> {
    Ok(Vec::new())
}

/// Recommended official driver for an unhealthy interface, plus — when this
/// repository ships the matching package — the relative path to the bundled
/// INF so `--driver-status` can report "available locally" instead of blindly
/// telling the user to download.
pub(crate) struct DriverAdvice {
    pub(crate) name: &'static str,
    pub(crate) install: &'static str,
    /// Relative path (from the repo root / exe dir) to a bundled INF, if any.
    pub(crate) local_inf: Option<&'static str>,
}

pub(crate) fn known_driver_advice(instance_id: &str) -> Option<DriverAdvice> {
    let id = instance_id.to_ascii_uppercase();
    if id.contains("VID_0483&PID_374B&MI_00")
        || id.contains("VID_0483&PID_374A&MI_00")
        || id.contains("VID_0483&PID_374E&MI_00")
        || id.contains("VID_0483&PID_374F&MI_00")
    {
        return Some(DriverAdvice {
            name: "STMicroelectronics STSW-LINK009 (WinUSB binding for ST-Link Debug)",
            install: r#"pnputil /add-driver ".\windows-driver\stsw-link009\stlink_dbg_winusb.inf" /install"#,
            local_inf: Some("windows-driver/stsw-link009/stlink_dbg_winusb.inf"),
        });
    }
    if id.contains("VID_0483&PID_DF11") {
        return Some(DriverAdvice {
            name: "STM32CubeProgrammer driver package or WinUSB",
            install: "Install STM32CubeProgrammer, or bind this DFU interface to WinUSB with Zadig.",
            local_inf: None,
        });
    }
    if id.contains("VID_10C4&PID_EA") {
        return Some(DriverAdvice {
            name: "Silicon Labs CP210x Universal Windows Driver",
            install: "Download from https://www.silabs.com/developers/usb-to-uart-bridge-vcp-drivers",
            local_inf: None,
        });
    }
    if id.contains("VID_1A86&") {
        return Some(DriverAdvice {
            name: "WCH CH34x/CH91xx Windows driver",
            install: "Download from https://www.wch-ic.com/downloads/CH341SER_EXE.html",
            local_inf: None,
        });
    }
    if id.contains("VID_0403&") {
        return Some(DriverAdvice {
            name: "FTDI CDM/VCP driver",
            install: "Download from https://ftdichip.com/drivers/vcp-drivers/",
            local_inf: None,
        });
    }
    if id.contains("VID_2E8A&PID_0003") || id.contains("VID_2E8A&PID_000F") {
        return Some(DriverAdvice {
            name: "Microsoft WinUSB for Raspberry Pi BOOTSEL",
            install: "Use Zadig to bind only the RP2 BOOTSEL interface to WinUSB.",
            local_inf: None,
        });
    }
    None
}

/// Print whether the recommended driver is already on hand — bundled in this
/// repo and/or staged in the Windows driver store — so the user knows they can
/// install offline instead of downloading.
pub(crate) fn report_local_driver_availability(advice: &DriverAdvice) {
    let Some(rel) = advice.local_inf else { return };
    match find_local_inf(rel) {
        Some(path) => println!("  Local INF: AVAILABLE - {} (no download needed)", path.display()),
        None => println!("  Local INF: not bundled at {rel}; download required"),
    }
    #[cfg(windows)]
    if let Some(original) = Path::new(rel).file_name().and_then(|n| n.to_str()) {
        let staged = driverstore_matches(original);
        if staged.is_empty() {
            println!("  Driver store: no package staged from {original} yet");
        } else {
            println!("  Driver store: already staged as {}", staged.join(", "));
        }
    }
}

/// Look for a bundled INF on disk: first relative to the current working
/// directory (running from the repo), then next to the executable (running an
/// installed/copied binary). Returns the first path that exists.
pub(crate) fn find_local_inf(rel: &str) -> Option<PathBuf> {
    let cwd = PathBuf::from(rel);
    if cwd.is_file() {
        return Some(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Published `oemNN.inf` names in the Windows driver store whose package was
/// staged from `original_inf` (e.g. `stlink_dbg_winusb.inf`). Parses
/// `pnputil /enum-drivers` locale-independently: it splits the output into
/// per-driver blocks and, for any block mentioning the original file name,
/// extracts the `oemNN.inf` token (which Windows assigns regardless of UI
/// language). An empty result means the package is not yet staged.
#[cfg(windows)]
pub(crate) fn driverstore_matches(original_inf: &str) -> Vec<String> {
    let output = match Command::new("pnputil").args(["/enum-drivers"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    parse_driverstore_oem(&String::from_utf8_lossy(&output.stdout), original_inf)
}

/// Pure parser for `pnputil /enum-drivers` output: returns the `oemNN.inf`
/// published names of every driver block that mentions `original_inf`. Split
/// out from `driverstore_matches` so it is testable on any OS and independent
/// of the locale-specific field labels Windows prints.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn parse_driverstore_oem(text: &str, original_inf: &str) -> Vec<String> {
    let target = original_inf.to_ascii_lowercase();
    let mut matches = Vec::new();
    for block in text.split("\r\n\r\n").flat_map(|b| b.split("\n\n")) {
        if !block.to_ascii_lowercase().contains(&target) {
            continue;
        }
        if let Some(oem) = block.split_whitespace().find(|token| {
            let t = token.trim_end_matches([',', ';']).to_ascii_lowercase();
            t.starts_with("oem") && t.ends_with(".inf")
        }) {
            let oem = oem.trim_end_matches([',', ';']).to_string();
            if !matches.contains(&oem) {
                matches.push(oem);
            }
        }
    }
    matches
}

pub(crate) fn instance_vidpid(instance_id: &str) -> Option<(u16, u16)> {
    let id = instance_id.to_ascii_uppercase();
    let vid = id.split("VID_").nth(1)?.get(..4)?;
    let pid = id.split("PID_").nth(1)?.get(..4)?;
    Some((
        u16::from_str_radix(vid, 16).ok()?,
        u16::from_str_radix(pid, 16).ok()?,
    ))
}

/// A coarse, actionable category for *why* a present USB interface is
/// unhealthy, derived from its Windows PnP problem code, bound service, and
/// status. The point is to separate "Windows never bound a driver" from
/// "a driver is bound but broken" so the next step is unambiguous.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DriverIssue {
    MissingDriver,
    DriverLoadFailure,
    StoppedNode,
    SignatureFailure,
    Blocked,
    ResourceConflict,
    Unknown,
}

impl DriverIssue {
    fn label(self) -> &'static str {
        match self {
            DriverIssue::MissingDriver => "MISSING FUNCTION DRIVER",
            DriverIssue::DriverLoadFailure => "DRIVER FAILED TO LOAD",
            DriverIssue::StoppedNode => "DEVICE NODE STOPPED/DISABLED",
            DriverIssue::SignatureFailure => "DRIVER SIGNATURE REJECTED",
            DriverIssue::Blocked => "DRIVER BLOCKED",
            DriverIssue::ResourceConflict => "RESOURCE CONFLICT",
            DriverIssue::Unknown => "UNCLASSIFIED PROBLEM",
        }
    }

    fn meaning(self) -> &'static str {
        match self {
            DriverIssue::MissingDriver => {
                "Windows enumerated the interface but bound no function driver (no service)."
            }
            DriverIssue::DriverLoadFailure => {
                "A driver is assigned but its service could not start: stale binding, failed driver entry, or a prior unload."
            }
            DriverIssue::StoppedNode => {
                "The node is disabled, not started, or waiting for a restart."
            }
            DriverIssue::SignatureFailure => {
                "The matched driver's digital signature could not be verified."
            }
            DriverIssue::Blocked => {
                "Windows blocked this driver under a known-bad list or security policy."
            }
            DriverIssue::ResourceConflict => {
                "The device reports an I/O, memory, or IRQ resource conflict."
            }
            DriverIssue::Unknown => "The problem code does not map to a known category.",
        }
    }

    fn next_action(self) -> &'static str {
        match self {
            DriverIssue::MissingDriver => {
                "Bind the correct function driver (see below). This is a driver gap, not a hardware fault."
            }
            DriverIssue::DriverLoadFailure => {
                "Remove the stale binding (pnputil /delete-driver) then reinstall the correct INF."
            }
            DriverIssue::StoppedNode => {
                "Enable/restart the node in Device Manager; reboot only if it reports NEED_RESTART."
            }
            DriverIssue::SignatureFailure => {
                "Install a properly signed driver package from the vendor."
            }
            DriverIssue::Blocked => {
                "Do not force-load. Obtain an updated, unblocked driver from the vendor."
            }
            DriverIssue::ResourceConflict => {
                "Move the device to another port/hub and check for a conflicting device."
            }
            DriverIssue::Unknown => {
                "Inspect the raw problem code for this instance in Device Manager."
            }
        }
    }
}

/// Map an unhealthy node to a `DriverIssue`. Returns `None` when the node is
/// healthy. The numeric values are Windows `CM_PROB_*` configuration-manager
/// problem codes reported via `DEVPKEY_Device_ProblemCode`.
pub(crate) fn classify_driver_issue(node: &WindowsUsbDriverNode) -> Option<DriverIssue> {
    if node.is_healthy() {
        return None;
    }
    let no_service = node.service.trim().is_empty();
    let code: u32 = node.problem.trim().parse().unwrap_or(0);
    let issue = match code {
        // CM_PROB_NOT_CONFIGURED / CM_PROB_FAILED_INSTALL: no driver vs broken.
        1 | 28 => {
            if no_service {
                DriverIssue::MissingDriver
            } else {
                DriverIssue::DriverLoadFailure
            }
        }
        // Disabled, not started, needs restart, phantom, etc.
        10 | 14 | 19 | 21 | 22 | 24 | 45 => DriverIssue::StoppedNode,
        // Stale binding, failed driver entry, failed/aborted load.
        31 | 37 | 38 | 39 => DriverIssue::DriverLoadFailure,
        // CM_PROB_UNSIGNED_DRIVER.
        52 => DriverIssue::SignatureFailure,
        // Blocked driver / boot-time blocked.
        43 | 44 | 48 => DriverIssue::Blocked,
        // CM_PROB_NORMAL_CONFLICT.
        12 => DriverIssue::ResourceConflict,
        // Status not OK but no code: empty service ⇒ missing, else unknown.
        0 if no_service => DriverIssue::MissingDriver,
        _ => DriverIssue::Unknown,
    };
    Some(issue)
}


pub mod status;
pub(crate) use status::*;
