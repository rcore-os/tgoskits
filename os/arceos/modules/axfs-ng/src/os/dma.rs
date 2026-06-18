use core::sync::atomic::{AtomicBool, Ordering};

use dma_api::DmaOp;
use spin::Once;

static DMA_OP: Once<&'static dyn DmaOp> = Once::new();
static DMA_READY: AtomicBool = AtomicBool::new(false);

pub fn install_dma_op(op: &'static dyn DmaOp) {
    DMA_OP.call_once(|| op);
    DMA_READY.store(true, Ordering::Release);
}

pub fn dma_op() -> Option<&'static dyn DmaOp> {
    DMA_OP.get().copied()
}

pub fn has_dma_op() -> bool {
    DMA_READY.load(Ordering::Acquire)
}
