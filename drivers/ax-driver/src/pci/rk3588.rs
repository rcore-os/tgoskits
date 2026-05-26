pub(super) use super::{register_fdt_legacy_irq, set_pcie_mem_range};

mod body {
    use log::{debug, info, warn};

    include!("rk3588/resources.rs");
    include!("rk3588/clocks_reset_gpio.rs");
    include!("rk3588/phy.rs");
    include!("rk3588/windows.rs");
    include!("rk3588/slots.rs");
}
