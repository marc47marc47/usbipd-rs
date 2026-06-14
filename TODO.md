# TODO

## Goal

Build a reliable first-contact workflow for unfamiliar USB development boards:
prove the USB-facing hardware is alive, identify missing or broken Windows
function drivers, locate the official driver, and only then test the downstream
MCU without destructive operations.

## Confirmed Findings

- [x] Native USB enumeration no longer depends on `usbipd.exe`.
- [x] `nusb` can read cached device/configuration/interface/endpoint
  descriptors without the ST-Link debug-interface driver.
- [x] Current device is ST-Link/V2-1 `0483:374b`, serial
  `0673FF575053787067081551`.
- [x] Composite parent, `MI_01` Mass Storage, and `MI_02` COM6 are healthy.
- [x] `MI_00 ST-Link Debug` has Windows Problem Code 28 and no function driver.
- [x] `nusb`, `probe-rs`, pyOCD, and `stlink-org/stlink` cannot send commands
  through `MI_00` until Windows exposes an accessible function-driver path.
- [x] `stlink-org/stlink` confirms V2/V2-1 uses raw USB commands on `MI_00`.
  Only obsolete ST-Link/V1 transports commands through Mass Storage/SCSI.
- [x] The mounted MBED drive exposes `F:\DETAILS.TXT`, currently containing
  only interface firmware version/build information.

## Next Work

### Driver Diagnostics

- [x] Add detection of locally available matching INF packages before
  recommending downloads. `--driver-status` now reports bundled-INF
  availability and whether the package is already staged in the Windows
  driver store (`pnputil /enum-drivers`, locale-independent parsing).
- [x] Distinguish missing driver, wrong driver, stopped device node, stale
  upper filter, signature failure, and driver-store duplication.
  `classify_driver_issue()` maps `CM_PROB_*` codes to MissingDriver /
  DriverLoadFailure (covers stale binding) / StoppedNode / SignatureFailure /
  Blocked / ResourceConflict, each with a safe next action. Driver-store
  duplication is surfaced when more than one `oemNN.inf` matches.
- [x] Add an explicit dry-run installation report showing the exact interface
  that would be modified. `--install-driver` (no `--confirm`) prints the
  target instance id, classified current state, INF path, and the exact
  `pnputil` command, and writes nothing.
- [x] Add optional, separately confirmed installation of the official
  `stlink_dbg_winusb.inf` for `MI_00` only. `--install-driver --confirm`
  (alias `-y`) filters strictly to `&MI_00` interfaces with a bundled INF, so
  Mass Storage / COM / composite functions can never be touched.
- [~] Verify uninstall/reinstall can recover without rebooting.
  `--install-driver --confirm` re-queries the same instance after `pnputil`
  and reports PASS (recovered, no reboot) / PARTIAL / re-enumerated. Still
  needs a real Administrator-shell run on the bench to confirm end-to-end.

### Hardware Evidence Without MI_00

- [ ] Include mounted Mass Storage metadata and `DETAILS.TXT` in
  `--driver-status`.
- [ ] Map USB serial, volume, COM port, and composite interfaces to one physical
  device record.
- [ ] Investigate whether board model can be inferred safely from MBED volume,
  firmware build, USB strings, or serial format.
- [ ] Clearly separate evidence for the ST-Link controller from evidence for
  the downstream target MCU.

### Minimal MCU Test

- [x] Add `--mcu-alive` with staged USB/driver/SWD reporting.
- [x] Add `--mcu-alive-native`: native ST-Link bulk protocol over `nusb` (no
  probe-rs / pyocd / stlink). Implements GET_VERSION, GET_TARGET_VOLTAGE,
  ENTER_SWD, READ_IDCODES, READDEBUGREG; reads DPIDR / CPUID / DBGMCU IDCODE +
  flash/UID/RDP read-only and reuses `decode_stlink_regs`. Protocol
  cross-checked against probe-rs, stlink-org/stlink, and OpenOCD.
- [x] Empirically confirmed the hard boundary: with no WinUSB on `MI_00`,
  `nusb` opens the device and reads descriptors but `claim_interface` fails
  ("could not determine driver for interface") — user-mode cannot issue any
  bulk/SWD transfer. The minimal route to send SWD is WinUSB on `MI_00` only
  (`--install-driver`), then `--mcu-alive-native` needs no external tool.
- [x] Run `--mcu-alive-native` end-to-end after WinUSB is bound, with an STM32
  wired to SWD. Confirmed on a Nucleo-F446: DPIDR 0x2BA01477, CPUID
  0x410FC241 (Cortex-M4 r0p1), DEV_ID 0x421, 512 KB flash, 128 KB SRAM, UID,
  RDP Level 0 — all read-only, no probe-rs/pyocd.
- [x] Externalize the STM32 table: built-in `stm_family` stays the compiled-in
  base; optional `etc/chips/*.chip` files (stlink subset format) extend/override
  it, a matching `.chip` wins. Templates under `etc/chips/`; `pack-release.sh`
  bundles them. Verified the F446 read sources `etc/chips/F446.chip`.

- [x] Remove the `--probe-layer2` flag and fold layer 2 into `--probe`. ST-Link
  layer 2 now runs automatically, auto-detects whether a SWD/JTAG target is
  present, and shows one or two layers accordingly. It uses the read-only native
  SWD reader (not the old pyocd path, which halted/reset), so auto-running it is
  safe. Deleted the orphaned pyocd layer-2 stack (`pyocd_read`,
  `run_pyocd_commands`, `parse_dap_register`, `diagnose_failure`,
  `pyocd_error_line`, `parse_reg_dump`) and `should_probe_stlink_target`.

### Windows WinUSB native-transfer notes (learned)

- `claim_interface` failing with "could not determine driver for interface" is
  the definitive no-driver boundary; descriptors stay readable, transfers don't.
- WinUSB bulk IN length must be a multiple of the endpoint max packet size.
- After an aborted command, `clear_halt` + a short IN drain resync the FSM;
  a physical replug also clears it.
- The flash-size field is a non-word-aligned 16-bit register on F4/F7; read the
  aligned word and pick the half-word (matches stlink `common_legacy.c`).
- Some ST-Link/V2 clones STALL unsupported commands (e.g. GET_TARGET_VOLTAGE
  0xF7). A STALL halts the endpoint; without a CLEAR_FEATURE every later command
  fails (the OUT then just times out). `cmd()` now clears both halts after a
  stall, so an unsupported optional command no longer wedges ENTER_SWD. Also
  leave DFU/SWIM/DEBUG mode before ENTER_SWD (bare V2 often enumerates in DFU).
- [ ] After installing a valid `MI_00` binding, verify `--mcu-alive` reaches
  the target at 100 kHz.
- [ ] Report ST-Link firmware version and target voltage before SWD discovery.
- [ ] Read and report only the minimum target identity: SWD DPIDR, AP IDR,
  CPUID, and STM32 DBGMCU IDCODE.
- [ ] Confirm the minimal path does not halt, reset, erase, unlock, or write
  target memory.
- [ ] Compare results from `probe-rs`, pyOCD, and `stlink-org/stlink`.

### Quality and Documentation

- [x] Add parser/classification tests for Windows PnP problem codes and driver
  recommendations. `cargo test` covers `classify_driver_issue`,
  `known_driver_advice`, `instance_vidpid`, and the `pnputil` driver-store
  parser (`parse_driverstore_oem`); 12 tests pass.
- [x] Update stale statements in `CLAUDE.md`, including the old claim that the
  repository has no tests.
- [ ] Document a troubleshooting decision tree: USB absent, USB present but
  driver missing, probe opens but SWD fails, and target identity succeeds.
- [ ] Keep generated logs, firmware dumps, and downloaded driver bundles out of
  commits unless intentionally required.

## Acceptance Criteria

- [ ] A fresh board can be diagnosed without assuming it is defective merely
  because a Windows function driver is missing.
- [ ] Every failed stage reports the exact boundary and a safe next action.
- [ ] Destructive operations always require explicit opt-in.
