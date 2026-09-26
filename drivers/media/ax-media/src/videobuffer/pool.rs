//! Buffer VbPool——基于 [`VbBuffer`] 的池化抽象。
//!
//! 对应原 `Vb2Queue` 的 “队列” 心智易误导为严格 FIFO。实际 `v4l2_buffer.index`
//! 允许按索引随机 `QBUF/DQBUF`，仅 `done_queue` 的 `DQBUF` 为 FIFO。本模块
//! 按新心智重命名：
//! * `VbPool`：`reqbufs` 构建、生命周期至 `streamoff`/`reqbufs(0)`
//! * `ready_queue`：`QBUF` 激活的缓冲（含 `Ready` 与 `Active`）
//! * `done_queue`：已完成（`Done`/`Error`）待 `DQBUF` 的缓冲
//! * `VbPoolLease`：驱动从 `VbPool` 获取的安全读写租约，`Drop` 自动 `abort`

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::any::Any;

use ax_sync::Mutex;
use axpoll::IoEvents;
use axpoll_set::PollSet;

use super::{
    allocator::VbMemOps,
    buf::{ActiveFrame, BufferState, Timestamp, VbBuffer},
};
use crate::V4l2Error;

/// 由内部锁保护的池状态。
pub(crate) struct VbPoolInner {
    buffers: Vec<VbBuffer>,
    ready_queue: VecDeque<u32>,
    done_queue: VecDeque<u32>,

    sequence: u32,
    generation: u64,
    streaming: bool,
    error: bool,
}

/// Buffer VbPool——`reqbufs` 构建、`streamoff` 销毁。
///
/// `ready_queue` 存放 `Ready`/`Active` 缓冲索引，`done_queue` 存放 `Done`/`Error`。
pub struct VbPool<M: VbMemOps> {
    pub(crate) state: Mutex<VbPoolInner>,
    pub(crate) poll_set: Arc<PollSet>,
    allocator: M,
    monotonic_nanos: fn() -> u64,
    min_buffers: u32,
    max_buffers: u32,
}

impl<M: VbMemOps> VbPool<M> {
    pub fn new(alloc: M, min_buffers: u32, max_buffers: u32, monotonic_nanos: fn() -> u64) -> Self {
        Self {
            state: Mutex::new(VbPoolInner {
                buffers: Vec::new(),
                ready_queue: VecDeque::new(),
                done_queue: VecDeque::new(),
                sequence: 0,
                generation: 0,
                streaming: false,
                error: false,
            }),
            allocator: alloc,
            monotonic_nanos,
            poll_set: Arc::new(PollSet::new()),
            min_buffers,
            max_buffers,
        }
    }

    // ── mmap ───────────────────────────────────────────────────────
    pub fn mmap(
        &self,
        offset: u64,
        length: u64,
    ) -> Option<(Vec<usize>, Arc<dyn Any + Send + Sync>)> {
        if length == 0 {
            return None;
        }
        let inner = self.state.lock();
        for vb in inner.buffers.iter() {
            let plane = vb.planes.first()?;
            let base = plane.offset as u64;
            let end = base + plane.length as u64;
            if offset >= base && offset < end {
                if offset.checked_add(length)? > end {
                    return None;
                }
                let sub = (offset - base) as usize;
                let page = 4096usize;
                let first_page = sub / page;
                let n_pages = (sub % page + length as usize).div_ceil(page);
                let (all, retain) = self.allocator.mmap(plane)?;
                let addrs = all.get(first_page..first_page + n_pages)?.to_vec();
                return Some((addrs, retain));
            }
        }
        None
    }

    // ── poll ───────────────────────────────────────────────────────
    pub fn vb_poll_set(&self) -> &Arc<PollSet> {
        &self.poll_set
    }

    pub fn is_readable(&self) -> bool {
        !self.state.lock().done_queue.is_empty()
    }

    pub fn is_error(&self) -> bool {
        self.state.lock().error
    }

    pub fn is_streaming(&self) -> bool {
        self.state.lock().streaming
    }

    /// Signal an error while preserving ownership of every live frame lease.
    pub fn set_error(&self) {
        self.state.lock().error = true;
        unsafe { self.poll_set.wake(IoEvents::ERR) };
    }

    // ── 驱动侧：VbPoolLease ─────────────────────────────────────────────
    pub(crate) fn acquire_frame(&self) -> Option<ActiveFrame> {
        let mut inner = self.state.lock();
        if inner.error || !inner.streaming {
            return None;
        }
        let generation = inner.generation;
        let index = inner.ready_queue.iter().copied().find(|&index| {
            inner.buffers.get(index as usize).is_some_and(|buffer| {
                buffer.state == BufferState::Ready
                    && buffer.planes.first().is_some_and(|plane| plane.length != 0)
            })
        })?;
        let buffer = &mut inner.buffers[index as usize];
        let plane = buffer.planes.first()?;
        buffer.state = BufferState::Active;
        Some(ActiveFrame {
            buffer_index: index,
            generation,
            data_ptr: plane.as_ptr(),
            len: plane.length as usize,
        })
    }

    /// 获取安全租约——始终返回 `VbPoolLease`，内部 `frame` 可能为 `None`（池空）。
    /// 驱动通过 `as_mut_slice` 安全访问，`commit` 时自动重填下一帧。
    pub fn acquire(self: &Arc<Self>) -> VbPoolLease<M> {
        let frame = self.acquire_frame();
        VbPoolLease::new(Arc::clone(self), frame)
    }

    fn commit_frame(&self, frame: ActiveFrame, bytesused: u32) {
        self.commit_inner(frame, bytesused, BufferState::Done)
    }

    fn abort_frame(&self, frame: ActiveFrame) {
        self.commit_inner(frame, 0, BufferState::Error)
    }

    fn commit_inner(&self, frame: ActiveFrame, bytesused: u32, state: BufferState) {
        let mut inner = self.state.lock();
        if frame.generation != inner.generation {
            return;
        }
        let error = inner.error;
        let Some(vb) = inner.buffers.get_mut(frame.buffer_index as usize) else {
            return;
        };
        if vb.state != BufferState::Active {
            return;
        }
        vb.bytesused = if error { 0 } else { bytesused };
        vb.timestamp = Timestamp::Monotonic((self.monotonic_nanos)());
        vb.state = if error { BufferState::Error } else { state };
        inner.sequence += 1;
        inner.buffers[frame.buffer_index as usize].sequence = inner.sequence;
        inner.done_queue.push_back(frame.buffer_index);
        drop(inner);
        unsafe { self.poll_set.wake(IoEvents::IN | IoEvents::ERR) };
    }
}

/// Ioctl 委托——`reqbufs/qbuf/dqbuf/streamon/streamoff` 直接调用的池操作。
impl<M: VbMemOps> VbPool<M> {
    pub fn reqbufs(&self, count: u32, plane_sizes: &[u32]) -> Result<(), V4l2Error> {
        let mut inner = self.state.lock();
        if inner.streaming {
            log::warn!(
                "[vb2] reqbufs Busy: streaming=true count={} buffers={}",
                count,
                inner.buffers.len()
            );
            return Err(V4l2Error::Busy);
        }
        if count == 0 {
            inner.generation = inner.generation.wrapping_add(1);
            for vb in inner.buffers.drain(..) {
                self.allocator.release(&vb.planes);
            }
            inner.ready_queue.clear();
            inner.done_queue.clear();
            inner.sequence = 0;
            return Ok(());
        }
        if plane_sizes.len() != 1 {
            return Err(V4l2Error::InvalidArgument);
        }
        let num_buffers = count.clamp(self.min_buffers, self.max_buffers);
        let mut tmp = Vec::with_capacity(num_buffers as usize);
        for _ in 0..num_buffers {
            match self.allocator.alloc(plane_sizes) {
                Ok(planes) => tmp.push(VbBuffer {
                    state: BufferState::Free,
                    planes,
                    bytesused: 0,
                    sequence: 0,
                    timestamp: Timestamp::Unset,
                }),
                Err(e) => {
                    // 回收已分配的缓冲
                    for vb in tmp {
                        self.allocator.release(&vb.planes);
                    }
                    return Err(e);
                }
            }
        }
        for vb in inner.buffers.drain(..) {
            self.allocator.release(&vb.planes);
        }
        inner.ready_queue.clear();
        inner.done_queue.clear();
        inner.sequence = 0;
        inner.generation = inner.generation.wrapping_add(1);
        inner.buffers = tmp;
        Ok(())
    }

    pub fn qbuf(&self, index: u32) -> Result<(), V4l2Error> {
        let mut inner = self.state.lock();
        if inner.error {
            return Err(V4l2Error::Io);
        }
        let vb = inner
            .buffers
            .get_mut(index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;
        if vb.state != BufferState::Free {
            return Err(V4l2Error::InvalidArgument);
        }
        vb.state = BufferState::Ready;
        inner.ready_queue.push_back(index);
        Ok(())
    }

    pub fn dqbuf(&self) -> Result<u32, V4l2Error> {
        let mut inner = self.state.lock();
        let idx = inner.done_queue.pop_front().ok_or(V4l2Error::Busy)?;
        let vb = &mut inner.buffers[idx as usize];
        if vb.state != BufferState::Done && vb.state != BufferState::Error {
            return Err(V4l2Error::InvalidArgument);
        }
        vb.state = BufferState::Free;
        if let Some(pos) = inner.ready_queue.iter().position(|&i| i == idx) {
            inner.ready_queue.remove(pos);
        }
        Ok(idx)
    }

    pub fn prepare_buf(&self, index: u32) -> Result<(), V4l2Error> {
        let mut inner = self.state.lock();
        if inner.error {
            return Err(V4l2Error::Io);
        }
        let vb = inner
            .buffers
            .get_mut(index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;
        if vb.state != BufferState::Free {
            return Err(V4l2Error::InvalidArgument);
        }
        Ok(())
    }

    pub fn streamon(&self) -> Result<(), V4l2Error> {
        let mut inner = self.state.lock();
        if inner.streaming {
            return Err(V4l2Error::Busy);
        }
        if inner.buffers.is_empty() {
            return Err(V4l2Error::InvalidArgument);
        }
        inner.streaming = true;
        Ok(())
    }

    pub fn streamoff(&self) {
        let mut inner = self.state.lock();
        inner.generation = inner.generation.wrapping_add(1);
        for vb in &mut inner.buffers {
            if vb.state != BufferState::Free {
                vb.state = BufferState::Free;
            }
        }
        inner.streaming = false;
        inner.error = false;
        inner.ready_queue.clear();
        inner.done_queue.clear();
        drop(inner);
        unsafe { self.poll_set.wake(IoEvents::IN | IoEvents::ERR) };
    }

    pub fn buffer_snapshot(&self, index: u32) -> Option<VbBuffer> {
        self.state.lock().buffers.get(index as usize).cloned()
    }

    pub fn num_buffers(&self) -> u32 {
        self.state.lock().buffers.len() as u32
    }
}

/// 驱动从 `VbPool` 获取的安全读写租约。
pub struct VbPoolLease<M: VbMemOps> {
    pool: Arc<VbPool<M>>,
    frame: Option<ActiveFrame>,
}

impl<M: VbMemOps> VbPoolLease<M> {
    fn new(pool: Arc<VbPool<M>>, frame: Option<ActiveFrame>) -> Self {
        Self { pool, frame }
    }

    /// 尝试获取可写缓冲守卫。
    pub fn try_acquire(&mut self) -> Option<FrameGuard<'_, M>> {
        if self.frame.is_none() {
            self.frame = self.pool.acquire_frame();
        }

        if self.frame.is_some() {
            Some(FrameGuard::new(self))
        } else {
            None
        }
    }

    /// 独占可写视图，驱动应通过此接口填充数据，避免裸指针 `copy`
    fn as_mut_slice(&mut self) -> &mut [u8] {
        let f = self.frame.as_mut().expect("frame already taken");
        unsafe { core::slice::from_raw_parts_mut(f.data_ptr, f.len) }
    }

    fn commit(&mut self, bytesused: u32) {
        if let Some(frame) = self.frame.take() {
            self.pool.commit_frame(frame, bytesused);
        }
        self.frame = self.pool.acquire_frame();
    }

    fn abort(&mut self) {
        if let Some(frame) = self.frame.take() {
            self.pool.abort_frame(frame);
        }
        self.frame = self.pool.acquire_frame();
    }
}

impl<M: VbMemOps> Drop for VbPoolLease<M> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            self.pool.abort_frame(frame);
        }
    }
}

pub struct FrameGuard<'a, M: VbMemOps> {
    lease: &'a mut VbPoolLease<M>,
}

impl<'a, M: VbMemOps> FrameGuard<'a, M> {
    pub fn new(lease: &'a mut VbPoolLease<M>) -> Self {
        Self { lease }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.lease.as_mut_slice()
    }

    pub fn commit(self, bytesused: u32) {
        self.lease.commit(bytesused)
    }

    pub fn abort(self) {
        self.lease.abort()
    }
}
