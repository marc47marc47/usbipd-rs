// Thin binary shim. All logic lives in the `usbipd_rs` library crate
// (src/lib.rs and its submodules); this just dispatches into it.
fn main() -> anyhow::Result<()> {
    usbipd_rs::run()
}
