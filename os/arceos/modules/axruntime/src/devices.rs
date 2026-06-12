pub(crate) fn probe_all_devices() {
    info!("Probe platform devices...");
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip platform device probe");
        return;
    }
    rdrive::probe_all(false)
        .unwrap_or_else(|err| panic!("failed to probe platform devices: {err:?}"));
}

#[cfg(all(feature = "fs", not(feature = "fs-ng"), feature = "plat-dyn"))]
pub(crate) fn take_dyn_fs_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs::FsBlockDevice>> {
    if !rdrive::is_initialized() {
        return alloc::vec::Vec::new();
    }
    let devices = ax_driver::block::take_block_devices();
    devices
        .into_iter()
        .map(|dev| {
            alloc::boxed::Box::new(FsBlockDevice::new(dev))
                as alloc::boxed::Box<dyn ax_fs::FsBlockDevice>
        })
        .collect()
}

#[cfg(all(feature = "fs", not(feature = "fs-ng"), not(feature = "plat-dyn")))]
pub(crate) fn take_static_fs_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs::FsBlockDevice>> {
    let devices = ax_driver::block::take_block_devices();
    devices
        .into_iter()
        .map(|dev| {
            alloc::boxed::Box::new(FsBlockDevice::new(dev))
                as alloc::boxed::Box<dyn ax_fs::FsBlockDevice>
        })
        .collect()
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
    ax_net::init_network(take_dyn_net_drivers(), config);
}

#[cfg(all(feature = "net", not(feature = "plat-dyn")))]
pub(crate) fn init_static_net() {
    #[cfg(feature = "irq")]
    ax_net::set_ethernet_irq_registrar(&crate::irq::NET_IRQ_REGISTRAR);
    register_unix_namespace();
    let config = parse_network_config();
    ax_net::init_network(take_static_net_drivers(), config);
}

#[cfg(all(feature = "net", feature = "fs-ng"))]
fn register_unix_namespace() {
    ax_net::unix::register_unix_namespace(crate::unix_ns::AxFsUnixNamespace);
}

#[cfg(all(feature = "net", not(feature = "fs-ng")))]
fn register_unix_namespace() {
    // Path-based Unix sockets require fs-ng namespace support
}

#[cfg(feature = "net")]
fn parse_network_config() -> ax_net::NetworkConfig {
    ax_net::NetworkConfig::default()
}

#[cfg(all(feature = "net", not(feature = "plat-dyn")))]
pub(crate) fn take_static_net_drivers()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>> {
    let mut devices = alloc::vec::Vec::new();
    for dev in rdrive::get_list::<ax_driver::net::PlatformNetDevice>() {
        let (net, name, irq) = ax_driver::net::take_rd_net_device(dev)
            .unwrap_or_else(|err| panic!("failed to open static net device: {err:?}"));
        let driver = ax_net::RdNetDriver::new(name, net, irq)
            .unwrap_or_else(|err| panic!("failed to adapt static net device: {err:?}"));
        devices
            .push(alloc::boxed::Box::new(driver) as alloc::boxed::Box<dyn ax_net::EthernetDriver>);
    }
    devices
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

#[cfg(all(feature = "net", feature = "plat-dyn"))]
fn take_dyn_net_drivers() -> alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>> {
    if !rdrive::is_initialized() {
        return alloc::vec::Vec::new();
    }
    let mut devices = alloc::vec::Vec::new();
    for dev in rdrive::get_list::<ax_driver::net::PlatformNetDevice>() {
        let (net, name, irq) = ax_driver::net::take_rd_net_device(dev)
            .unwrap_or_else(|err| panic!("failed to open net device: {err:?}"));
        let driver = ax_net::RdNetDriver::new(name, net, irq)
            .unwrap_or_else(|err| panic!("failed to adapt net device: {err:?}"));
        devices
            .push(alloc::boxed::Box::new(driver) as alloc::boxed::Box<dyn ax_net::EthernetDriver>);
    }
    devices
}

#[cfg(all(feature = "fs", not(feature = "fs-ng")))]
struct FsBlockDevice {
    _irq: Option<crate::block::BlockIrqRegistration>,
    block: ax_driver::block::Block,
}

#[cfg(all(feature = "fs", not(feature = "fs-ng")))]
impl FsBlockDevice {
    fn new(mut block: ax_driver::block::Block) -> Self {
        let irq = crate::block::register_irq_handler(&mut block);
        Self { _irq: irq, block }
    }
}

#[cfg(all(feature = "fs", not(feature = "fs-ng")))]
impl ax_fs::FsBlockDevice for FsBlockDevice {
    fn name(&self) -> &str {
        self.block.name()
    }

    fn num_blocks(&self) -> u64 {
        self.block.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.block.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        self.block.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        self.block.write_block(block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        self.block.flush()
    }
}
