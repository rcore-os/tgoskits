#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
use ax_fs_ng::{
    block_runtime::{
        BlockDeviceHandle, BlockDmaBuffer, BlockDmaDirection, BlockDmaProvider, BlockDrainWake,
        BlockIrqBridge, BlockRuntime, BlockRuntimeConfig, BlockWaitToken, BlockWaiter,
    },
    os::{AddressTranslator, BlockTimeProvider},
};
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
use rdif_block::{BlkError, IQueue};
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
use spin::Once;

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static BLOCK_DRAIN_WQ: ax_task::WaitQueue = ax_task::WaitQueue::new();
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static BLOCK_DRAIN_DEVICE_BITS: AtomicU64 = AtomicU64::new(0);
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static BLOCK_DRAIN_FULL_SCAN: AtomicBool = AtomicBool::new(false);
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static BLOCK_DRAIN_SPAWNED: Once<()> = Once::new();
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static BLOCK_RUNTIME: Once<Arc<BlockRuntime>> = Once::new();
#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
static BLOCK_IRQ_STATES: Once<Vec<&'static BlockIrqState>> = Once::new();

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeTimeProvider;

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl BlockTimeProvider for RuntimeTimeProvider {
    fn wall_time(&self) -> core::time::Duration {
        ax_hal::time::wall_time()
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeAddressTranslator;

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl AddressTranslator for RuntimeAddressTranslator {
    fn virt_to_phys(&self, vaddr: usize) -> usize {
        ax_hal::mem::virt_to_phys(ax_hal::mem::VirtAddr::from(vaddr)).as_usize()
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static TIME_PROVIDER: RuntimeTimeProvider = RuntimeTimeProvider;
#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
static ADDRESS_TRANSLATOR: RuntimeAddressTranslator = RuntimeAddressTranslator;

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeWaitToken {
    ready: AtomicBool,
    wq: ax_task::WaitQueue,
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl RuntimeWaitToken {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            wq: ax_task::WaitQueue::new(),
        }
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl BlockWaitToken for RuntimeWaitToken {
    fn wait(&self) {
        self.wq.wait_until(|| self.ready.load(Ordering::Acquire));
    }

    fn wake(&self) {
        self.ready.store(true, Ordering::Release);
        self.wq.notify_one(true);
    }

    fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeWaiter;

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl BlockWaiter for RuntimeWaiter {
    fn new_token(&self) -> Arc<dyn BlockWaitToken> {
        Arc::new(RuntimeWaitToken::new())
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeDrainWake {
    device_index: usize,
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl BlockDrainWake for RuntimeDrainWake {
    fn wake_drain(&self) {
        mark_block_drain_device(self.device_index);
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
#[derive(Clone, Copy)]
struct DrainSelection {
    full_scan: bool,
    device_bits: u64,
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn mark_block_drain_device(device_index: usize) {
    if device_index < u64::BITS as usize {
        BLOCK_DRAIN_DEVICE_BITS.fetch_or(1 << device_index, Ordering::AcqRel);
    } else {
        BLOCK_DRAIN_FULL_SCAN.store(true, Ordering::Release);
    }
    BLOCK_DRAIN_WQ.notify_one(false);
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn block_drain_has_pending() -> bool {
    BLOCK_DRAIN_FULL_SCAN.load(Ordering::Acquire)
        || BLOCK_DRAIN_DEVICE_BITS.load(Ordering::Acquire) != 0
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn take_block_drain_selection() -> DrainSelection {
    DrainSelection {
        full_scan: BLOCK_DRAIN_FULL_SCAN.swap(false, Ordering::AcqRel),
        device_bits: BLOCK_DRAIN_DEVICE_BITS.swap(0, Ordering::AcqRel),
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn drain_selection_contains(selection: DrainSelection, device_index: usize) -> bool {
    selection.full_scan
        || (device_index < u64::BITS as usize && selection.device_bits & (1 << device_index) != 0)
}

#[cfg(all(
    test,
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
fn selected_drain_device_indices(
    device_count: usize,
    full_scan: bool,
    device_bits: u64,
) -> Vec<usize> {
    let selection = DrainSelection {
        full_scan,
        device_bits,
    };
    (0..device_count)
        .filter(|&device_index| drain_selection_contains(selection, device_index))
        .collect()
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeDmaProvider;

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl BlockDmaProvider for RuntimeDmaProvider {
    fn alloc(
        &self,
        dma_mask: u64,
        len: usize,
        align: usize,
        direction: BlockDmaDirection,
    ) -> Result<Box<dyn BlockDmaBuffer>, BlkError> {
        let dma = DeviceDma::new(dma_mask, axklib::dma::op());
        let dma_direction = match direction {
            BlockDmaDirection::Read => DmaDirection::FromDevice,
            BlockDmaDirection::Write => DmaDirection::ToDevice,
        };
        let len = len.max(1);
        let buffer = dma
            .contiguous_array_zero_with_align(len, align.max(1), dma_direction)
            .map_err(BlkError::from)?;
        Ok(Box::new(RuntimeDmaBuffer { buffer }))
    }
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
struct RuntimeDmaBuffer {
    buffer: ContiguousArray<u8>,
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
impl BlockDmaBuffer for RuntimeDmaBuffer {
    fn len(&self) -> usize {
        self.buffer.len()
    }

    fn bus_addr(&self) -> u64 {
        self.buffer.dma_addr().as_u64()
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.buffer.as_ptr().as_ptr()
    }

    fn prepare_for_submit(&mut self, direction: BlockDmaDirection, src: Option<&[u8]>) {
        match direction {
            BlockDmaDirection::Read => self.buffer.prepare_for_device_all(),
            BlockDmaDirection::Write => {
                if let Some(src) = src {
                    self.buffer.copy_to_device_from_slice(src);
                }
            }
        }
    }

    fn complete_after_submit(&mut self, direction: BlockDmaDirection, dst: Option<&mut [u8]>) {
        if direction == BlockDmaDirection::Read
            && let Some(dst) = dst
        {
            self.buffer.copy_from_device_to_slice(dst);
        }
    }
}

#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
struct BlockIrqState {
    handler: ax_driver::block::BlockIrqHandler,
    device: Arc<BlockDeviceHandle>,
    device_index: usize,
    irq_handle: Once<axklib::irq::IrqHandle>,
}

#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
impl BlockIrqState {
    fn on_drain_complete(&self) -> bool {
        let event = self.handler.on_drain_complete();
        self.device.record_driver_event_for_pending(event)
    }
}

#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
unsafe fn handle_block_irq(
    _ctx: axklib::irq::IrqContext,
    data: NonNull<()>,
) -> axklib::irq::IrqReturn {
    let state = unsafe { data.cast::<BlockIrqState>().as_ref() };
    let event = state.handler.handle();
    state.device.record_driver_event(event);
    mark_block_drain_device(state.device_index);
    axklib::irq::IrqReturn::Handled
}

#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
impl Drop for BlockIrqState {
    fn drop(&mut self) {
        if let Some(handle) = self.irq_handle.get().copied()
            && let Err(err) = axklib::irq::free(handle)
        {
            warn!("failed to free block irq handler: {err:?}");
        }
    }
}

#[cfg(all(feature = "fs", feature = "plat-dyn"))]
pub(crate) fn init_dyn_fs(bootargs: Option<&str>) {
    #[cfg(target_os = "none")]
    init_fs_from_raw_blocks(take_raw_block_devices(), bootargs);

    #[cfg(not(target_os = "none"))]
    let _ = bootargs;
}

#[cfg(all(feature = "fs", not(feature = "plat-dyn")))]
pub(crate) fn init_static_fs() {
    init_fs_from_raw_blocks(take_raw_block_devices(), None);
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn take_raw_block_devices() -> Vec<ax_driver::block::RawBlockDevice> {
    ax_driver::block::take_raw_block_devices()
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn init_fs_from_raw_blocks(blocks: Vec<ax_driver::block::RawBlockDevice>, bootargs: Option<&str>) {
    ax_fs_ng::os::set_time_provider(&TIME_PROVIDER);
    ax_fs_ng::os::set_address_translator(&ADDRESS_TRANSLATOR);

    let runtime = Arc::new(build_block_runtime(blocks));
    let fs_devices: Vec<_> = runtime.devices().to_vec();
    BLOCK_RUNTIME.call_once(|| runtime.clone());
    spawn_block_drain_task(runtime.clone());
    ax_fs_ng::root::init_root(
        fs_devices.into_iter().map(|dev| Box::new(dev) as Box<_>),
        bootargs,
    );
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn build_block_runtime(blocks: Vec<ax_driver::block::RawBlockDevice>) -> BlockRuntime {
    let mut runtime = BlockRuntime::new();
    #[cfg(feature = "irq")]
    let mut irq_states = Vec::new();
    for block in blocks {
        let device_index = runtime.devices().len();
        match build_block_device(block, device_index) {
            Ok(registered) => {
                #[cfg(feature = "irq")]
                {
                    let (device, states) = registered;
                    irq_states.extend(states);
                    runtime.push_device(device);
                }
                #[cfg(not(feature = "irq"))]
                runtime.push_device(registered);
            }
            Err(err) => warn!("failed to register submit/poll filesystem block device: {err:?}"),
        }
    }
    #[cfg(feature = "irq")]
    BLOCK_IRQ_STATES.call_once(|| irq_states);
    runtime
}

#[cfg(all(
    not(feature = "irq"),
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
type RegisteredBlockDevice = Arc<BlockDeviceHandle>;

#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
type RegisteredBlockDevice = (Arc<BlockDeviceHandle>, Vec<&'static BlockIrqState>);

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn build_block_device(
    mut block: ax_driver::block::RawBlockDevice,
    device_index: usize,
) -> Result<RegisteredBlockDevice, ax_errno::AxError> {
    let name = String::from(block.name());

    let mut queues: Vec<Box<dyn IQueue>> = Vec::new();
    while let Some(queue) = block.interface_mut().create_queue() {
        queues.push(queue);
    }
    if queues.is_empty() {
        return Err(ax_errno::AxError::BadState);
    }

    let bridge = Arc::new(BlockIrqBridge::new());
    let device = BlockDeviceHandle::new(
        name.clone(),
        queues,
        bridge.clone(),
        BlockRuntimeConfig::new(
            Arc::new(RuntimeDmaProvider),
            Arc::new(RuntimeWaiter),
            Arc::new(RuntimeDrainWake { device_index }),
        ),
    )
    .map_err(ax_fs_ng::block_runtime::map_blk_err_to_ax_err)?;

    let irq_states = register_irq_handlers(&mut block, device.clone(), device_index)?;
    block.enable_irq();
    if !block.interface().is_irq_enabled() {
        return Err(ax_errno::AxError::Unsupported);
    }

    info!("registered submit/poll filesystem block device {name}");
    #[cfg(feature = "irq")]
    return Ok((device, irq_states));
    #[cfg(not(feature = "irq"))]
    Ok(device)
}

#[cfg(all(
    feature = "irq",
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
fn register_irq_handlers(
    block: &mut ax_driver::block::RawBlockDevice,
    device: Arc<BlockDeviceHandle>,
    device_index: usize,
) -> ax_errno::AxResult<Vec<&'static BlockIrqState>> {
    let irq_sources = block.interface().irq_sources();
    if irq_sources.is_empty() {
        return Err(ax_errno::AxError::Unsupported);
    }

    let mut states = Vec::new();
    for source in irq_sources {
        let Some((irq_num, handler)) = block.take_irq_handler(source.id) else {
            return Err(ax_errno::AxError::Unsupported);
        };
        let mut state = Box::new(BlockIrqState {
            handler,
            device: device.clone(),
            device_index,
            irq_handle: Once::new(),
        });
        let data = NonNull::from(state.as_mut()).cast();
        let handle = axklib::irq::request_shared(irq_num, handle_block_irq, data)?;
        axklib::irq::enable(handle)?;
        state.irq_handle.call_once(|| handle);
        states.push(Box::leak(state) as &'static BlockIrqState);
    }
    Ok(states)
}

#[cfg(all(
    not(feature = "irq"),
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
fn register_irq_handlers(
    _block: &mut ax_driver::block::RawBlockDevice,
    _device: Arc<BlockDeviceHandle>,
    _device_index: usize,
) -> ax_errno::AxResult<()> {
    Err(ax_errno::AxError::Unsupported)
}

#[cfg(all(feature = "fs", any(not(feature = "plat-dyn"), target_os = "none")))]
fn spawn_block_drain_task(runtime: Arc<BlockRuntime>) {
    BLOCK_DRAIN_SPAWNED.call_once(|| {
        ax_task::spawn_raw(
            move || loop {
                BLOCK_DRAIN_WQ.wait_until(block_drain_has_pending);
                let selection = take_block_drain_selection();
                for (device_index, device) in runtime.devices().iter().enumerate() {
                    if drain_selection_contains(selection, device_index) {
                        device.drain_events();
                    }
                }
                #[cfg(feature = "irq")]
                if let Some(states) = BLOCK_IRQ_STATES.get() {
                    for state in states {
                        if drain_selection_contains(selection, state.device_index)
                            && state.on_drain_complete()
                        {
                            mark_block_drain_device(state.device_index);
                        }
                    }
                }
            },
            "block_drain".to_string(),
            ax_config::TASK_STACK_SIZE,
        );
    });
}

#[cfg(all(
    test,
    feature = "fs",
    any(not(feature = "plat-dyn"), target_os = "none")
))]
mod tests {
    use super::*;

    #[test]
    fn directed_drain_selects_only_marked_devices() {
        let selected = selected_drain_device_indices(4, false, 0b0100);

        assert_eq!(selected.as_slice(), &[2]);
    }

    #[test]
    fn full_scan_selects_all_devices() {
        let selected = selected_drain_device_indices(3, true, 0);

        assert_eq!(selected.as_slice(), &[0, 1, 2]);
    }
}
