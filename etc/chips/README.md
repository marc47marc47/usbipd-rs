# Chip definitions (`*.chip`)

`usbipd-rs --mcu-alive-native` identifies a downstream STM32 from its DBGMCU
`DEV_ID`. The program ships a **built-in family table** covering the common
STM32F0/F1/F2/F3/F4/F7 parts, so it works with no files here at all.

These `*.chip` files are an **optional override / extension**: a model whose
`chip_id` matches one of these files takes **priority** over the built-in table,
and a `chip_id` the built-in table doesn't know becomes recognizable by dropping
a file here — no recompile needed.

## Search path

Loaded (in order) from:

1. `etc/chips/` relative to the current directory (running from a checkout)
2. `etc/chips/` and `chips/` next to the executable (a packaged binary)

## Format

A subset of the [stlink-org/stlink](https://github.com/stlink-org/stlink)
`.chip` format — real stlink chip files work unmodified; unknown keys are
ignored. Keys this tool reads:

| Key              | Meaning                                                        |
|------------------|---------------------------------------------------------------|
| `chip_id`        | DBGMCU `DEV_ID` (low 12 bits of `0xE0042000`) — the match key  |
| `dev_type`       | Human name, shown as "Device ID"                              |
| `flash_size_reg` | Address of the 16-bit flash-size (F_SIZE) field               |
| `sram_size`      | Main SRAM in bytes (shown in KB)                             |

`#` and `//` start comments. Integers may be `0x` hex or decimal.

The UID and read-protection register addresses always come from the built-in
family table (they are family-specific and verified), so a `.chip` for a family
the built-in table doesn't cover will still show name / flash size / SRAM / core
/ DEV_ID, just without UID/RDP.

See `F446.chip`, `F103xB.chip`, and `F401xC.chip` for templates.
