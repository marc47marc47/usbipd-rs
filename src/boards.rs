
/// One avrdude connection attempt: an MCU paired with the bootloader
/// programmer that drives it, plus the sync baud rates to try (in order).
/// FT232R-class boards don't reveal which baud the bootloader uses, so
/// `bauds` may hold several candidates.
#[derive(Clone, Copy)]
pub(crate) struct AvrTarget {
    pub(crate) mcu: &'static str,
    pub(crate) programmer: &'static str,
    pub(crate) bauds: &'static [u32],
}

#[derive(Clone, Copy)]
pub(crate) enum ProbeKind {
    Espflash,
    /// Try each `AvrTarget` in order; the first that syncs wins. A board on a
    /// dedicated Arduino VID:PID passes exactly one target; a board behind a
    /// generic USB-UART bridge passes several, since the bridge can't reveal
    /// whether a Uno-class (STK500v1) or Mega-class (STK500v2) MCU is wired
    /// to it.
    Avrdude { targets: &'static [AvrTarget] },
    /// STM32 / GD32 system-memory UART bootloader probe (`stm32flash`).
    /// Requires the chip to be in bootloader mode (BOOT0=HIGH at reset);
    /// fails fast (~3s) when no chip responds, so safe to chain.
    Stm32Flash,
    /// STM32 USB DFU (DfuSe) bootloader probe via `dfu-util -l`. The ROM
    /// bootloader presents as native USB (VID:PID 0483:DF11), not a COM port,
    /// so this is read-only descriptor listing — alt settings + per-region
    /// memory layout (flash base/size, option bytes) — with no chip reset.
    Dfu,
    /// Read FTDI device descriptors via nusb (manufacturer / product /
    /// serial / bcdDevice chip variant / Windows driver binding). Read-only,
    /// no COM port, no chip reset — safe to chain before any serial probe.
    Ftdi,
    Picotool,
    Daplink,
    Pyocd,
    /// Identify the USB-visible ST-Link debug controller itself (layer 1) from
    /// its VID:PID and cached USB descriptors. This is deliberately
    /// non-invasive: it does not open the debug interface, issue SWD/JTAG
    /// commands, halt a target, or reset either MCU.
    Stlink,
    /// Explicit opt-in downstream SWD target probe (layer 2). Reads identity,
    /// flash-size, UID, and read-protection registers, then resets the target
    /// to resume firmware. Never erases, unlocks, dumps, or writes flash.
    StlinkTarget,
    /// Identify a CMSIS-DAP debug probe itself (layer 1) — e.g. the RP2040-based
    /// Raspberry Pi Debug Probe / picoprobe — from VID:PID + USB descriptors.
    CmsisDap,
    /// Downstream SWD target read (layer 2) through a CMSIS-DAP probe, spoken
    /// natively over the CMSIS-DAP v2 bulk protocol via nusb. Read-only: brings
    /// up SWD and reads ID registers only; no halt, reset, erase, or write.
    CmsisDapTarget,
}

impl ProbeKind {
    pub(crate) fn needs_serial_port(&self) -> bool {
        matches!(
            self,
            ProbeKind::Espflash | ProbeKind::Avrdude { .. } | ProbeKind::Stm32Flash
        )
    }

    pub(crate) fn tool_name(&self) -> &'static str {
        match self {
            ProbeKind::Espflash => "espflash",
            ProbeKind::Avrdude { .. } => "avrdude",
            ProbeKind::Stm32Flash => "stm32flash",
            ProbeKind::Dfu => "dfu-util",
            ProbeKind::Ftdi => "nusb",
            ProbeKind::Picotool => "picotool",
            ProbeKind::Daplink => "DETAILS.TXT",
            ProbeKind::Pyocd => "pyocd",
            ProbeKind::Stlink => "nusb (layer 1 only)",
            ProbeKind::StlinkTarget => "native SWD (layer 2)",
            ProbeKind::CmsisDap => "nusb (layer 1, CMSIS-DAP)",
            ProbeKind::CmsisDapTarget => "native CMSIS-DAP SWD (layer 2)",
        }
    }

    /// Short tag shown in the "Probable boards detected" listing.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            ProbeKind::Espflash => "ESP",
            ProbeKind::Avrdude { .. } => "AVR",
            ProbeKind::Stm32Flash => "STM32",
            ProbeKind::Dfu => "DFU",
            ProbeKind::Ftdi => "FTDI",
            ProbeKind::Picotool => "RP2",
            ProbeKind::Daplink => "DAP",
            ProbeKind::Pyocd => "SWD",
            ProbeKind::Stlink => "STL",
            ProbeKind::StlinkTarget => "SWD2",
            ProbeKind::CmsisDap => "DAP",
            ProbeKind::CmsisDapTarget => "SWD2",
        }
    }
}

pub(crate) struct KnownBoard {
    pub(crate) vid: u16,
    pub(crate) pid: u16,
    pub(crate) name: &'static str,
    pub(crate) probes: &'static [ProbeKind],
}

// ── avrdude connection profiles, keyed by Arduino bootloader family ─────
pub(crate) const AVR_UNO:  AvrTarget = AvrTarget { mcu: "atmega328p", programmer: "arduino", bauds: &[115200] };
pub(crate) const AVR_MEGA: AvrTarget = AvrTarget { mcu: "atmega2560", programmer: "wiring",  bauds: &[115200] };
pub(crate) const AVR_32U4: AvrTarget = AvrTarget { mcu: "atmega32u4", programmer: "avr109",  bauds: &[57600] };

// Reusable probe pipelines
pub(crate) const ESP: &[ProbeKind] = &[ProbeKind::Espflash];
pub(crate) const PICO: &[ProbeKind] = &[ProbeKind::Picotool];
pub(crate) const DFU: &[ProbeKind] = &[ProbeKind::Dfu];
pub(crate) const DAP_PIPELINE: &[ProbeKind] = &[ProbeKind::Daplink, ProbeKind::Pyocd];
pub(crate) const STLINK: &[ProbeKind] = &[ProbeKind::Stlink, ProbeKind::StlinkTarget];
pub(crate) const CMSISDAP: &[ProbeKind] = &[ProbeKind::CmsisDap, ProbeKind::CmsisDapTarget];

// A generic USB-UART bridge (CH340, CP210x, FT2232/FT232H/FT231X) carries no
// information about which MCU is wired to its TX/RX lines — the same chip
// ships on ESP, STM32/GD32, and Arduino (Uno/Nano/Mega) clones alike. Probe
// in fail-fast order:
//   1. espflash — fast on real ESP via DTR/RTS auto-bootloader (~2s).
//   2. stm32flash — needs user to manually pull BOOT0=HIGH + RESET, but fails
//      in ~3s when no STM32/GD32 responds. Catches GD32F103 / STM32 dev
//      boards that don't auto-reset (most generic boards don't wire DTR/RTS
//      to BOOT0/RESET).
//   3. avrdude — slowest (multiple programmer+baud combos, ~10s when failing),
//      but the only way to detect Arduino-class boards. Tries both bootloader
//      dialects: STK500v1 ("arduino", Uno/Nano-class) and STK500v2 ("wiring",
//      Mega 2560).
// probe_boards() stops at the first serial probe that syncs, so a real ESP
// never reaches stm32flash; a real GD32 in bootloader never reaches avrdude.
pub(crate) const BRIDGE: &[ProbeKind] = &[
    ProbeKind::Espflash,
    ProbeKind::Stm32Flash,
    ProbeKind::Avrdude {
        targets: &[
            // Uno/Nano-class: bootloader baud varies by clone age → try the
            // modern 115200 first, then the legacy 57600.
            AvrTarget { mcu: "atmega328p", programmer: "arduino", bauds: &[115200, 57600] },
            AVR_MEGA,
        ],
    },
];

pub(crate) const KNOWN_BOARDS: &[KnownBoard] = &[
    // ── Arduino official boards (probe via avrdude) ──────────────────────
    KnownBoard { vid: 0x2341, pid: 0x0001, name: "Arduino Uno R1",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_UNO] }] },
    KnownBoard { vid: 0x2341, pid: 0x0043, name: "Arduino Uno R3",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_UNO] }] },
    KnownBoard { vid: 0x2341, pid: 0x0010, name: "Arduino Mega 2560",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_MEGA] }] },
    KnownBoard { vid: 0x2341, pid: 0x0042, name: "Arduino Mega 2560 R3",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_MEGA] }] },
    KnownBoard { vid: 0x2341, pid: 0x0044, name: "Arduino Mega ADK",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_MEGA] }] },
    KnownBoard { vid: 0x2341, pid: 0x8036, name: "Arduino Leonardo",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_32U4] }] },
    KnownBoard { vid: 0x2341, pid: 0x8037, name: "Arduino Micro",
        probes: &[ProbeKind::Avrdude { targets: &[AVR_32U4] }] },

    // ── Classic Arduino behind a bare FTDI FT232R USB-UART bridge ────────
    // FT232R (0403:6001) is a bare USB-serial chip — it can't tell us what MCU
    // sits on its TX/RX lines. Classic Arduinos that use it (Nano, Duemilanove)
    // speak the STK500v1 bootloader protocol, so probe via avrdude. We request
    // atmega328p (the common case); avrdude runs with -F, so a 328PB / 168 /
    // LGT8F328P clone still connects and reports its true signature instead of
    // erroring. Bootloader baud varies by board age → try 57600 then 115200.
    //
    // Always run the read-only Ftdi descriptor probe first so the user gets
    // useful info (chip variant, serial, driver binding) even when no Arduino
    // MCU is wired downstream and avrdude fails to sync.
    KnownBoard { vid: 0x0403, pid: 0x6001, name: "FT232R Arduino (ATmega328-class)",
        probes: &[
            ProbeKind::Ftdi,
            ProbeKind::Avrdude {
                targets: &[AvrTarget { mcu: "atmega328p", programmer: "arduino", bauds: &[57600, 115200] }],
            },
        ] },

    // ── Generic USB-UART bridges: an ESP *or* an Arduino may sit behind ──
    // them, so probe espflash first and fall back to avrdude (see BRIDGE).
    KnownBoard { vid: 0x10c4, pid: 0xea60, name: "CP2102/CP2102N", probes: BRIDGE },
    KnownBoard { vid: 0x10c4, pid: 0xea70, name: "CP2105",         probes: BRIDGE },
    KnownBoard { vid: 0x10c4, pid: 0xea71, name: "CP2108",         probes: BRIDGE },
    KnownBoard { vid: 0x1a86, pid: 0x7523, name: "CH340",          probes: BRIDGE },
    KnownBoard { vid: 0x1a86, pid: 0x55d4, name: "CH9102",         probes: BRIDGE },
    KnownBoard { vid: 0x0403, pid: 0x6010, name: "FT2232",         probes: BRIDGE },
    KnownBoard { vid: 0x0403, pid: 0x6014, name: "FT232H",         probes: BRIDGE },
    KnownBoard { vid: 0x0403, pid: 0x6015, name: "FT231X",         probes: BRIDGE },

    // ── ESP32 native USB (the ESP silicon itself presents USB) ───────────
    KnownBoard { vid: 0x303a, pid: 0x1001, name: "ESP32 USB-Serial-JTAG", probes: ESP },
    KnownBoard { vid: 0x303a, pid: 0x4001, name: "ESP32 USB-OTG",  probes: ESP },

    // ── STM32 system ROM USB DFU bootloader (DfuSe) (probe via dfu-util) ──
    // VID:PID 0483:DF11 is the on-chip DFU bootloader every STM32 with USB
    // exposes (entered via BOOT0=HIGH at reset, or a firmware "jump to
    // bootloader"). It's native USB, not a UART/COM port, so stm32flash can't
    // reach it — dfu-util reads the alt-setting memory map (flash base/size,
    // option bytes) read-only. On Windows the DFU interface needs a WinUSB
    // driver (run `usbipd-rs --install zadig`).
    KnownBoard { vid: 0x0483, pid: 0xdf11, name: "STM32 DFU Bootloader (DfuSe)", probes: DFU },

    // ── Raspberry Pi Pico (RP2040 / RP2350) (probe via picotool) ─────────
    KnownBoard { vid: 0x2e8a, pid: 0x0003, name: "RP2040 BOOTSEL (Pi Pico)",   probes: PICO },
    KnownBoard { vid: 0x2e8a, pid: 0x000f, name: "RP2350 BOOTSEL (Pi Pico 2)", probes: PICO },

    // ── DAPLink-based boards (BBC micro:bit, NXP FRDM, etc.) ─────────────
    // Read DETAILS.TXT from MSD first, then ask pyocd what target chip is on
    // the other end of the SWD lines (board database lookup; no chip reset).
    KnownBoard { vid: 0x0d28, pid: 0x0204, name: "DAPLink (mbed CMSIS-DAP)", probes: DAP_PIPELINE },

    // ── RP2040-based CMSIS-DAP debug probes (layer 1 = RP2040; layer 2 = SWD) ──
    // Native CMSIS-DAP v2 read of the downstream target — no probe-rs/pyocd.
    KnownBoard { vid: 0x2e8a, pid: 0x000c, name: "Raspberry Pi Debug Probe (RP2040 CMSIS-DAP)", probes: CMSISDAP },
    KnownBoard { vid: 0x2e8a, pid: 0x0004, name: "Picoprobe (RP2040 CMSIS-DAP)",               probes: CMSISDAP },

    // ── ST-Link debug controllers (layer 1 only; downstream target disabled) ──
    KnownBoard { vid: 0x0483, pid: 0x3748, name: "ST-Link/V2 debug controller",   probes: STLINK },
    KnownBoard { vid: 0x0483, pid: 0x374b, name: "ST-Link/V2-1 debug controller", probes: STLINK },
    KnownBoard { vid: 0x0483, pid: 0x374e, name: "ST-Link/V3 debug controller",   probes: STLINK },
    KnownBoard { vid: 0x0483, pid: 0x374f, name: "ST-Link/V3 debug controller",   probes: STLINK },
];

pub(crate) fn lookup_board(vid: u16, pid: u16) -> Option<&'static KnownBoard> {
    KNOWN_BOARDS.iter().find(|b| b.vid == vid && b.pid == pid)
}
