#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
use rdrive::DriverGeneric;

#[cfg(feature = "plat-dyn")]
pub(crate) fn init_dyn_devices() {
    info!("Initialize dynamic platform devices...");
    #[cfg(target_os = "none")]
    axplat_dyn::drivers::probe_all_devices()
        .unwrap_or_else(|err| panic!("failed to probe dynamic platform devices: {err:?}"));
}

#[cfg(not(feature = "plat-dyn"))]
pub(crate) fn init_static_devices() {
    info!("Initialize static platform devices...");
    if rdrive::is_initialized() {
        rdrive::probe_all(false)
            .unwrap_or_else(|err| panic!("failed to probe static platform devices: {err:?}"));
    }
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
    take_static_blocks()
        .into_iter()
        .map(|dev| alloc::boxed::Box::new(dev) as alloc::boxed::Box<dyn ax_fs::FsBlockDevice>)
        .collect()
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
    take_static_blocks()
        .into_iter()
        .map(|dev| alloc::boxed::Box::new(dev) as alloc::boxed::Box<dyn ax_fs_ng::FsBlockDevice>)
        .collect()
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

#[cfg(all(feature = "display", not(feature = "plat-dyn")))]
pub(crate) fn init_static_display() {
    let devices = ax_drivers::bindings::display::take_display_devices()
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

#[cfg(all(feature = "input", not(feature = "plat-dyn")))]
pub(crate) fn init_static_input() {
    let devices = ax_drivers::bindings::input::take_input_devices()
        .unwrap_or_else(|err| panic!("failed to open static input devices: {err:?}"))
        .into_iter()
        .map(|dev| ax_input::ErasedInputDevice::new(ax_input::rdif::RdifInputDevice::new(dev)));
    ax_input::init_input(devices);
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
    for dev in rdrive::get_list::<ax_drivers::bindings::net::PlatformNetDevice>() {
        let (net, name, irq_num) = ax_drivers::bindings::net::take_rd_net_device(dev)
            .unwrap_or_else(|err| panic!("failed to open static net device: {err:?}"));
        let driver = ax_net_ng::RdNetDriver::new(name, net, irq_num)
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
    let devices = ax_drivers::bindings::vsock::take_vsock_devices()
        .unwrap_or_else(|err| panic!("failed to open static vsock devices: {err:?}"));
    ax_net_ng::init_vsock(devices);
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

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
struct StaticBlockDevice {
    name: alloc::string::String,
    queue: spin::Mutex<rd_block::CmdQueue>,
}

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
impl StaticBlockDevice {
    fn new(mut block: rd_block::Block) -> Result<Self, ax_errno::AxError> {
        let name = block.name().into();
        let queue = block.create_queue().ok_or(ax_errno::AxError::BadState)?;
        Ok(Self {
            name,
            queue: spin::Mutex::new(queue),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn num_blocks(&self) -> u64 {
        self.queue.lock().num_blocks() as _
    }

    fn block_size(&self) -> usize {
        self.queue.lock().block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let mut queue = self.queue.lock();
        for (offset, chunk) in buf.chunks_mut(block_size).enumerate() {
            let mut blocks = queue.read_blocks_blocking(block_id as usize + offset, 1);
            let block = blocks
                .pop()
                .ok_or(ax_errno::AxError::Io)?
                .map_err(map_blk_err_to_ax_err)?;
            if block.len() != chunk.len() {
                return Err(ax_errno::AxError::Io);
            }
            chunk.copy_from_slice(&block);
        }
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let mut queue = self.queue.lock();
        for (offset, chunk) in buf.chunks(block_size).enumerate() {
            for block in queue.write_blocks_blocking(block_id as usize + offset, chunk) {
                block.map_err(map_blk_err_to_ax_err)?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        Ok(())
    }
}

#[cfg(feature = "fs")]
impl ax_fs::FsBlockDevice for StaticBlockDevice {
    fn name(&self) -> &str {
        self.name()
    }

    fn num_blocks(&self) -> u64 {
        self.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        StaticBlockDevice::read_block(self, block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        StaticBlockDevice::write_block(self, block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        StaticBlockDevice::flush(self)
    }
}

#[cfg(feature = "fs-ng")]
impl ax_fs_ng::FsBlockDevice for StaticBlockDevice {
    fn name(&self) -> &str {
        self.name()
    }

    fn num_blocks(&self) -> u64 {
        self.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_errno::AxResult {
        StaticBlockDevice::read_block(self, block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_errno::AxResult {
        StaticBlockDevice::write_block(self, block_id, buf)
    }

    fn flush(&mut self) -> ax_errno::AxResult {
        StaticBlockDevice::flush(self)
    }
}

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
fn take_static_blocks() -> alloc::vec::Vec<StaticBlockDevice> {
    rdrive::get_list::<rd_block::Block>()
        .into_iter()
        .map(|dev| {
            let mut guard = dev
                .lock()
                .unwrap_or_else(|err| panic!("failed to lock static block device: {err:?}"));
            let block = core::mem::replace(
                &mut *guard,
                rd_block::Block::new(EmptyBlock, axklib::dma::op()),
            );
            StaticBlockDevice::new(block)
                .unwrap_or_else(|err| panic!("failed to adapt static block device: {err:?}"))
        })
        .collect()
}

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
struct EmptyBlock;

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
impl rdrive::DriverGeneric for EmptyBlock {
    fn name(&self) -> &str {
        "empty-block"
    }
}

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
impl rd_block::Interface for EmptyBlock {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        None
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> rd_block::Event {
        rd_block::Event::none()
    }
}

#[cfg(all(any(feature = "fs", feature = "fs-ng"), not(feature = "plat-dyn")))]
fn map_blk_err_to_ax_err(err: rd_block::BlkError) -> ax_errno::AxError {
    match err {
        rd_block::BlkError::NotSupported => ax_errno::AxError::Unsupported,
        rd_block::BlkError::Retry => ax_errno::AxError::WouldBlock,
        rd_block::BlkError::NoMemory => ax_errno::AxError::NoMemory,
        rd_block::BlkError::InvalidBlockIndex(_) => ax_errno::AxError::InvalidInput,
        rd_block::BlkError::Other(_) => ax_errno::AxError::Io,
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
