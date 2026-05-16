use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    num::NonZeroUsize,
    ptr::NonNull,
    time::Duration,
};

use ramdisk::RamDisk;
use rd_block::Block;

struct ExampleDmaOp;

impl dma_api::DmaOp for ExampleDmaOp {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn map_single(
        &self,
        _dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        _direction: dma_api::DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        let layout = Layout::from_size_align(size.get(), align.max(1))?;
        let dma_addr = (addr.as_ptr() as usize as u64).into();
        Ok(unsafe { dma_api::DmaMapHandle::new(addr, dma_addr, layout, None) })
    }

    unsafe fn unmap_single(&self, _handle: dma_api::DmaMapHandle) {}

    unsafe fn alloc_coherent(&self, _dma_mask: u64, layout: Layout) -> Option<dma_api::DmaHandle> {
        let ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(ptr)?;
        let dma_addr = (ptr.as_ptr() as usize as u64).into();
        Some(unsafe { dma_api::DmaHandle::new(ptr, dma_addr, layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) }
    }
}

static DMA_OP: ExampleDmaOp = ExampleDmaOp;

fn main() {
    let mut block = Block::new(RamDisk::new(16, 1024), &DMA_OP);
    let mut queue = block.create_queue().expect("queue must be created");

    let irq = block.irq_handler();
    std::thread::spawn(move || {
        loop {
            irq.handle();
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let result = queue.read_blocks_blocking(3, 2);
    for block in result {
        println!("read: {:?}", block.expect("read should succeed"));
    }

    let size = queue.block_size();
    let mut data = vec![0xAAu8; size];
    data.extend(vec![0xBBu8; size]);

    let result = queue.write_blocks_blocking(3, &data);
    for write in result {
        println!("write: {:?}", write);
    }

    let result = queue.read_blocks_blocking(3, 2);
    for block in result {
        println!("after write: {:?}", block.expect("read should succeed"));
    }
}
