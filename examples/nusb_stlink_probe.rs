use anyhow::{Context, Result};
use nusb::{DeviceInfo, MaybeFuture};

const ST_VID: u16 = 0x0483;

fn main() -> Result<()> {
    let devices = nusb::list_devices()
        .wait()
        .context("failed to enumerate USB devices")?;
    let mut found = false;

    for info in devices.filter(is_stlink_candidate) {
        found = true;
        inspect(info);
    }

    if !found {
        println!("No ST-Link candidate was found.");
    }

    Ok(())
}

fn is_stlink_candidate(info: &DeviceInfo) -> bool {
    info.vendor_id() == ST_VID
        && (info
            .product_string()
            .is_some_and(|name| name.to_ascii_lowercase().contains("st-link"))
            || (0x3744..=0x3757).contains(&info.product_id()))
}

fn inspect(info: DeviceInfo) {
    println!(
        "ST-Link candidate: {:04x}:{:04x} bus={} address={} ports={:?}",
        info.vendor_id(),
        info.product_id(),
        info.bus_id(),
        info.device_address(),
        info.port_chain()
    );
    println!("  manufacturer: {:?}", info.manufacturer_string());
    println!("  product:      {:?}", info.product_string());
    println!("  serial:       {:?}", info.serial_number());
    println!("  speed:        {:?}", info.speed());
    println!("  cached interfaces:");

    for interface in info.interfaces() {
        println!(
            "    MI_{:02}: class={:02x} subclass={:02x} protocol={:02x} name={:?}",
            interface.interface_number(),
            interface.class(),
            interface.subclass(),
            interface.protocol(),
            interface.interface_string()
        );
    }

    let device = match info.open().wait() {
        Ok(device) => device,
        Err(error) => {
            println!("  open device: FAIL - {error}");
            return;
        }
    };

    println!("  open device: OK");
    match device.active_configuration() {
        Ok(configuration) => println!("  active configuration:\n{configuration:#?}"),
        Err(error) => println!("  active configuration: FAIL - {error}"),
    }

    let debug_interface = info
        .interfaces()
        .find(|interface| interface.class() == 0xff)
        .map(|interface| interface.interface_number())
        .unwrap_or(0);

    match device.claim_interface(debug_interface).wait() {
        Ok(_) => println!(
            "  claim MI_{debug_interface:02}: OK - transfers are possible; no command was sent"
        ),
        Err(error) => println!(
            "  claim MI_{debug_interface:02}: FAIL - {error}\n\
             \n  Layer 2 unavailable: nusb cannot transfer data through this interface."
        ),
    }
}
