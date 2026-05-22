#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::{
    boxed::Box,
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::{
    alloc::Layout,
    any::Any,
    cell::UnsafeCell,
    fmt::Debug,
    ops::{Deref, DerefMut},
    task::Poll,
};

use dma_api::{DArrayPool, DBuff, DeviceDma, DmaDirection, DmaOp};
use futures::task::AtomicWaker;
pub use rdif_block::*;

pub struct Block {
    inner: Arc<BlockInner>,
}

struct QueueWakerMap(UnsafeCell<BTreeMap<usize, Arc<AtomicWaker>>>);

impl QueueWakerMap {
    fn new() -> Self {
        Self(UnsafeCell::new(BTreeMap::new()))
    }

    fn register(&self, queue_id: usize) -> Arc<AtomicWaker> {
        let waker = Arc::new(AtomicWaker::new());
        unsafe { &mut *self.0.get() }.insert(queue_id, waker.clone());
        waker
    }

    fn wake(&self, queue_id: usize) {
        if let Some(waker) = unsafe { &*self.0.get() }.get(&queue_id) {
            waker.wake();
        }
    }
}

struct BlockInner {
    interface: UnsafeCell<Box<dyn Interface>>,
    dma_op: &'static dyn DmaOp,
    queue_waker_map: QueueWakerMap,
}

unsafe impl Send for BlockInner {}
unsafe impl Sync for BlockInner {}

struct IrqGuard<'a> {
    enabled: bool,
    inner: &'a Block,
}

impl<'a> Drop for IrqGuard<'a> {
    fn drop(&mut self) {
        if self.enabled {
            self.inner.interface().enable_irq();
        }
    }
}

impl DriverGeneric for Block {
    fn name(&self) -> &str {
        self.interface().name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl Block {
    pub fn new(interface: impl Interface, dma_op: &'static dyn DmaOp) -> Self {
        Self {
            inner: Arc::new(BlockInner {
                interface: UnsafeCell::new(Box::new(interface)),
                dma_op,
                queue_waker_map: QueueWakerMap::new(),
            }),
        }
    }

    pub fn typed_ref<T: Interface + 'static>(&self) -> Option<&T> {
        self.interface().raw_any()?.downcast_ref::<T>()
    }

    pub fn typed_mut<T: Interface + 'static>(&mut self) -> Option<&mut T> {
        self.interface().raw_any_mut()?.downcast_mut::<T>()
    }

    #[allow(clippy::mut_from_ref)]
    fn interface(&self) -> &mut dyn Interface {
        unsafe { &mut **self.inner.interface.get() }
    }

    fn irq_guard(&self) -> IrqGuard<'_> {
        let enabled = self.interface().is_irq_enabled();
        if enabled {
            self.interface().disable_irq();
        }
        IrqGuard {
            enabled,
            inner: self,
        }
    }

    pub fn create_queue_with_capacity(&mut self, capacity: usize) -> Option<CmdQueue> {
        let irq_guard = self.irq_guard();
        let queue = self.interface().create_queue()?;
        let queue_id = queue.id();
        let config = queue.buff_config();
        let block_size = queue.block_size();
        if block_size == 0 || config.size < block_size {
            return None;
        }
        let layout = Layout::from_size_align(config.size, config.align).ok()?;
        let dma = DeviceDma::new(config.dma_mask, self.inner.dma_op);
        let pool = dma.new_pool(layout, DmaDirection::FromDevice, capacity);
        let waker = self.inner.queue_waker_map.register(queue_id);
        drop(irq_guard);

        Some(CmdQueue::new(queue, waker, pool, config.size))
    }

    pub fn create_queue(&mut self) -> Option<CmdQueue> {
        self.create_queue_with_capacity(32)
    }

    pub fn irq_handler(&self) -> IrqHandler {
        IrqHandler {
            inner: self.inner.clone(),
        }
    }
}

pub struct IrqHandler {
    inner: Arc<BlockInner>,
}

unsafe impl Sync for IrqHandler {}

impl IrqHandler {
    pub fn handle(&self) {
        let iface = unsafe { &mut **self.inner.interface.get() };
        let event = iface.handle_irq();
        for id in event.queue.iter() {
            self.inner.queue_waker_map.wake(id);
        }
    }
}

pub struct CmdQueue {
    interface: Box<dyn IQueue>,
    waker: Arc<AtomicWaker>,
    pool: DArrayPool,
    buffer_size: usize,
}

impl CmdQueue {
    fn new(
        interface: Box<dyn IQueue>,
        waker: Arc<AtomicWaker>,
        pool: DArrayPool,
        buffer_size: usize,
    ) -> Self {
        Self {
            interface,
            waker,
            pool,
            buffer_size,
        }
    }

    pub fn id(&self) -> usize {
        self.interface.id()
    }

    pub fn num_blocks(&self) -> usize {
        self.interface.num_blocks()
    }

    pub fn block_size(&self) -> usize {
        self.interface.block_size()
    }

    pub fn max_blocks_per_request(&self) -> usize {
        let block_size = self.block_size();
        debug_assert!(block_size > 0);
        debug_assert!(self.buffer_size >= block_size);
        self.buffer_size / block_size
    }

    pub fn read_blocks(
        &mut self,
        blk_id: usize,
        blk_count: usize,
    ) -> impl core::future::Future<Output = Vec<Result<BlockData, BlkError>>> {
        let block_size = self.block_size();
        let request_ls = block_ranges(blk_id, blk_count, self.max_blocks_per_request(), block_size);
        ReadFuture::new(self, request_ls)
    }

    pub fn read_blocks_blocking(
        &mut self,
        blk_id: usize,
        blk_count: usize,
    ) -> Vec<Result<BlockData, BlkError>> {
        spin_on::spin_on(self.read_blocks(blk_id, blk_count))
    }

    pub async fn write_blocks(
        &mut self,
        start_blk_id: usize,
        data: &[u8],
    ) -> Vec<Result<(), BlkError>> {
        let block_size = self.block_size();
        assert_eq!(data.len() % block_size, 0);
        let max_blocks = self.max_blocks_per_request();
        let max_bytes = max_blocks * block_size;
        let mut block_vecs = Vec::new();
        for (i, chunk) in data.chunks(max_bytes).enumerate() {
            let blk_id = start_blk_id + i * max_blocks;
            block_vecs.push((blk_id, chunk));
        }
        WriteFuture::new(self, block_vecs).await
    }

    pub fn write_blocks_blocking(
        &mut self,
        start_blk_id: usize,
        data: &[u8],
    ) -> Vec<Result<(), BlkError>> {
        spin_on::spin_on(self.write_blocks(start_blk_id, data))
    }
}

pub struct BlockData {
    block_id: usize,
    data: DBuff,
    len: usize,
}

pub struct ReadFuture<'a> {
    queue: &'a mut CmdQueue,
    req_ls: Vec<(usize, usize)>,
    requested: BTreeMap<usize, Option<(DBuff, usize)>>,
    map: BTreeMap<usize, RequestId>,
    results: BTreeMap<usize, Result<BlockData, BlkError>>,
}

impl<'a> ReadFuture<'a> {
    fn new(queue: &'a mut CmdQueue, req_ls: Vec<(usize, usize)>) -> Self {
        Self {
            queue,
            req_ls,
            requested: BTreeMap::new(),
            map: BTreeMap::new(),
            results: BTreeMap::new(),
        }
    }
}

impl<'a> core::future::Future for ReadFuture<'a> {
    type Output = Vec<Result<BlockData, BlkError>>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let this = self.get_mut();

        for &(blk_id, len) in &this.req_ls {
            if this.results.contains_key(&blk_id) {
                continue;
            }

            if this.requested.contains_key(&blk_id) {
                continue;
            }

            match this.queue.pool.alloc() {
                Ok(buff) => {
                    let kind = RequestKind::Read(Buffer {
                        virt: buff.as_ptr().as_ptr(),
                        bus: buff.dma_addr().as_u64(),
                        size: len,
                    });

                    match this.queue.interface.submit_request(Request {
                        block_id: blk_id,
                        kind,
                    }) {
                        Ok(req_id) => {
                            this.map.insert(blk_id, req_id);
                            this.requested.insert(blk_id, Some((buff, len)));
                        }
                        Err(BlkError::Retry) => {
                            this.queue.waker.register(cx.waker());
                            return Poll::Pending;
                        }
                        Err(e) => {
                            this.results.insert(blk_id, Err(e));
                        }
                    }
                }
                Err(e) => {
                    this.results.insert(blk_id, Err(e.into()));
                }
            }
        }

        for (blk_id, buff) in &mut this.requested {
            if this.results.contains_key(blk_id) {
                continue;
            }

            let req_id = this.map[blk_id];

            match this.queue.interface.poll_request(req_id) {
                Ok(_) => {
                    let (data, len) = buff
                        .take()
                        .expect("DMA read buffer should exist until completion");
                    this.results.insert(
                        *blk_id,
                        Ok(BlockData {
                            block_id: *blk_id,
                            data,
                            len,
                        }),
                    );
                }
                Err(BlkError::Retry) => {
                    this.queue.waker.register(cx.waker());
                    return Poll::Pending;
                }
                Err(e) => {
                    this.results.insert(*blk_id, Err(e));
                }
            }
        }

        let mut out = Vec::with_capacity(this.req_ls.len());
        for (blk_id, _) in &this.req_ls {
            let result = this
                .results
                .remove(blk_id)
                .expect("all blocks should have completion results");
            out.push(result);
        }
        Poll::Ready(out)
    }
}

pub struct WriteFuture<'a, 'b> {
    queue: &'a mut CmdQueue,
    req_ls: Vec<(usize, &'b [u8])>,
    requested: BTreeSet<usize>,
    map: BTreeMap<usize, RequestId>,
    results: BTreeMap<usize, Result<(), BlkError>>,
}

impl<'a, 'b> WriteFuture<'a, 'b> {
    fn new(queue: &'a mut CmdQueue, req_ls: Vec<(usize, &'b [u8])>) -> Self {
        Self {
            queue,
            req_ls,
            requested: BTreeSet::new(),
            map: BTreeMap::new(),
            results: BTreeMap::new(),
        }
    }
}

impl<'a, 'b> core::future::Future for WriteFuture<'a, 'b> {
    type Output = Vec<Result<(), BlkError>>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let this = self.get_mut();
        for &(blk_id, buff) in &this.req_ls {
            if this.results.contains_key(&blk_id) {
                continue;
            }

            if this.requested.contains(&blk_id) {
                continue;
            }

            match this.queue.interface.submit_request(Request {
                block_id: blk_id,
                kind: RequestKind::Write(buff),
            }) {
                Ok(req_id) => {
                    this.map.insert(blk_id, req_id);
                    this.requested.insert(blk_id);
                }
                Err(BlkError::Retry) => {
                    this.queue.waker.register(cx.waker());
                    return Poll::Pending;
                }
                Err(e) => {
                    this.results.insert(blk_id, Err(e));
                }
            }
        }

        for blk_id in &this.requested {
            if this.results.contains_key(blk_id) {
                continue;
            }

            let req_id = this.map[blk_id];

            match this.queue.interface.poll_request(req_id) {
                Ok(_) => {
                    this.results.insert(*blk_id, Ok(()));
                }
                Err(BlkError::Retry) => {
                    this.queue.waker.register(cx.waker());
                    return Poll::Pending;
                }
                Err(e) => {
                    this.results.insert(*blk_id, Err(e));
                }
            }
        }

        let mut out = Vec::with_capacity(this.req_ls.len());
        for (blk_id, _) in &this.req_ls {
            let result = this
                .results
                .remove(blk_id)
                .expect("all blocks should have completion results");
            out.push(result);
        }
        Poll::Ready(out)
    }
}

impl Debug for BlockData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BlockData")
            .field("block_id", &self.block_id)
            .field("data", &self.deref())
            .finish()
    }
}

impl BlockData {
    pub fn block_id(&self) -> usize {
        self.block_id
    }
}

impl Deref for BlockData {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { core::slice::from_raw_parts(self.data.as_ptr().as_ptr(), self.len) }
    }
}

impl DerefMut for BlockData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { core::slice::from_raw_parts_mut(self.data.as_ptr().as_ptr(), self.len) }
    }
}

fn block_ranges(
    start_blk_id: usize,
    block_count: usize,
    max_blocks: usize,
    block_size: usize,
) -> Vec<(usize, usize)> {
    let max_blocks = max_blocks.max(1);
    let mut out = Vec::new();
    let mut blk_id = start_blk_id;
    let mut remaining = block_count;
    while remaining > 0 {
        let count = remaining.min(max_blocks);
        out.push((blk_id, count * block_size));
        blk_id += count;
        remaining -= count;
    }
    out
}

#[cfg(test)]
mod tests {
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

    use dma_api::{DmaError, DmaHandle, DmaMapHandle};

    use super::*;

    struct TestDma;

    static TEST_DMA: TestDma = TestDma;

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn map_single(
            &self,
            _dma_mask: u64,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _align: usize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 8)?;
            Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
        }

        unsafe fn unmap_single(&self, _handle: DmaMapHandle) {}

        unsafe fn alloc_coherent(&self, _dma_mask: u64, layout: Layout) -> Option<DmaHandle> {
            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr)?;
            Some(unsafe { DmaHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
            unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }
    }

    struct TestBlock {
        block_size: usize,
        buffer_size: usize,
    }

    impl DriverGeneric for TestBlock {
        fn name(&self) -> &str {
            "test-block"
        }
    }

    impl Interface for TestBlock {
        fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
            Some(Box::new(TestQueue {
                block_size: self.block_size,
                buffer_size: self.buffer_size,
            }))
        }

        fn enable_irq(&mut self) {}

        fn disable_irq(&mut self) {}

        fn is_irq_enabled(&self) -> bool {
            false
        }

        fn handle_irq(&mut self) -> Event {
            Event::none()
        }
    }

    struct TestQueue {
        block_size: usize,
        buffer_size: usize,
    }

    impl IQueue for TestQueue {
        fn id(&self) -> usize {
            0
        }

        fn num_blocks(&self) -> usize {
            1
        }

        fn block_size(&self) -> usize {
            self.block_size
        }

        fn buff_config(&self) -> BuffConfig {
            BuffConfig {
                dma_mask: u64::MAX,
                align: 8,
                size: self.buffer_size,
            }
        }

        fn submit_request(&mut self, _request: Request<'_>) -> Result<RequestId, BlkError> {
            Ok(RequestId::new(0))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<(), BlkError> {
            Ok(())
        }
    }

    #[test]
    fn create_queue_rejects_buffer_smaller_than_block_size() {
        let mut block = Block::new(
            TestBlock {
                block_size: 512,
                buffer_size: 256,
            },
            &TEST_DMA,
        );

        assert!(block.create_queue_with_capacity(1).is_none());
    }
}
