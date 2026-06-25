use crate::*;

/// What the ST-Link debug interface's WinUSB binding looks like on Windows.
pub(crate) enum StlinkDriver {
    /// Bound to WinUSB — pyocd/libusb can open it. (A stale/broken WinUSB bind
    /// still reports as this; only a runtime probe failure exposes that case.)
    WinUsb,
    /// Present but bound to some other service (e.g. `usbccgp` / a vendor
    /// driver / nothing) — pyocd cannot open it until it's switched to WinUSB.
    Other(String),
}

/// Check which driver the ST-Link *debug* interface is bound to. pyocd/libusb
/// can only open it when it's WinUSB (installed via Zadig — `--install zadig`);
/// a wrong/missing driver makes every SWD read fail with a misleading
/// "No device connected", so we look *before* probing and tell the user how to
/// fix it. Returns `None` when the binding can't be determined (then we probe
/// anyway rather than block on a guess). Windows-only — pyocd uses libusb/udev
/// elsewhere, so there is nothing to check.
#[cfg(windows)]
pub(crate) fn stlink_driver_check(pid: u16) -> Option<StlinkDriver> {
    // Match the debug interface: MI_00 on the composite V2-1/V3. Fall back to
    // the bare device (no MI_xx) only for the single-interface V2 — otherwise
    // the composite *parent* (also has no MI_, service `usbccgp`) gets picked
    // and we'd misreport the debug interface as not-WinUSB.
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         $all = Get-PnpDevice -PresentOnly | Where-Object {{ $_.InstanceId -match 'VID_0483&PID_{pid:04X}' }};\
         $d = $all | Where-Object {{ $_.InstanceId -match '&MI_00' }} | Select-Object -First 1;\
         if (-not $d) {{ $d = $all | Where-Object {{ $_.InstanceId -notmatch '&MI_' }} | Select-Object -First 1 }};\
         if ($d) {{\
           $svc=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_Service').Data;\
           $node=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DevNodeStatus').Data;\
           $problem=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data;\
           $filters=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_UpperFilters').Data -join ',';\
           $inf=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data;\
           $provider=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverProvider').Data;\
           $version=(Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverVersion').Data;\
           Write-Output \"SERVICE=$svc\"; Write-Output \"DEVNODE=$node\"; Write-Output \"PROBLEM=$problem\"; Write-Output \"FILTERS=$filters\";\
           Write-Output \"INF=$inf\"; Write-Output \"PROVIDER=$provider\"; Write-Output \"VERSION=$version\"\
         }}"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let svc = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("SERVICE="))?
        .trim()
        .to_string();
    let started = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("DEVNODE="))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|flags| flags & 0x8 != 0)
        .unwrap_or(true);
    let problem = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("PROBLEM="))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let field = |name: &str| {
        stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .unwrap_or("?")
            .trim()
    };
    if svc.is_empty() {
        return Some(StlinkDriver::Other(format!(
            "no function driver is bound (problem code {problem}); INF {}, provider {}, version {}",
            value_or_dash(field("INF=")),
            value_or_dash(field("PROVIDER=")),
            value_or_dash(field("VERSION="))
        )));
    }
    Some(if svc.eq_ignore_ascii_case("WinUSB") && started {
        StlinkDriver::WinUsb
    } else if svc.eq_ignore_ascii_case("WinUSB") {
        let filters = field("FILTERS=");
        let conflict = if filters.is_empty() {
            String::new()
        } else {
            format!("; conflicting upper filter(s): {filters}")
        };
        StlinkDriver::Other(format!(
            "WinUSB device node stopped (problem code {problem}){conflict}; INF {}, provider {}, version {}",
            field("INF="),
            field("PROVIDER="),
            field("VERSION=")
        ))
    } else {
        StlinkDriver::Other(svc)
    })
}

#[cfg(not(windows))]
pub(crate) fn stlink_driver_check(_pid: u16) -> Option<StlinkDriver> {
    None
}

