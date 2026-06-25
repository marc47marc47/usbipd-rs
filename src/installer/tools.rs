use crate::*;


pub(crate) const PICOTOOL_WIN_URL: &str   = "https://github.com/raspberrypi/pico-sdk-tools/releases/download/v2.2.0-3/picotool-2.2.0-a4-x64-win.zip";
pub(crate) const PICOTOOL_MAC_URL: &str   = "https://github.com/raspberrypi/pico-sdk-tools/releases/download/v2.2.0-3/picotool-2.2.0-a4-mac.zip";
pub(crate) const PICOTOOL_LINUX_URL: &str = "https://github.com/raspberrypi/pico-sdk-tools/releases/download/v2.2.0-3/picotool-2.2.0-a4-x86_64-lin.tar.gz";

pub(crate) const PICOTOOL_LINUX_INSTRUCTIONS: &str = "Linux: the archive is .tar.gz (zip extraction skipped). Extract manually:\n\
  tar xzf <downloaded_file> -C ~/.local/bin/picotool/\n\
or install via your package manager (Ubuntu 24.04+: `sudo apt install picotool`).";

pub(crate) const ZADIG_URL: &str = "https://github.com/pbatard/libwdi/releases/download/v1.5.1/zadig-2.9.exe";

pub(crate) const ZADIG_INSTRUCTIONS: &str = "1. Right-click the downloaded zadig-2.9.exe and pick 'Run as administrator'.\n\
2. In Zadig: Options → check 'List All Devices'.\n\
3. Pick the BOOTSEL/PICOBOOT/RP2 Boot device from the dropdown.\n\
4. Set the right-side driver to 'WinUSB' and click 'Replace Driver'.\n\
5. Re-run usbipd-rs --probe to verify pi pico probing works.";

// ── Step-2 tools ──
pub(crate) const AVRDUDE_WIN_URL: &str =
    "https://github.com/avrdudes/avrdude/releases/download/v8.1/avrdude-v8.1-windows-x64.zip";

pub(crate) const CP210X_WIN_URL: &str =
    "https://www.silabs.com/documents/public/software/CP210x_Universal_Windows_Driver.zip";

pub(crate) const CP210X_INSTRUCTIONS: &str =
    "Silabs Universal Driver is .inf-based (no .exe installer). Two ways to install:\n\
     \n\
     A) Right-click the silabser.inf path printed above → 'Install'.\n\
        (Confirm any 'Open File' / UAC prompt.)\n\
     \n\
     B) From an admin PowerShell, run:\n\
          pnputil /add-driver \"<silabser.inf path>\" /install\n\
     \n\
     Replug your CP2102/CP2104 board after install — it should appear as a COM port.";

pub(crate) const CH340_WIN_URL: &str = "https://www.wch-ic.com/download/file?id=65";

pub(crate) const CH340_INSTRUCTIONS: &str =
    "1. Right-click CH341SER.EXE (path printed above) → 'Run as administrator'.\n\
     2. Click 'INSTALL' in the WCH installer dialog.\n\
     3. Replug the CH340-based board to bind the new driver.";

pub(crate) const FTDI_CDM_URL: &str =
    "https://ftdichip.com/wp-content/uploads/2021/08/CDM212364_Setup.zip";

pub(crate) const FTDI_INSTRUCTIONS: &str =
    "FTDI's web server returns HTTP 403 to non-browser clients, so this driver\n\
     cannot be auto-downloaded — fetch it in a browser instead:\n\
     \n\
     1. Open the URL above in a browser and save CDM212364_Setup.zip.\n\
     2. Extract it, then right-click CDM212364_Setup.exe → 'Run as administrator'.\n\
     3. Replug the FT232R board — it should appear as a COM port.\n\
     \n\
     You usually DON'T need this: Windows 10/11 installs the FTDI VCP driver\n\
     automatically via Windows Update. If your board already shows a COMx\n\
     port, the driver is already working — this entry is just for offline or\n\
     freshly-imaged machines.\n\
     \n\
     Chocolatey users can instead run:  choco install ftdi-drivers";

// stm32flash upstream lives on SourceForge whose download URLs require
// browser-side redirects, so instead of auto-downloading we ship the
// upstream v0.7 binary zip in windows-driver/ (227 KB, three-OS bundle:
// stm32flash.exe + stm32flash_linux + stm32flash_macos) and just extract it.
pub(crate) const STM32FLASH_BUNDLE: &str = "stm32flash-0.7-binaries.zip";

// arduino-cli ships per-OS archives; the `_latest_` URLs are stable (302 →
// current release). Windows is a .zip we can auto-extract; macOS uses Homebrew
// (matches avrdude); Linux is a .tar.gz the bundled zip crate can't open, so we
// download and print manual-extract steps (same pattern as picotool on Linux).
pub(crate) const ARDUINO_CLI_WIN_URL: &str =
    "https://downloads.arduino.cc/arduino-cli/arduino-cli_latest_Windows_64bit.zip";
pub(crate) const ARDUINO_CLI_LINUX_URL: &str =
    "https://downloads.arduino.cc/arduino-cli/arduino-cli_latest_Linux_64bit.tar.gz";
pub(crate) const ARDUINO_CLI_LINUX_INSTRUCTIONS: &str = "Linux: the archive is .tar.gz (zip extraction skipped). Extract manually:\n\
  tar xzf <downloaded_file> -C ~/.local/bin/\n\
or install via the official script:\n\
  curl -fsSL https://raw.githubusercontent.com/arduino/arduino-cli/master/install.sh | sh";

// dfu-util upstream ships only a SourceForge .tar.xz behind browser redirects,
// so Windows is a Manual step; macOS/Linux have it in brew / apt.
pub(crate) const DFU_UTIL_WIN_URL: &str = "https://dfu-util.sourceforge.net/releases/";

pub(crate) const DFU_UTIL_WIN_INSTRUCTIONS: &str =
    "Windows has no auto-installer for dfu-util (SourceForge serves a .tar.xz\n\
     behind browser redirects). Install manually:\n\
     \n\
     1. Open the URL above and download the latest dfu-util-<ver>-binaries.tar.xz.\n\
     2. Extract it (7-Zip handles .tar.xz); add the win64\\ folder to PATH, or\n\
        copy dfu-util.exe next to usbipd-rs.exe.\n\
     3. The STM32 DFU interface needs a WinUSB driver — run\n\
          usbipd-rs --install zadig\n\
        and replace the '0483:DF11 / STM32 BOOTLOADER' device driver with WinUSB.\n\
     \n\
     Chocolatey users can instead run:  choco install dfu-util";

pub(crate) const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: "espflash",
        name: "espflash",
        purpose: "ESP32/ESP8266 chip identification & flashing",
        resolve: |_os| Some(InstallStep::Command {
            program: "cargo",
            args: &["install", "espflash"],
        }),
        check_command: Some("espflash"),
    },
    ToolSpec {
        id: "pyocd",
        name: "pyOCD",
        purpose: "CMSIS-DAP / DAPLink target chip identification",
        resolve: |_os| Some(InstallStep::Command {
            program: "pip",
            args: &["install", "pyocd"],
        }),
        check_command: Some("pyocd"),
    },
    ToolSpec {
        id: "picotool",
        name: "picotool",
        purpose: "Raspberry Pi RP2040/RP2350 inspection & flashing",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: PICOTOOL_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "picotool.exe" },
            }),
            Os::Macos => Some(InstallStep::Download {
                url: PICOTOOL_MAC_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "picotool" },
            }),
            Os::Linux => Some(InstallStep::Download {
                url: PICOTOOL_LINUX_URL,
                filename: None,
                action: DownloadAction::PromptToRun { instructions: PICOTOOL_LINUX_INSTRUCTIONS },
            }),
        },
        check_command: None, // bundled — find_picotool() locates it
    },
    ToolSpec {
        id: "zadig",
        name: "Zadig",
        purpose: "Windows-only: replace a USB device's driver with WinUSB so libusb-based tools (picotool, etc.) can talk to it.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: ZADIG_URL,
                filename: None,
                action: DownloadAction::PromptToRun { instructions: ZADIG_INSTRUCTIONS },
            }),
            _ => None,
        },
        check_command: None,
    },

    // ── Step-2 tools ───────────────────────────────────────────────────
    ToolSpec {
        id: "avrdude",
        name: "avrdude",
        purpose: "AVR (Arduino Uno/Mega/Leonardo/Micro) chip ID & flashing",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: AVRDUDE_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "avrdude.exe" },
            }),
            Os::Macos => Some(InstallStep::Command {
                program: "brew",
                args: &["install", "avrdude"],
            }),
            Os::Linux => Some(InstallStep::Command {
                program: "sudo",
                args: &["apt-get", "install", "-y", "avrdude"],
            }),
        },
        check_command: Some("avrdude"),
    },
    ToolSpec {
        id: "stm32flash",
        name: "stm32flash",
        purpose: "STM32 / GD32 (and bootloader-compatible clones) chip ID & flashing via UART bootloader. Bundled v0.7 zip ships binaries for Windows, Linux, and macOS.",
        // The bundled zip carries all three OS binaries; the per-OS
        // binary_hint just tells the post-extract scan which file to highlight.
        resolve: |os| Some(InstallStep::LocalArchive {
            filename: STM32FLASH_BUNDLE,
            action: DownloadAction::ExtractToBundle {
                binary_hint: match os {
                    Os::Windows => "stm32flash.exe",
                    Os::Linux   => "stm32flash_linux",
                    Os::Macos   => "stm32flash_macos",
                },
            },
        }),
        // Bundled binary isn't on PATH — find_stm32flash() locates it at runtime
        // (similar to find_picotool()), so a `which`-style probe would be misleading.
        check_command: None,
    },
    ToolSpec {
        id: "dfu-util",
        name: "dfu-util",
        purpose: "STM32 USB DFU (DfuSe) bootloader (0483:DF11) memory-map / chip-family ID & flashing.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Manual {
                url: DFU_UTIL_WIN_URL,
                instructions: DFU_UTIL_WIN_INSTRUCTIONS,
            }),
            Os::Macos => Some(InstallStep::Command {
                program: "brew",
                args: &["install", "dfu-util"],
            }),
            Os::Linux => Some(InstallStep::Command {
                program: "sudo",
                args: &["apt-get", "install", "-y", "dfu-util"],
            }),
        },
        check_command: Some("dfu-util"),
    },
    ToolSpec {
        id: "ravedude",
        name: "ravedude",
        purpose: "avr-hal `cargo run` runner — wraps avrdude to flash Rust AVR firmware & open a serial monitor.",
        resolve: |_os| Some(InstallStep::Command {
            program: "cargo",
            args: &["install", "ravedude"],
        }),
        check_command: Some("ravedude"),
    },
    ToolSpec {
        id: "arduino-cli",
        name: "Arduino CLI",
        purpose: "Arduino board/core manager. `arduino-cli core install arduino:avr` then bundles avrdude (and esp32 core → esptool, etc.).",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: ARDUINO_CLI_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractToBundle { binary_hint: "arduino-cli.exe" },
            }),
            Os::Macos => Some(InstallStep::Command {
                program: "brew",
                args: &["install", "arduino-cli"],
            }),
            Os::Linux => Some(InstallStep::Download {
                url: ARDUINO_CLI_LINUX_URL,
                filename: None,
                action: DownloadAction::PromptToRun { instructions: ARDUINO_CLI_LINUX_INSTRUCTIONS },
            }),
        },
        check_command: Some("arduino-cli"),
    },
    ToolSpec {
        id: "cp210x",
        name: "Silabs CP210x VCP Driver",
        purpose: "Windows-only: USB Serial driver for ESP32 dev boards using CP2102/CP2104.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: CP210X_WIN_URL,
                filename: None,
                action: DownloadAction::ExtractAndPrompt {
                    binary_hint: "silabser.inf",
                    instructions: CP210X_INSTRUCTIONS,
                },
            }),
            Os::Macos => None,  // Mac CP210x driver is in-kernel since macOS 11+
            Os::Linux => None,  // Linux ships cp210x kernel module by default
        },
        check_command: None,
    },
    ToolSpec {
        id: "ch340",
        name: "WCH CH340/CH341 Driver",
        purpose: "Windows-only: USB Serial driver for cheap clone Arduinos & ESP boards using CH340G.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Download {
                url: CH340_WIN_URL,
                filename: Some("CH341SER.EXE"),
                action: DownloadAction::PromptToRun { instructions: CH340_INSTRUCTIONS },
            }),
            Os::Macos => None,  // WCH provides separate macOS package; out of scope
            Os::Linux => None,  // ch341 kernel module ships with mainline Linux
        },
        check_command: None,
    },
    ToolSpec {
        id: "ftdi",
        name: "FTDI VCP Driver (CDM)",
        purpose: "Windows-only: USB Serial (VCP) driver for FTDI FT232R/FT232RL — classic Arduinos & USB-UART adapters. Windows 10/11 usually installs it automatically.",
        resolve: |os| match os {
            Os::Windows => Some(InstallStep::Manual {
                url: FTDI_CDM_URL,
                instructions: FTDI_INSTRUCTIONS,
            }),
            Os::Macos => None,  // macOS ships an FTDI VCP driver in-kernel
            Os::Linux => None,  // Linux ftdi_sio kernel module ships by default
        },
        check_command: None,
    },
];

