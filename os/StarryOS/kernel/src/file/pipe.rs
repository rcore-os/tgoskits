use alloc::{borrow::Cow, collections::VecDeque, format, sync::Arc};
use core::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_memory_addr::PAGE_SIZE_4K;
use ax_task::{
    current,
    future::{block_on, poll_io},
};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::{
    general::{O_RDONLY, O_WRONLY, S_IFIFO},
    ioctl::FIONREAD,
};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use starry_signal::{SignalInfo, Signo};
use starry_vm::VmMutPtr;

use super::{FileLike, Kstat};
use crate::{
    StarryError, StarryResult,
    file::{IoDst, IoSrc},
    sync::Mutex,
    task::{AsThread, send_signal_to_process},
};

const RING_BUFFER_INIT_SIZE: usize = 65536; // 64 KiB

const RING_BUFFER_MAX_SIZE: usize = 1024 * 1024; // 1 MiB

const PIPE_BUF: usize = PAGE_SIZE_4K;

struct Shared {
    state: Mutex<PipeState>,
    poll_rx: PollSet,
    poll_tx: PollSet,
}

struct PipeState {
    buffer: HeapRb<u8>,
    buffers: VecDeque<usize>,
    readers: usize,
    writers: usize,
}

impl PipeState {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: HeapRb::new(capacity),
            buffers: VecDeque::new(),
            readers: 1,
            writers: 1,
        }
    }

    fn has_free_buffer(&self) -> bool {
        self.buffers.len() < self.buffer.capacity().get() / PIPE_BUF
    }

    fn can_merge(&self, bytes: usize) -> bool {
        self.buffers
            .back()
            .is_some_and(|length| length + bytes <= PIPE_BUF)
    }

    fn copy_from(&mut self, src: &mut IoSrc, limit: usize) -> StarryResult<usize> {
        let (left, right) = self.buffer.vacant_slices_mut();
        let left_limit = left.len().min(limit);
        // `left` covers vacant ring storage and the following `read` initializes
        // exactly the returned prefix before the write index is advanced.
        let left = unsafe { left.assume_init_mut() };
        let mut copied = src.read(&mut left[..left_limit])?;
        if copied == left_limit && copied < limit {
            let right_limit = right.len().min(limit - copied);
            // The same vacant-storage contract applies to the wrapped slice.
            let right = unsafe { right.assume_init_mut() };
            copied += src.read(&mut right[..right_limit])?;
        }
        // Both reads initialized the first `copied` bytes across the two vacant
        // slices, and neither slice aliases occupied ring contents.
        unsafe { self.buffer.advance_write_index(copied) };
        Ok(copied)
    }

    fn merge_from(&mut self, src: &mut IoSrc, bytes: usize) -> StarryResult<usize> {
        debug_assert!(self.can_merge(bytes));
        let copied = self.copy_from(src, bytes)?;
        *self
            .buffers
            .back_mut()
            .expect("merge requires an existing pipe buffer") += copied;
        Ok(copied)
    }

    fn append_from(&mut self, src: &mut IoSrc) -> StarryResult<usize> {
        debug_assert!(self.has_free_buffer());
        let limit = src.remaining().min(PIPE_BUF);
        let copied = self.copy_from(src, limit)?;
        if copied > 0 {
            self.buffers.push_back(copied);
        }
        Ok(copied)
    }

    fn consume(&mut self, mut bytes: usize) {
        while bytes > 0 {
            let front = self
                .buffers
                .front_mut()
                .expect("pipe bytes require a pipe buffer");
            let consumed = bytes.min(*front);
            *front -= consumed;
            bytes -= consumed;
            if *front == 0 {
                self.buffers.pop_front();
            }
        }
    }

    fn poll_events(&self, read_side: bool) -> IoEvents {
        let mut events = IoEvents::empty();
        if read_side {
            events.set(
                IoEvents::IN | IoEvents::RDNORM,
                self.buffer.occupied_len() > 0,
            );
            events.set(IoEvents::HUP, self.writers == 0);
        } else {
            events.set(IoEvents::ERR, self.readers == 0);
            events.set(IoEvents::OUT | IoEvents::WRNORM, self.has_free_buffer());
        }
        events
    }
}

pub struct Pipe {
    read_side: bool,
    shared: Arc<Shared>,
    non_blocking: AtomicBool,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if self.read_side {
            let wake_writers = {
                let mut state = self.shared.state.lock();
                debug_assert!(state.readers > 0);
                state.readers = state.readers.saturating_sub(1);
                state.readers == 0
            };
            if wake_writers {
                // Reader count is published before waking blocked writers.
                unsafe { self.shared.poll_tx.wake(IoEvents::ERR | IoEvents::OUT) };
            }
            return;
        }

        let wake_readers = {
            let mut state = self.shared.state.lock();
            debug_assert!(state.writers > 0);
            state.writers = state.writers.saturating_sub(1);
            state.writers == 0
        };
        if wake_readers {
            // Writer count is published before waking blocked readers.
            unsafe { self.shared.poll_rx.wake(IoEvents::HUP | IoEvents::IN) };
        }
    }
}

impl Pipe {
    pub fn new() -> (Pipe, Pipe) {
        let shared = Arc::new(Shared {
            state: Mutex::new(PipeState::new(RING_BUFFER_INIT_SIZE)),
            poll_rx: PollSet::new(),
            poll_tx: PollSet::new(),
        });
        let read_end = Pipe {
            read_side: true,
            shared: shared.clone(),
            non_blocking: AtomicBool::new(false),
        };
        let write_end = Pipe {
            read_side: false,
            shared,
            non_blocking: AtomicBool::new(false),
        };
        (read_end, write_end)
    }

    /// Opens another file description for the same pipe endpoint.
    ///
    /// Unlike `dup`, reopening `/proc/self/fd/<n>` creates independent file
    /// status flags while retaining the same underlying pipe buffer. The
    /// endpoint count must therefore be incremented so closing either file
    /// description cannot prematurely report EOF or a broken pipe.
    pub(crate) fn reopen(&self, non_blocking: bool) -> Pipe {
        let mut state = self.shared.state.lock();
        if self.read_side {
            state.readers += 1;
        } else {
            state.writers += 1;
        }
        drop(state);

        Pipe {
            read_side: self.read_side,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(non_blocking),
        }
    }

    pub const fn is_read(&self) -> bool {
        self.read_side
    }

    pub const fn is_write(&self) -> bool {
        !self.read_side
    }

    pub fn capacity(&self) -> usize {
        self.shared.state.lock().buffer.capacity().get()
    }

    pub fn resize(&self, new_size: usize) -> StarryResult<()> {
        let new_size = rounded_pipe_size(new_size)?;

        let expanded = {
            let mut state = self.shared.state.lock();
            let old_size = state.buffer.capacity().get();
            if new_size == old_size {
                return Ok(());
            }
            if new_size / PIPE_BUF < state.buffers.len() {
                return Err(StarryError::ResourceBusy);
            }
            let old_buffer = mem::replace(
                &mut state.buffer,
                HeapRb::try_new(new_size).map_err(|_| StarryError::NoMemory)?,
            );
            let (left, right) = old_buffer.as_slices();
            let copied = state.buffer.push_slice(left) + state.buffer.push_slice(right);
            debug_assert_eq!(copied, left.len() + right.len());
            new_size > old_size
        };

        if expanded {
            // Newly freed capacity is visible before waking writers.
            unsafe { self.shared.poll_tx.wake(IoEvents::OUT) };
        }
        Ok(())
    }

    #[cfg(all(test, not(axtest)))]
    fn duplicate_read_end_for_test(&self) -> Pipe {
        assert!(self.is_read());
        self.shared.state.lock().readers += 1;
        Pipe {
            read_side: true,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(self.nonblocking()),
        }
    }

    fn write_with_broken_pipe_handler(
        &self,
        src: &mut IoSrc,
        on_broken_pipe: impl Fn(),
    ) -> StarryResult<usize> {
        if !self.is_write() {
            return Err(StarryError::BadFileDescriptor);
        }
        let size = src.remaining();
        if size == 0 {
            return Ok(0);
        }

        let mut total_written = 0;
        let mut merge_pending = true;
        let merge_bytes = size % PIPE_BUF;

        let result = block_on(poll_io(self, IoEvents::OUT, self.nonblocking(), || {
            enum WriteStep {
                Closed,
                WouldBlock,
                Wrote(usize),
            }

            let step = {
                let mut state = self.shared.state.lock();
                // Linux makes writes no larger than PIPE_BUF commit atomically;
                // nonblocking callers get EAGAIN until the whole record fits.
                if state.readers == 0 {
                    WriteStep::Closed
                } else {
                    let mut written = 0;
                    if merge_pending {
                        merge_pending = false;
                        if merge_bytes > 0 && state.can_merge(merge_bytes) {
                            written += state.merge_from(src, merge_bytes)?;
                        }
                    }
                    while src.remaining() > 0 && state.has_free_buffer() {
                        let appended = state.append_from(src)?;
                        written += appended;
                        if appended == 0 {
                            break;
                        }
                    }
                    if written == 0 {
                        WriteStep::WouldBlock
                    } else {
                        WriteStep::Wrote(written)
                    }
                }
            };

            let written = match step {
                WriteStep::Closed => {
                    if total_written > 0 {
                        return Ok(total_written);
                    }
                    on_broken_pipe();
                    return Err(StarryError::BrokenPipe);
                }
                WriteStep::WouldBlock => return Err(StarryError::WouldBlock),
                WriteStep::Wrote(written) => written,
            };

            if written > 0 {
                // Pipe bytes were committed before waking readers.
                unsafe { self.shared.poll_rx.wake(IoEvents::IN) };
                total_written += written;
                if total_written == size || self.nonblocking() {
                    return Ok(total_written);
                }
            }
            Err(StarryError::WouldBlock)
        }));

        // Linux returns committed bytes instead of EINTR once a pipe write
        // has made progress. This also prevents SA_RESTART from replaying the
        // whole userspace buffer after the prefix is already visible.
        match result {
            Err(StarryError::Interrupted | StarryError::TaskInterrupted(_))
                if total_written > 0 =>
            {
                Ok(total_written)
            }
            result => result,
        }
    }

    #[cfg(test)]
    fn write_without_sigpipe_for_test(&self, src: &mut IoSrc) -> StarryResult<usize> {
        // Axtests run in a kernel task without Starry process signal state. The
        // write transition is identical, but SIGPIPE delivery is outside this
        // direct pipe test and cannot be requested from that task.
        self.write_with_broken_pipe_handler(src, || {})
    }

    #[cfg(all(test, not(axtest)))]
    fn duplicate_write_end_for_test(&self) -> Pipe {
        assert!(self.is_write());
        self.shared.state.lock().writers += 1;
        Pipe {
            read_side: false,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(self.nonblocking()),
        }
    }
}

fn rounded_pipe_size(size: usize) -> StarryResult<usize> {
    let page_count = size.div_ceil(PAGE_SIZE_4K).max(1);
    let page_count = page_count
        .checked_next_power_of_two()
        .ok_or(StarryError::InvalidInput)?;
    let size = page_count
        .checked_mul(PAGE_SIZE_4K)
        .ok_or(StarryError::InvalidInput)?;
    if size > RING_BUFFER_MAX_SIZE {
        return Err(StarryError::OperationNotPermitted);
    }
    Ok(size)
}

#[cfg(all(test, not(axtest)))]
fn peer_close_with_multiple_readers_is_visible_for_test() -> bool {
    let (read_end, write_end) = Pipe::new();
    let second_reader = read_end.duplicate_read_end_for_test();

    drop(write_end);

    read_end.poll().contains(IoEvents::HUP) && second_reader.poll().contains(IoEvents::HUP)
}

#[cfg(all(test, not(axtest)))]
fn resize_rejects_oversized_pipe_for_test() -> bool {
    let (read_end, _write_end) = Pipe::new();
    read_end.resize(1024 * 1024 + 1).is_err()
}

#[cfg(all(test, not(axtest)))]
fn pipe_linux_io_semantics_hold_for_test() -> bool {
    let null_io_matches = {
        let (read_end, write_end) = Pipe::new();
        read_end.set_nonblocking(true).ok();
        write_end.set_nonblocking(true).ok();

        let mut empty_dst: &mut [u8] = &mut [];
        let null_read = read_end.read(&mut empty_dst as &mut dyn super::WriteBuf);
        drop(read_end);
        let mut empty_src: &[u8] = &[];
        let null_write =
            write_end.write_without_sigpipe_for_test(&mut empty_src as &mut dyn super::ReadBuf);

        matches!(null_read, Ok(0)) && matches!(null_write, Ok(0))
    };

    let atomic_write_and_poll_match = {
        let mut state = PipeState::new(PIPE_BUF);
        let initial = [b'a'; 4000];
        let mut initial_src: &[u8] = &initial;
        let initial_write = state.append_from(&mut initial_src as &mut dyn super::ReadBuf);
        let atomic_can_commit = state.can_merge(200) || state.has_free_buffer();

        matches!(initial_write, Ok(written) if written == initial.len())
            && !atomic_can_commit
            && state.buffer.occupied_len() == initial.len()
            && !state.poll_events(false).contains(IoEvents::OUT)
    };

    let closed_reader_poll_matches = {
        let mut state = PipeState::new(PIPE_BUF);
        state.readers = 0;
        let events = state.poll_events(false);
        events.contains(IoEvents::OUT | IoEvents::ERR)
    };

    let duplicates_preserve_nonblocking = {
        let (read_end, write_end) = Pipe::new();
        read_end.set_nonblocking(true).ok();
        write_end.set_nonblocking(true).ok();
        read_end.duplicate_read_end_for_test().nonblocking()
            && write_end.duplicate_write_end_for_test().nonblocking()
    };

    let page_slot_fragmentation_matches = {
        let mut state = PipeState::new(2 * PIPE_BUF);
        let initial = [b'a'; 5000];
        let mut initial_src: &[u8] = &initial;
        let first_write = state.append_from(&mut initial_src as &mut dyn super::ReadBuf);
        let second_write = state.append_from(&mut initial_src as &mut dyn super::ReadBuf);
        unsafe { state.buffer.advance_read_index(1000) };
        state.consume(1000);
        let shrink_is_busy = 1 < state.buffers.len();
        let atomic_can_commit = state.can_merge(4000) || state.has_free_buffer();

        matches!(first_write, Ok(PIPE_BUF))
            && matches!(second_write, Ok(written) if written == initial.len() - PIPE_BUF)
            && !state.poll_events(false).contains(IoEvents::OUT)
            && shrink_is_busy
            && !atomic_can_commit
    };

    null_io_matches
        && atomic_write_and_poll_match
        && closed_reader_poll_matches
        && duplicates_preserve_nonblocking
        && page_slot_fragmentation_matches
}

#[cfg(all(test, axtest))]
fn interrupted_pipe_write_preserves_partial_progress_for_test() -> bool {
    use ax_task::TaskState;

    let (read_end, write_end) = Pipe::new();
    if write_end.resize(PIPE_BUF).is_err() {
        return false;
    }

    let initial = [b'a'; PIPE_BUF];
    let mut initial_src: &[u8] = &initial;
    if !matches!(
        write_end.write_without_sigpipe_for_test(&mut initial_src),
        Ok(PIPE_BUF)
    ) {
        return false;
    }

    let write_end = Arc::new(write_end);
    let result = Arc::new(Mutex::new(None));
    let writer_task = {
        let write_end = Arc::clone(&write_end);
        let result = Arc::clone(&result);
        ax_task::spawn(move || {
            let bytes = [b'b'; 2 * PIPE_BUF];
            let mut src: &[u8] = &bytes;
            *result.lock() = Some(write_end.write_without_sigpipe_for_test(&mut src));
        })
    };

    if !wait_for_pipe_test_condition(|| writer_task.state() == TaskState::Blocked) {
        writer_task.interrupt();
        writer_task.join();
        return false;
    }

    let mut consumed = [0u8; PIPE_BUF];
    let mut dst: &mut [u8] = &mut consumed;
    if !matches!(read_end.read(&mut dst), Ok(PIPE_BUF)) {
        writer_task.interrupt();
        writer_task.join();
        return false;
    }

    let refilled_and_blocked = wait_for_pipe_test_condition(|| {
        read_end.shared.state.lock().buffer.occupied_len() == PIPE_BUF
            && writer_task.state() == TaskState::Blocked
    });
    writer_task.interrupt();
    writer_task.join();

    refilled_and_blocked && matches!(&*result.lock(), Some(Ok(written)) if *written == PIPE_BUF)
}

#[cfg(all(test, axtest))]
fn wait_for_pipe_test_condition(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..10_000 {
        if condition() {
            return true;
        }
        ax_task::yield_now();
    }
    false
}

fn raise_pipe() {
    let curr = current();
    send_signal_to_process(
        curr.as_thread().proc_data.proc.pid_number(),
        Some(SignalInfo::new_kernel(Signo::SIGPIPE)),
    )
    .expect("Failed to send SIGPIPE");
}

impl FileLike for Pipe {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if !self.is_read() {
            return Err(StarryError::BadFileDescriptor);
        }
        if dst.is_full() {
            return Ok(0);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let (read, writers) = {
                let mut state = self.shared.state.lock();
                let (left, right) = state.buffer.as_slices();
                let mut count = dst.write(left)?;
                if count >= left.len() {
                    count += dst.write(right)?;
                }
                unsafe { state.buffer.advance_read_index(count) };
                state.consume(count);
                (count, state.writers)
            };
            if read > 0 {
                // Pipe capacity was freed before waking writers.
                unsafe { self.shared.poll_tx.wake(IoEvents::OUT) };
                Ok(read)
            } else if writers == 0 {
                Ok(0)
            } else {
                Err(StarryError::WouldBlock)
            }
        }))
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.write_with_broken_pipe_handler(src, raise_pipe)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(Kstat {
            mode: S_IFIFO | if self.is_read() { 0o444 } else { 0o222 },
            ..Default::default()
        })
    }

    fn path(&self) -> Cow<'_, str> {
        format!("pipe:[{}]", self as *const _ as usize).into()
    }

    fn open_flags(&self) -> u32 {
        if self.is_read() { O_RDONLY } else { O_WRONLY }
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
        self.non_blocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> StarryResult<usize> {
        match cmd {
            FIONREAD => {
                (arg as *mut u32).vm_write(self.shared.state.lock().buffer.occupied_len() as u32)?;
                Ok(0)
            }
            _ => Err(StarryError::NotATty),
        }
    }
}

impl Pollable for Pipe {
    fn poll(&self) -> IoEvents {
        let state = self.shared.state.lock();
        // Linux reports POLLOUT when the pipe has a free PIPE_BUF-sized slot,
        // independently of whether the reader has already closed.
        state.poll_events(self.read_side)
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let read_ready = events.intersects(IoEvents::IN | IoEvents::RDNORM);
        let write_ready = events.intersects(IoEvents::OUT | IoEvents::WRNORM);
        let mut interests = if self.read_side {
            events & IoEvents::HUP
        } else {
            events & IoEvents::ERR
        };
        if self.read_side && read_ready {
            interests.insert(IoEvents::IN);
            interests.insert(IoEvents::HUP);
        }
        if !self.read_side && write_ready {
            interests.insert(IoEvents::OUT);
            interests.insert(IoEvents::ERR);
        }
        if interests.is_empty() {
            return;
        }
        if self.read_side {
            // Registration happens from file poll task context.
            unsafe { self.shared.poll_rx.register(context.waker(), interests) };
        } else {
            // Registration happens from file poll task context.
            unsafe { self.shared.poll_tx.register(context.waker(), interests) };
        }
    }
}

#[cfg(all(test, not(axtest)))]
fn pipe_resize_rounding_and_state_rules_hold_for_test() -> bool {
    let (read_end, _write_end) = Pipe::new();

    // Initial capacity is the default 64 KiB ring buffer.
    let initial_capacity = read_end.capacity();

    // Newly allocated pipe has one reader and one writer.
    read_end.is_read()
        && !read_end.is_write()
        // Resizing to the current capacity is a no-op success.
        && read_end.resize(initial_capacity).is_ok()
        && read_end.capacity() == initial_capacity
        // Round up to the next power-of-two page multiple: 4097 -> 8192.
        && read_end.resize(4097).is_ok()
        && read_end.capacity() == 8192
        // Sub-page sizes are rounded up to a single page (4096).
        && read_end.resize(1).is_ok()
        && read_end.capacity() == 4096
        // Sizes above RING_BUFFER_MAX_SIZE (1 MiB) are rejected.
        && read_end.resize(1024 * 1024 + 1).is_err()
        // Zero-sized resize rounds up to one page (no InvalidInput).
        && read_end.resize(0).is_ok()
        && read_end.capacity() == 4096
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, not(axtest)))]
    #[test]
    fn peer_close_with_multiple_readers_is_visible() {
        assert!(super::peer_close_with_multiple_readers_is_visible_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn resize_rejects_oversized_pipe() {
        assert!(super::resize_rejects_oversized_pipe_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn pipe_linux_io_semantics_hold() {
        assert!(super::pipe_linux_io_semantics_hold_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn interrupted_pipe_write_preserves_partial_progress() {
        assert!(super::interrupted_pipe_write_preserves_partial_progress_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn pipe_resize_rounding_and_state_rules_hold() {
        assert!(super::pipe_resize_rounding_and_state_rules_hold_for_test());
    }
}
