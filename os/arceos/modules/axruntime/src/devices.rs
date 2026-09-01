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
        ax_display::init_display(core::iter::empty::<ax_display::ErasedDisplayDevice>());
        return;
    }
    let devices = ax_driver::display::take_display_devices()
        .unwrap_or_else(|err| panic!("failed to open display devices: {err:?}"))
        .into_iter()
        .map(adapt_display_device);
    ax_display::init_display(devices);
}

#[cfg(feature = "display")]
fn adapt_display_device(
    taken: ax_driver::display::TakenDisplayDevice,
) -> ax_display::ErasedDisplayDevice {
    let name = alloc::string::String::from(taken.device.name());
    let irq = resolve_display_irq(&name, taken.irq)
        .unwrap_or_else(|err| panic!("failed to resolve display IRQ for {name}: {err:?}"));
    let display = ax_display::rdif::RdifDisplayDevice::new_with_irq(taken.device, irq)
        .unwrap_or_else(|err| panic!("failed to adapt display device: {err:?}"));
    ax_display::ErasedDisplayDevice::new(display)
}

#[cfg(feature = "display")]
fn resolve_display_irq(
    _name: &str,
    irq: Option<ax_driver::BindingIrq>,
) -> Result<Option<irq_framework::IrqId>, irq_framework::IrqError> {
    irq.map(crate::irq::resolve_binding_irq).transpose()
}

#[cfg(feature = "input")]
pub(crate) fn init_input() {
    if !rdrive::is_initialized() {
        ax_input::init_input(core::iter::empty::<ax_input::ErasedInputDevice>());
        return;
    }
    let devices = ax_driver::input::take_input_devices()
        .unwrap_or_else(|err| panic!("failed to open input devices: {err:?}"))
        .into_iter()
        .map(adapt_input_device);
    ax_input::init_input(devices);
}

#[cfg(feature = "input")]
fn adapt_input_device(taken: ax_driver::input::TakenInputDevice) -> ax_input::ErasedInputDevice {
    let name = alloc::string::String::from(taken.device.name());
    let irq = resolve_input_irq(&name, taken.irq)
        .unwrap_or_else(|err| panic!("failed to resolve input IRQ for {name}: {err:?}"));
    ax_input::ErasedInputDevice::new(ax_input::rdif::RdifInputDevice::new_with_irq(
        taken.device,
        irq,
    ))
}

#[cfg(feature = "input")]
fn resolve_input_irq(
    _name: &str,
    irq: Option<ax_driver::BindingIrq>,
) -> Result<Option<irq_framework::IrqId>, irq_framework::IrqError> {
    irq.map(crate::irq::resolve_binding_irq).transpose()
}

#[cfg(feature = "net")]
pub(crate) fn init_net() {
    register_unix_namespace();
    let config = parse_network_config();

    if !rdrive::is_initialized() {
        ax_net::init_network(None, alloc::vec::Vec::new(), config);
        return;
    }

    let devices = collect_net_devices();
    if devices.is_empty() {
        ax_net::init_network(None, alloc::vec::Vec::new(), config);
        return;
    }
    let (runtime, ports) = ax_net::NetworkRuntimeBuilder::new(
        devices,
        &crate::irq::NET_IRQ_REGISTRAR,
        ax_hal::cpu_num(),
    )
    .build()
    .unwrap_or_else(|error| panic!("failed to initialize network queue runtime: {error}"));
    ax_net::init_network(Some(runtime), ports, config);
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

#[cfg(feature = "net")]
fn collect_net_devices() -> alloc::vec::Vec<ax_net::NetworkDeviceInput> {
    let mut devices = alloc::vec::Vec::new();
    for device in rdrive::get_list::<ax_driver::net::PlatformNetDevice>() {
        let taken = ax_driver::net::take_net_device(device)
            .unwrap_or_else(|error| panic!("failed to take network device: {error}"));
        let name = alloc::string::String::from(taken.name);
        let prepared = rd_net::prepare_device(taken.prepared_device, taken.dma)
            .unwrap_or_else(|error| panic!("failed to prepare network device {name}: {error}"));
        let mut irq_sources = alloc::vec::Vec::with_capacity(taken.irq_sources.len());
        for source in taken.irq_sources {
            let source_id = u16::try_from(source.source_id).unwrap_or_else(|_| {
                panic!(
                    "network device {name} IRQ source {} exceeds the source-id width",
                    source.source_id
                )
            });
            let irq = crate::irq::resolve_binding_irq(source.irq).unwrap_or_else(|error| {
                panic!("failed to resolve network device {name} IRQ source {source_id}: {error:?}")
            });
            irq_sources.push(ax_net::ResolvedNetIrqSource {
                source_id: rd_net::NetIrqSourceId::new(source_id),
                irq,
            });
        }
        devices.push(ax_net::NetworkDeviceInput {
            name,
            device: prepared,
            irq_sources,
            tx_queue_discipline: ax_net::TxQueueDiscipline::Fifo {
                max_frames: core::num::NonZeroUsize::new(64).unwrap(),
            },
        });
    }
    devices
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
