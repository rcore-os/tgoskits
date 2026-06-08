use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_fs_ng::{
    block_runtime::{
        BlockDeviceHandle, BlockDmaBuffer, BlockDmaDirection, BlockDmaProvider, BlockDrainWake,
        BlockIrqBridge, BlockRuntime, BlockRuntimeConfig,
    },
    os::{AddressTranslator, BlockTaskOps, BlockTimeProvider},
};
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};
use rdif_block::{BlkError, IQueue};
use spin::Once;

static BLOCK_DRAIN_WQ: ax_task::WaitQueue = ax_task::WaitQueue::new();
static BLOCK_DRAIN_DEVICE_BITS: AtomicU64 = AtomicU64::new(0);
static BLOCK_DRAIN_FULL_SCAN: AtomicBool = AtomicBool::new(false);
static BLOCK_DRAIN_SPAWNED: Once<()> = Once::new();
static BLOCK_RUNTIME: Once<Arc<BlockRuntime>> = Once::new();
#[cfg(feature = "irq")]
static BLOCK_IRQ_STATES: Once<Vec<&'static BlockIrqState>> = Once::new();

struct RuntimeTimeProvider;

impl BlockTimeProvider for RuntimeTimeProvider {
    fn wall_time(&self) -> core::time::Duration {
        ax_hal::time::wall_time()
    }
}

struct RuntimeAddressTranslator;

impl AddressTranslator for RuntimeAddressTranslator {
    fn virt_to_phys(&self, vaddr: usize) -> usize {
        ax_hal::mem::virt_to_phys(ax_hal::mem::VirtAddr::from(vaddr)).as_usize()
    }
}

static TIME_PROVIDER: RuntimeTimeProvider = RuntimeTimeProvider;
static ADDRESS_TRANSLATOR: RuntimeAddressTranslator = RuntimeAddressTranslator;
static TASK_OPS: RuntimeTaskOps = RuntimeTaskOps;

struct RuntimeTaskOps;

impl BlockTaskOps for RuntimeTaskOps {
    fn current_task_id(&self) -> Option<u64> {
        ax_task::current_may_uninit().map(|curr| curr.id().as_u64())
    }

    fn task_yield(&self) {
        ax_task::yield_now();
    }

    fn wake_task(&self, task_id: u64) {
        let _ = ax_task::wake_task_by_id(task_id);
    }
}

struct RuntimeDrainWake {
    device_index: usize,
}

impl BlockDrainWake for RuntimeDrainWake {
    fn wake_drain(&self) {
        mark_block_drain_device(self.device_index);
    }
}

#[derive(Clone, Copy)]
struct DrainSelection {
    full_scan: bool,
    device_bits: u64,
}

fn mark_block_drain_device(device_index: usize) {
    if device_index < u64::BITS as usize {
        BLOCK_DRAIN_DEVICE_BITS.fetch_or(1 << device_index, Ordering::AcqRel);
    } else {
        BLOCK_DRAIN_FULL_SCAN.store(true, Ordering::Release);
    }
    BLOCK_DRAIN_WQ.notify_one(false);
}

fn block_drain_has_pending() -> bool {
    BLOCK_DRAIN_FULL_SCAN.load(Ordering::Acquire)
        || BLOCK_DRAIN_DEVICE_BITS.load(Ordering::Acquire) != 0
}

fn take_block_drain_selection() -> DrainSelection {
    DrainSelection {
        full_scan: BLOCK_DRAIN_FULL_SCAN.swap(false, Ordering::AcqRel),
        device_bits: BLOCK_DRAIN_DEVICE_BITS.swap(0, Ordering::AcqRel),
    }
}

fn drain_selection_contains(selection: DrainSelection, device_index: usize) -> bool {
    selection.full_scan
        || (device_index < u64::BITS as usize && selection.device_bits & (1 << device_index) != 0)
}

#[cfg(test)]
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

struct RuntimeDmaProvider;

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

struct RuntimeDmaBuffer {
    buffer: ContiguousArray<u8>,
}

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

#[cfg(feature = "irq")]
struct BlockIrqState {
    handler: ax_driver::block::BlockIrqHandler,
    device: Arc<BlockDeviceHandle>,
    device_index: usize,
    irq_handle: Once<axklib::irq::IrqHandle>,
}

#[cfg(feature = "irq")]
impl BlockIrqState {
    fn on_drain_complete(&self) -> bool {
        let event = self.handler.on_drain_complete();
        self.device.record_driver_event_for_pending(event)
    }
}

#[cfg(feature = "irq")]
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

#[cfg(feature = "irq")]
impl Drop for BlockIrqState {
    fn drop(&mut self) {
        if let Some(handle) = self.irq_handle.get().copied()
            && let Err(err) = axklib::irq::free(handle)
        {
            warn!("failed to free block irq handler: {err:?}");
        }
    }
}

pub(super) fn init(bootargs: Option<&str>) {
    init_fs_from_raw_blocks(take_raw_block_devices(), bootargs);
}

fn take_raw_block_devices() -> Vec<ax_driver::block::RawBlockDevice> {
    ax_driver::block::take_raw_block_devices()
}

fn init_fs_from_raw_blocks(blocks: Vec<ax_driver::block::RawBlockDevice>, bootargs: Option<&str>) {
    ax_fs_ng::os::set_time_provider(&TIME_PROVIDER);
    ax_fs_ng::os::set_address_translator(&ADDRESS_TRANSLATOR);
    ax_fs_ng::os::set_task_ops(&TASK_OPS);

    let runtime = Arc::new(build_block_runtime(blocks));
    BLOCK_RUNTIME.call_once(|| runtime.clone());
    spawn_block_drain_task(runtime.clone());
    ax_fs_ng::root::init_root(runtime.devices().iter().cloned(), bootargs);
}

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

#[cfg(not(feature = "irq"))]
type RegisteredBlockDevice = Arc<BlockDeviceHandle>;

#[cfg(feature = "irq")]
type RegisteredBlockDevice = (Arc<BlockDeviceHandle>, Vec<&'static BlockIrqState>);

#[cfg(feature = "irq")]
type OwnedBlockIrqStates = Vec<Box<BlockIrqState>>;

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
            Arc::new(RuntimeDrainWake { device_index }),
        ),
    )
    .map_err(ax_fs_ng::block_runtime::map_blk_err_to_ax_err)?;

    #[cfg(feature = "irq")]
    let irq_states =
        match register_irq_handlers(&mut block, device.clone(), device_index).and_then(|states| {
            block.enable_irq();
            if block.interface().is_irq_enabled() {
                Ok(activate_irq_states(states))
            } else {
                Err((ax_errno::AxError::Unsupported, states))
            }
        }) {
            Ok(states) => states,
            Err((err, states)) => {
                block.interface().disable_irq();
                drop(states);
                warn!(
                    "submit/poll filesystem block device {name} falls back to polling without \
                     IRQ: {err:?}"
                );
                Vec::new()
            }
        };

    info!("registered submit/poll filesystem block device {name}");
    #[cfg(feature = "irq")]
    return Ok((device, irq_states));
    #[cfg(not(feature = "irq"))]
    Ok(device)
}

#[cfg(feature = "irq")]
fn register_irq_handlers(
    block: &mut ax_driver::block::RawBlockDevice,
    device: Arc<BlockDeviceHandle>,
    device_index: usize,
) -> Result<OwnedBlockIrqStates, (ax_errno::AxError, OwnedBlockIrqStates)> {
    let irq_sources = block.interface().irq_sources();
    if irq_sources.is_empty() {
        return Err((ax_errno::AxError::Unsupported, Vec::new()));
    }

    let mut states = Vec::new();
    for source in irq_sources {
        let Some((irq_num, handler)) = block.take_irq_handler(source.id) else {
            return Err((ax_errno::AxError::Unsupported, states));
        };
        let mut state = Box::new(BlockIrqState {
            handler,
            device: device.clone(),
            device_index,
            irq_handle: Once::new(),
        });
        let data = NonNull::from(state.as_mut()).cast();
        let handle = match axklib::irq::request_shared(irq_num, handle_block_irq, data) {
            Ok(handle) => handle,
            Err(err) => return Err((err, states)),
        };
        if let Err(err) = axklib::irq::enable(handle) {
            let _ = axklib::irq::free(handle);
            return Err((err, states));
        }
        state.irq_handle.call_once(|| handle);
        states.push(state);
    }
    Ok(states)
}

#[cfg(feature = "irq")]
fn activate_irq_states(states: OwnedBlockIrqStates) -> Vec<&'static BlockIrqState> {
    states
        .into_iter()
        .map(|state| Box::leak(state) as &'static BlockIrqState)
        .collect()
}

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

#[cfg(test)]
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
