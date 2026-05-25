use alloc::{
    string::{String, ToString},
    vec::Vec,
};
#[cfg(feature = "irq")]
use alloc::{
    sync::{Arc, Weak},
    task::Wake,
};
#[cfg(feature = "irq")]
use core::{
    future::Future,
    pin::pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use log::{error, warn};
use rd_block::BlkError;
use rdrive::Device;
pub struct Block {
    name: String,
    irq_num: Option<usize>,
    #[cfg(feature = "irq")]
    irq_state: Option<Arc<BlockIrqState>>,
    queue: SpinNoIrq<rd_block::CmdQueue>,
}

pub struct PlatformBlockDevice {
    name: String,
    block: Option<rd_block::Block>,
    irq_num: Option<usize>,
}

impl PlatformBlockDevice {
    fn new(name: String, block: rd_block::Block, irq_num: Option<usize>) -> Self {
        Self {
            name,
            block: Some(block),
            irq_num,
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(feature = "irq")]
struct BlockIrqState {
    handler: rd_block::IrqHandler,
}

#[cfg(feature = "irq")]
impl BlockIrqState {
    fn handle_irq(&self) {
        self.handler.handle();
    }
}

#[cfg(feature = "irq")]
struct BlockIrqWaiter {
    woken: AtomicBool,
}

#[cfg(feature = "irq")]
impl Wake for BlockIrqWaiter {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }
}

#[cfg(feature = "irq")]
const BLOCK_IRQ_SLOTS: usize = 16;
#[cfg(feature = "irq")]
static IRQ_SOURCES: [SpinNoIrq<Option<Weak<BlockIrqState>>>; BLOCK_IRQ_SLOTS] =
    [const { SpinNoIrq::new(None) }; BLOCK_IRQ_SLOTS];
#[cfg(feature = "irq")]
const BLOCK_IRQ_REPOLL_SPINS: usize = 256;

impl Block {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    fn use_irq_completion(&self) -> bool {
        #[cfg(feature = "irq")]
        {
            self.irq_state.is_some()
        }

        #[cfg(not(feature = "irq"))]
        {
            false
        }
    }

    fn read_blocks_wait(
        queue: &mut rd_block::CmdQueue,
        block_id: usize,
        block_count: usize,
        use_irq: bool,
    ) -> Vec<Result<rd_block::BlockData, BlkError>> {
        #[cfg(feature = "irq")]
        {
            if use_irq {
                return wait_on_block_irq(queue.read_blocks(block_id, block_count));
            }
        }
        #[cfg(not(feature = "irq"))]
        let _ = use_irq;

        queue.read_blocks_blocking(block_id, block_count)
    }

    fn write_blocks_wait(
        queue: &mut rd_block::CmdQueue,
        block_id: usize,
        data: &[u8],
        use_irq: bool,
    ) -> Vec<Result<(), BlkError>> {
        #[cfg(feature = "irq")]
        {
            if use_irq {
                return wait_on_block_irq(queue.write_blocks(block_id, data));
            }
        }
        #[cfg(not(feature = "irq"))]
        let _ = use_irq;

        queue.write_blocks_blocking(block_id, data)
    }

    pub fn num_blocks(&self) -> u64 {
        self.queue.lock().num_blocks() as _
    }

    pub fn block_size(&self) -> usize {
        self.queue.lock().block_size()
    }

    pub fn flush(&mut self) -> AxResult {
        Ok(())
    }

    pub fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let use_irq = self.use_irq_completion();
        let mut queue = self.queue.lock();
        let block_count = buf.len() / block_size;
        let blocks = Self::read_blocks_wait(&mut queue, block_id as usize, block_count, use_irq);
        let mut copied = 0;
        for block in blocks {
            let block = block.map_err(map_blk_err_to_ax_err)?;
            let end = copied + block.len();
            if end > buf.len() {
                return Err(AxError::Io);
            }
            buf[copied..end].copy_from_slice(&block);
            copied = end;
        }
        if copied != buf.len() {
            return Err(AxError::Io);
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let use_irq = self.use_irq_completion();
        let mut queue = self.queue.lock();
        let blocks = Self::write_blocks_wait(&mut queue, block_id as usize, buf, use_irq);
        for block in blocks {
            block.map_err(map_blk_err_to_ax_err)?;
        }
        Ok(())
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for Block {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let irq_num = dev.irq_num;
        let mut block = dev.block.take().ok_or(AxError::BadState)?;
        #[cfg(feature = "irq")]
        let irq_handler = irq_num.map(|_| block.irq_handler());
        let queue = block.create_queue().ok_or(AxError::BadState)?;
        drop(dev);

        #[cfg(feature = "irq")]
        let irq_state = if let (Some(irq_num), Some(handler)) = (irq_num, irq_handler) {
            let state = Arc::new(BlockIrqState { handler });
            if let Some(slot) = reserve_block_irq_slot(&state) {
                if axklib::irq::register(irq_num, BLOCK_IRQ_HANDLERS[slot]) {
                    Some(state)
                } else {
                    release_block_irq_slot(slot, &state);
                    warn!("failed to register block irq handler for irq {irq_num}");
                    None
                }
            } else {
                warn!("no free block irq source slot for irq {irq_num}");
                None
            }
        } else {
            None
        };
        #[cfg(feature = "irq")]
        let irq_num = irq_state.as_ref().and(irq_num);
        #[cfg(not(feature = "irq"))]
        let irq_num = {
            let _ = irq_num;
            None
        };

        Ok(Self {
            name,
            irq_num,
            #[cfg(feature = "irq")]
            irq_state,
            queue: SpinNoIrq::new(queue),
        })
    }
}

pub trait PlatformDeviceBlock {
    fn register_block<T: rd_block::Interface>(self, dev: T);
    fn register_block_with_irq<T: rd_block::Interface>(self, dev: T, irq_num: Option<usize>);
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: rd_block::Interface>(self, dev: T) {
        self.register_block_with_irq(dev, None);
    }

    fn register_block_with_irq<T: rd_block::Interface>(self, dev: T, irq_num: Option<usize>) {
        #[cfg(feature = "irq")]
        let mut dev = dev;
        #[cfg(feature = "irq")]
        if irq_num.is_some() {
            dev.enable_irq();
        }
        #[cfg(not(feature = "irq"))]
        let dev = {
            let _ = irq_num;
            dev
        };
        let name = dev.name().to_string();
        let block = rd_block::Block::new(dev, axklib::dma::op());
        #[cfg(feature = "irq")]
        let registered_irq_num = irq_num;
        #[cfg(not(feature = "irq"))]
        let registered_irq_num = None;
        self.register(PlatformBlockDevice::new(name, block, registered_irq_num));
    }
}

pub fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    decode_irq_cells(&interrupt.specifier)
}

fn decode_irq_cells(specifier: &[u32]) -> Option<usize> {
    match specifier {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

pub fn take_block_devices() -> Vec<Block> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|dev| match Block::try_from(dev) {
            Ok(block) => Some(block),
            Err(err) => {
                warn!("failed to take block device: {err:?}");
                None
            }
        })
        .collect()
}

#[cfg(feature = "irq")]
fn handle_block_irq(slot: usize) {
    let Some(state) = IRQ_SOURCES[slot].lock().as_ref().and_then(Weak::upgrade) else {
        return;
    };
    state.handle_irq();
}

#[cfg(feature = "irq")]
fn handle_block_irq_slot<const SLOT: usize>(_: usize) {
    handle_block_irq(SLOT);
}

#[cfg(feature = "irq")]
const BLOCK_IRQ_HANDLERS: [fn(usize); BLOCK_IRQ_SLOTS] = [
    handle_block_irq_slot::<0>,
    handle_block_irq_slot::<1>,
    handle_block_irq_slot::<2>,
    handle_block_irq_slot::<3>,
    handle_block_irq_slot::<4>,
    handle_block_irq_slot::<5>,
    handle_block_irq_slot::<6>,
    handle_block_irq_slot::<7>,
    handle_block_irq_slot::<8>,
    handle_block_irq_slot::<9>,
    handle_block_irq_slot::<10>,
    handle_block_irq_slot::<11>,
    handle_block_irq_slot::<12>,
    handle_block_irq_slot::<13>,
    handle_block_irq_slot::<14>,
    handle_block_irq_slot::<15>,
];

#[cfg(feature = "irq")]
fn reserve_block_irq_slot(state: &Arc<BlockIrqState>) -> Option<usize> {
    for (slot, source) in IRQ_SOURCES.iter().enumerate() {
        let mut source = source.lock();
        if source.as_ref().and_then(Weak::upgrade).is_none() {
            *source = Some(Arc::downgrade(state));
            return Some(slot);
        }
    }
    None
}

#[cfg(feature = "irq")]
fn release_block_irq_slot(slot: usize, state: &Arc<BlockIrqState>) {
    let mut source = IRQ_SOURCES[slot].lock();
    if source
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|registered| Arc::ptr_eq(&registered, state))
    {
        *source = None;
    }
}

#[cfg(feature = "irq")]
fn wait_on_block_irq<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waiter = Arc::new(BlockIrqWaiter {
        woken: AtomicBool::new(false),
    });
    let waker = Waker::from(waiter.clone());
    let mut cx = Context::from_waker(&waker);

    loop {
        waiter.woken.store(false, Ordering::Release);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => {
                // rd-block registers the waker after a Retry result, so keep a
                // bounded repoll fallback for completions racing that window.
                for _ in 0..BLOCK_IRQ_REPOLL_SPINS {
                    if waiter.woken.swap(false, Ordering::AcqRel) {
                        break;
                    }
                    core::hint::spin_loop();
                }
            }
        }
    }
}

fn map_blk_err_to_ax_err(err: BlkError) -> AxError {
    match err {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) => AxError::InvalidInput,
        BlkError::Other(error) => {
            error!("Block device error: {error}");
            AxError::Io
        }
    }
}
