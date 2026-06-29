pub(crate) fn probe_all_devices() {
    info!("Probe platform devices...");
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip platform device probe");
        return;
    }
    rdrive::probe_all(false)
        .unwrap_or_else(|err| panic!("failed to probe platform devices: {err:?}"));
}

#[cfg(all(feature = "display", feature = "plat-dyn"))]
pub(crate) fn init_dyn_display() {
    if !rdrive::is_initialized() {
        ax_display::init_display(core::iter::empty::<ax_display::ErasedDisplayDevice>());
        return;
    }
    let devices = ax_driver::display::take_display_devices()
        .unwrap_or_else(|err| panic!("failed to open display devices: {err:?}"))
        .into_iter()
        .map(|dev| {
            let display = ax_display::rdif::RdifDisplayDevice::new(dev)
                .unwrap_or_else(|err| panic!("failed to adapt display device: {err:?}"));
            ax_display::ErasedDisplayDevice::new(display)
        });
    ax_display::init_display(devices);
}

#[cfg(all(feature = "display", not(feature = "plat-dyn")))]
pub(crate) fn init_static_display() {
    let devices = ax_driver::display::take_display_devices()
        .unwrap_or_else(|err| panic!("failed to open static display devices: {err:?}"))
        .into_iter()
        .map(|dev| {
            let display = ax_display::rdif::RdifDisplayDevice::new(dev)
                .unwrap_or_else(|err| panic!("failed to adapt static display device: {err:?}"));
            ax_display::ErasedDisplayDevice::new(display)
        });
    ax_display::init_display(devices);
}

#[cfg(all(feature = "input", feature = "plat-dyn"))]
pub(crate) fn init_dyn_input() {
    if !rdrive::is_initialized() {
        ax_input::init_input(core::iter::empty::<ax_input::ErasedInputDevice>());
        return;
    }
    let devices = ax_driver::input::take_input_devices()
        .unwrap_or_else(|err| panic!("failed to open input devices: {err:?}"))
        .into_iter()
        .map(|dev| ax_input::ErasedInputDevice::new(ax_input::rdif::RdifInputDevice::new(dev)));
    ax_input::init_input(devices);
}

#[cfg(all(feature = "input", not(feature = "plat-dyn")))]
pub(crate) fn init_static_input() {
    let devices = ax_driver::input::take_input_devices()
        .unwrap_or_else(|err| panic!("failed to open static input devices: {err:?}"))
        .into_iter()
        .map(|dev| ax_input::ErasedInputDevice::new(ax_input::rdif::RdifInputDevice::new(dev)));
    ax_input::init_input(devices);
}

#[cfg(all(feature = "net", feature = "plat-dyn"))]
pub(crate) fn init_dyn_net() {
    #[cfg(feature = "irq")]
    ax_net::set_ethernet_irq_registrar(&crate::irq::NET_IRQ_REGISTRAR);
    register_unix_namespace();
    let config = parse_network_config();
    let (nics, wireless) = collect_dyn_net_devices();
    ax_net::init_network(nics, config);
    register_wireless_devices(wireless);
}

#[cfg(all(feature = "net", not(feature = "plat-dyn")))]
pub(crate) fn init_static_net() {
    #[cfg(feature = "irq")]
    ax_net::set_ethernet_irq_registrar(&crate::irq::NET_IRQ_REGISTRAR);
    register_unix_namespace();
    let config = parse_network_config();
    let (nics, wireless) = collect_static_net_devices();
    ax_net::init_network(nics, config);
    register_wireless_devices(wireless);
}

#[cfg(all(feature = "net", feature = "fs"))]
fn register_unix_namespace() {
    ax_net::unix::register_unix_namespace(crate::unix_ns::AxFsUnixNamespace);
}

#[cfg(all(feature = "net", not(feature = "fs")))]
fn register_unix_namespace() {
    // Path-based Unix sockets require filesystem namespace support.
}

#[cfg(feature = "net")]
fn parse_network_config() -> ax_net::NetworkConfig {
    ax_net::NetworkConfig::default()
}

/// A wireless device that registers *after* `init_network`: its already-wrapped
/// driver plus the link policy (static IP + optional DHCP-server lease) the
/// board reported for it.
#[cfg(feature = "net")]
type WirelessDevice = (
    alloc::boxed::Box<dyn ax_net::EthernetDriver>,
    ax_net::NetConfig,
);

/// Wraps one probed net device, splitting it into either a plain NIC (for the
/// `init_network` device list) or a wireless device (registered separately with
/// its link policy).
///
/// A wireless device is just a `PlatformNetDevice` whose underlying `Interface`
/// exposes a [`rd_net::WifiControl`] (via `Net::wifi_control`). We read its link
/// policy and wire its out-of-band RX callback here, then wrap the same
/// `rd_net::Net` data plane every NIC uses. Keeping the Wi-Fi specifics on the
/// device (not in the stack) is what lets the protocol stack stay link-agnostic.
#[cfg(feature = "net")]
fn adapt_net_device(
    net: rd_net::Net,
    name: &'static str,
    irq_num: Option<usize>,
    nics: &mut alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>>,
    wireless: &mut alloc::vec::Vec<WirelessDevice>,
) {
    // If this device has a wireless control plane, wire its out-of-band RX wake
    // and read the link policy the board attached to it.
    let policy = if let Some(ctrl) = net.wifi_control() {
        // SDIO Wi-Fi RX is out-of-band (not the ethernet IRQ framework); the
        // chip's RX-data callback wakes the stack's dedicated poll task.
        ctrl.set_rx_wake(ax_net::wake_net_task_irq);
        ctrl.link_policy()
    } else {
        None
    };

    // Capture a standalone control-plane handle *before* the `Net` is consumed
    // into the data-plane driver, so runtime mode switching can reach this
    // device's `WifiControl` by name (see `ax_net::reconfigure_wifi`).
    if let Some(handle) = net.wifi_control_handle() {
        ax_net::register_wifi_control(name, handle);
    }

    let driver = ax_net::RdNetDriver::new(name, net, irq_num)
        .unwrap_or_else(|err| panic!("failed to adapt net device {name}: {err:?}"));
    let driver = alloc::boxed::Box::new(driver) as alloc::boxed::Box<dyn ax_net::EthernetDriver>;

    match policy {
        Some(p) => wireless.push((
            driver,
            ax_net::NetConfig {
                name: name.into(),
                ip: p.ip,
                prefix_len: p.prefix_len,
                dhcp_server_client_ip: p.dhcp_server_client_ip,
                dedicated_poll: true,
            },
        )),
        None => nics.push(driver),
    }
}

/// Registers wireless devices that carry a link policy with the
/// already-initialized network stack (static IP + optional DHCP server +
/// dedicated out-of-band RX poll). Plain NICs are handled by `init_network`.
#[cfg(feature = "net")]
fn register_wireless_devices(wireless: alloc::vec::Vec<WirelessDevice>) {
    for (driver, config) in wireless {
        ax_net::register_device_with_config(driver, config);
    }
}

#[cfg(all(feature = "vsock", feature = "plat-dyn"))]
pub(crate) fn init_dyn_vsock() {
    if !rdrive::is_initialized() {
        ax_net::init_vsock(alloc::vec::Vec::new());
        return;
    }
    let devices = ax_driver::vsock::take_vsock_devices()
        .unwrap_or_else(|err| panic!("failed to open vsock devices: {err:?}"));
    ax_net::init_vsock(devices);
}

#[cfg(all(feature = "vsock", not(feature = "plat-dyn")))]
pub(crate) fn init_static_vsock() {
    let devices = ax_driver::vsock::take_vsock_devices()
        .unwrap_or_else(|err| panic!("failed to open static vsock devices: {err:?}"));
    ax_net::init_vsock(devices);
}

#[cfg(all(feature = "net", not(feature = "plat-dyn")))]
fn collect_static_net_devices() -> (
    alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>>,
    alloc::vec::Vec<WirelessDevice>,
) {
    let mut nics = alloc::vec::Vec::new();
    let mut wireless = alloc::vec::Vec::new();
    for dev in rdrive::get_list::<ax_driver::net::PlatformNetDevice>() {
        let (net, name, irq_num) = ax_driver::net::take_rd_net_device(dev)
            .unwrap_or_else(|err| panic!("failed to open static net device: {err:?}"));
        adapt_net_device(net, name, irq_num, &mut nics, &mut wireless);
    }
    (nics, wireless)
}

#[cfg(all(feature = "net", feature = "plat-dyn"))]
fn collect_dyn_net_devices() -> (
    alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>>,
    alloc::vec::Vec<WirelessDevice>,
) {
    let mut nics = alloc::vec::Vec::new();
    let mut wireless = alloc::vec::Vec::new();
    if !rdrive::is_initialized() {
        return (nics, wireless);
    }
    for dev in rdrive::get_list::<ax_driver::net::PlatformNetDevice>() {
        let (net, name, irq_num) = ax_driver::net::take_rd_net_device(dev)
            .unwrap_or_else(|err| panic!("failed to open net device: {err:?}"));
        adapt_net_device(net, name, irq_num, &mut nics, &mut wireless);
    }
    (nics, wireless)
}
