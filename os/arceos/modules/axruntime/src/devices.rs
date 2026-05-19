#[cfg(feature = "plat-dyn")]
pub(crate) fn init_dyn_devices() {
    info!("Initialize dynamic platform devices...");
    #[cfg(target_os = "none")]
    axplat_dyn::drivers::probe_all_devices()
        .unwrap_or_else(|err| panic!("failed to probe dynamic platform devices: {err:?}"));
}

#[cfg(all(feature = "static-devices", not(feature = "plat-dyn")))]
pub(crate) fn init_static_devices() {
    info!("Initialize static platform devices...");
    crate::static_devices::init()
        .unwrap_or_else(|err| panic!("failed to initialize static platform devices: {err:?}"));
}

#[cfg(all(feature = "fs", feature = "plat-dyn"))]
pub(crate) fn take_dyn_fs_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs::FsBlockDevice>> {
    #[cfg(target_os = "none")]
    {
        axplat_dyn::drivers::take_block_devices()
            .into_iter()
            .map(|dev| {
                alloc::boxed::Box::new(DynFsBlockDevice(dev))
                    as alloc::boxed::Box<dyn ax_fs::FsBlockDevice>
            })
            .collect()
    }

    #[cfg(not(target_os = "none"))]
    alloc::vec::Vec::new()
}

#[cfg(all(feature = "fs", not(feature = "plat-dyn")))]
pub(crate) fn take_static_fs_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs::FsBlockDevice>> {
    crate::static_devices::take_fs_block_devices()
}

#[cfg(all(feature = "fs-ng", feature = "plat-dyn"))]
pub(crate) fn take_dyn_fs_ng_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs_ng::FsBlockDevice>> {
    #[cfg(target_os = "none")]
    {
        axplat_dyn::drivers::take_block_devices()
            .into_iter()
            .map(|dev| {
                alloc::boxed::Box::new(DynFsBlockDevice(dev))
                    as alloc::boxed::Box<dyn ax_fs_ng::FsBlockDevice>
            })
            .collect()
    }

    #[cfg(not(target_os = "none"))]
    alloc::vec::Vec::new()
}

#[cfg(all(feature = "fs-ng", not(feature = "plat-dyn")))]
pub(crate) fn take_static_fs_ng_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs_ng::FsBlockDevice>> {
    crate::static_devices::take_fs_ng_block_devices()
}

#[cfg(all(feature = "display", feature = "plat-dyn"))]
pub(crate) fn init_dyn_display() {
    #[cfg(target_os = "none")]
    {
        let devices = axplat_dyn::drivers::display::take_display_devices()
            .unwrap_or_else(|err| panic!("failed to open display devices: {err:?}"))
            .into_iter()
            .map(|dev| {
                let display = ax_display::rdif::RdifDisplayDevice::new(dev)
                    .unwrap_or_else(|err| panic!("failed to adapt display device: {err:?}"));
                ax_display::ErasedDisplayDevice::new(display)
            });
        ax_display::init_display(devices);
    }

    #[cfg(not(target_os = "none"))]
    ax_display::init_display(core::iter::empty::<ax_display::ErasedDisplayDevice>());
}

#[cfg(all(feature = "input", feature = "plat-dyn"))]
pub(crate) fn init_dyn_input() {
    #[cfg(target_os = "none")]
    {
        let devices = axplat_dyn::drivers::input::take_input_devices()
            .unwrap_or_else(|err| panic!("failed to open input devices: {err:?}"))
            .into_iter()
            .map(|dev| ax_input::ErasedInputDevice::new(ax_input::rdif::RdifInputDevice::new(dev)));
        ax_input::init_input(devices);
    }

    #[cfg(not(target_os = "none"))]
    ax_input::init_input(core::iter::empty::<ax_input::ErasedInputDevice>());
}

#[cfg(all(feature = "net", feature = "plat-dyn"))]
pub(crate) fn init_dyn_net() {
    ax_net::init_network(take_dyn_net_drivers());
}

#[cfg(all(feature = "net-ng", feature = "plat-dyn"))]
pub(crate) fn init_dyn_net_ng() {
    ax_net_ng::init_network(take_dyn_net_ng_drivers());
}

#[cfg(all(feature = "net-ng", not(feature = "plat-dyn")))]
pub(crate) fn take_static_net_ng_drivers()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_net_ng::EthernetDriver>> {
    let mut devices = alloc::vec::Vec::new();
    for dev in rdrive::get_list::<rd_net::Net>() {
        let mut guard = dev
            .lock()
            .unwrap_or_else(|err| panic!("failed to lock static net device: {err:?}"));
        let net = core::mem::replace(
            &mut *guard,
            rd_net::Net::new(StaticEmptyNet, crate::static_devices::identity_dma()),
        );
        let driver = ax_net_ng::RdNetDriver::new("virtio-net", net, None)
            .unwrap_or_else(|err| panic!("failed to adapt static net device: {err:?}"));
        devices.push(
            alloc::boxed::Box::new(driver) as alloc::boxed::Box<dyn ax_net_ng::EthernetDriver>
        );
    }
    devices
}

#[cfg(all(feature = "vsock", feature = "plat-dyn"))]
pub(crate) fn init_dyn_vsock() {
    #[cfg(target_os = "none")]
    {
        let devices = axplat_dyn::drivers::vsock::take_vsock_devices()
            .unwrap_or_else(|err| panic!("failed to open vsock devices: {err:?}"));
        ax_net_ng::init_vsock(devices);
    }

    #[cfg(not(target_os = "none"))]
    ax_net_ng::init_vsock(alloc::vec::Vec::new());
}

#[cfg(all(feature = "vsock", not(feature = "plat-dyn")))]
pub(crate) fn init_static_vsock() {
    ax_net_ng::init_vsock(alloc::vec::Vec::new());
}

#[cfg(all(feature = "net", feature = "plat-dyn"))]
fn take_dyn_net_drivers() -> alloc::vec::Vec<alloc::boxed::Box<dyn ax_net::EthernetDriver>> {
    #[cfg(target_os = "none")]
    {
        let mut devices = alloc::vec::Vec::new();
        for dev in rdrive::get_list::<axplat_dyn::drivers::net::PlatformNetDevice>() {
            let (net, name, irq_num) = axplat_dyn::drivers::net::take_rd_net_device(dev)
                .unwrap_or_else(|err| panic!("failed to open net device: {err:?}"));
            let driver = ax_net_ng::RdNetDriver::new(name, net, irq_num)
                .unwrap_or_else(|err| panic!("failed to adapt net device: {err:?}"));
            devices.push(
                alloc::boxed::Box::new(driver) as alloc::boxed::Box<dyn ax_net::EthernetDriver>
            );
        }
        devices
    }

    #[cfg(not(target_os = "none"))]
    alloc::vec::Vec::new()
}

#[cfg(all(feature = "net-ng", feature = "plat-dyn"))]
fn take_dyn_net_ng_drivers() -> alloc::vec::Vec<alloc::boxed::Box<dyn ax_net_ng::EthernetDriver>> {
    #[cfg(target_os = "none")]
    {
        let mut devices = alloc::vec::Vec::new();
        for dev in rdrive::get_list::<axplat_dyn::drivers::net::PlatformNetDevice>() {
            let (net, name, irq_num) = axplat_dyn::drivers::net::take_rd_net_device(dev)
                .unwrap_or_else(|err| panic!("failed to open net device: {err:?}"));
            let driver = ax_net_ng::RdNetDriver::new(name, net, irq_num)
                .unwrap_or_else(|err| panic!("failed to adapt net device: {err:?}"));
            devices
                .push(alloc::boxed::Box::new(driver)
                    as alloc::boxed::Box<dyn ax_net_ng::EthernetDriver>);
        }
        devices
    }

    #[cfg(not(target_os = "none"))]
    alloc::vec::Vec::new()
}

#[cfg(all(
    any(feature = "fs", feature = "fs-ng"),
    feature = "plat-dyn",
    target_os = "none"
))]
struct DynFsBlockDevice(axplat_dyn::drivers::blk::Block);

#[cfg(all(feature = "net-ng", not(feature = "plat-dyn")))]
struct StaticEmptyNet;

#[cfg(all(feature = "net-ng", not(feature = "plat-dyn")))]
impl rdrive::DriverGeneric for StaticEmptyNet {
    fn name(&self) -> &str {
        "empty-net"
    }
}

#[cfg(all(feature = "net-ng", not(feature = "plat-dyn")))]
impl rd_net::Interface for StaticEmptyNet {
    fn mac_address(&self) -> [u8; 6] {
        [0; 6]
    }

    fn create_tx_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_net::ITxQueue>> {
        None
    }

    fn create_rx_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_net::IRxQueue>> {
        None
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> rd_net::Event {
        rd_net::Event::none()
    }
}

#[cfg(all(feature = "fs-ng", feature = "plat-dyn", target_os = "none"))]
impl ax_fs_ng::FsBlockDevice for DynFsBlockDevice {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn num_blocks(&self) -> u64 {
        self.0.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.0.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        self.0.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        self.0.write_block(block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        self.0.flush()
    }
}

#[cfg(all(feature = "fs", feature = "plat-dyn", target_os = "none"))]
impl ax_fs::FsBlockDevice for DynFsBlockDevice {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn num_blocks(&self) -> u64 {
        self.0.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.0.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        self.0.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        self.0.write_block(block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        self.0.flush()
    }
}
