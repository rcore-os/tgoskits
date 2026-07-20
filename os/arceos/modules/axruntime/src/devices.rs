pub(crate) fn probe_all_devices() {
    info!("Probe platform devices...");
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip platform device probe");
        return;
    }
    rdrive::probe_all(false)
        .unwrap_or_else(|err| panic!("failed to probe platform devices: {err:?}"));
}

#[cfg(feature = "display")]
pub(crate) fn init_display() {
    if !rdrive::is_initialized() {
        ax_display::init_display(None);
        return;
    }
    let mut devices = ax_driver::display::take_display_devices()
        .unwrap_or_else(|err| panic!("failed to open display devices: {err:?}"))
        .into_iter();
    let display = devices.next().map(|taken| {
        crate::display::activate_display(taken)
            .unwrap_or_else(|error| panic!("failed to activate display owner: {error}"))
    });
    ax_display::init_display(display);
}

#[cfg(feature = "input")]
pub(crate) fn init_input() {
    if !rdrive::is_initialized() {
        ax_input::init_input(core::iter::empty::<ax_input::InputDeviceFacade>());
        return;
    }
    let devices = ax_driver::input::take_input_devices()
        .unwrap_or_else(|err| panic!("failed to open input devices: {err:?}"))
        .into_iter()
        .filter_map(|taken| match crate::input::activate_input(taken) {
            Ok(device) => Some(device),
            Err(error) => {
                warn!("failed to activate input owner: {error}");
                None
            }
        });
    ax_input::init_input(devices);
}

#[cfg(feature = "net")]
pub(crate) fn init_net() {
    register_unix_namespace();
    let mut config = parse_network_config();
    let nics = collect_net_devices(&mut config);
    ax_net::init_network(nics, config);
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

/// Activates one discovered NIC and merges its immutable owner-established
/// link policy into the ordinary network configuration.
///
/// Mutable wireless control never escapes the CPU-pinned owner. A device that
/// needs later mode changes receives typed mailbox commands through that same
/// owner rather than exposing a second out-of-band runtime boundary.
#[cfg(feature = "net")]
fn adapt_net_device(
    net: rd_net::Net,
    name: &'static str,
    irq: Option<ax_driver::BindingIrq>,
    nics: &mut alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>>,
    config: &mut ax_net::NetworkConfig,
) {
    let irq = resolve_net_irq(name, irq);
    let activated = crate::net::activate_net_device(net, name, irq)
        .unwrap_or_else(|err| panic!("failed to activate net device {name}: {err}"));
    if let Some(policy) = activated.link_policy {
        config.interfaces.push(ax_net::InterfaceConfig {
            name: alloc::string::String::from(name),
            match_by: ax_net::InterfaceMatcher::ByDriverName(alloc::string::String::from(name)),
            static_ip: Some(ax_net::StaticIpConfig {
                ip: core::net::Ipv4Addr::from(policy.ip),
                prefix_len: policy.prefix_len,
                gateway: core::net::Ipv4Addr::UNSPECIFIED,
            }),
            dhcp: false,
            metric: 100,
            dns_servers: alloc::vec::Vec::new(),
        });
    }
    nics.push(activated.driver);
}

#[cfg(all(feature = "net", feature = "irq"))]
fn resolve_net_irq(name: &str, irq: Option<ax_driver::BindingIrq>) -> Option<irq_framework::IrqId> {
    let irq = irq?;
    match crate::irq::resolve_binding_irq(irq) {
        Ok(id) => Some(id),
        Err(err) => {
            warn!("failed to resolve net IRQ for {name}: {err:?}");
            None
        }
    }
}

#[cfg(all(feature = "net", not(feature = "irq")))]
fn resolve_net_irq(
    _name: &str,
    _irq: Option<ax_driver::BindingIrq>,
) -> Option<irq_framework::IrqId> {
    None
}

#[cfg(feature = "vsock")]
pub(crate) fn init_vsock() {
    if !rdrive::is_initialized() {
        ax_net::init_vsock(alloc::vec::Vec::new());
        return;
    }
    let devices = ax_driver::vsock::take_vsock_devices()
        .unwrap_or_else(|err| panic!("failed to open vsock devices: {err:?}"));
    ax_net::init_vsock(devices);
}

#[cfg(feature = "net")]
fn collect_net_devices(
    config: &mut ax_net::NetworkConfig,
) -> alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>> {
    let mut nics = alloc::vec::Vec::new();
    if !rdrive::is_initialized() {
        return nics;
    }
    for dev in rdrive::get_list::<ax_driver::net::PlatformNetDevice>() {
        let (net, name, irq) = ax_driver::net::take_rd_net_device(dev)
            .unwrap_or_else(|err| panic!("failed to open net device: {err:?}"));
        adapt_net_device(net, name, irq, &mut nics, config);
    }
    nics
}
