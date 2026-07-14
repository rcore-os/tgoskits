use alloc::{borrow::Cow, format, sync::Arc};
use core::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::PAGE_SIZE_4K;
use ax_sync::PiMutex;
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
    file::{IoDst, IoSrc},
    task::{
        current,
        future::{block_on, poll_io},
        send_signal_to_process,
    },
};

const RING_BUFFER_INIT_SIZE: usize = 65536; // 64 KiB
const RING_BUFFER_MAX_SIZE: usize = 1024 * 1024; // 1 MiB

struct Shared {
    state: PiMutex<PipeState>,
    poll_rx: PollSet,
    poll_tx: PollSet,
}

struct PipeState {
    buffer: HeapRb<u8>,
    readers: usize,
    writers: usize,
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
            state: PiMutex::new(PipeState {
                buffer: HeapRb::new(RING_BUFFER_INIT_SIZE),
                readers: 1,
                writers: 1,
            }),
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

    pub const fn is_read(&self) -> bool {
        self.read_side
    }

    pub const fn is_write(&self) -> bool {
        !self.read_side
    }

    pub fn capacity(&self) -> usize {
        self.shared.state.lock().buffer.capacity().get()
    }

    pub fn resize(&self, new_size: usize) -> AxResult<()> {
        let new_size = rounded_pipe_size(new_size)?;

        let expanded = {
            let mut state = self.shared.state.lock();
            let old_size = state.buffer.capacity().get();
            if new_size == old_size {
                return Ok(());
            }
            if new_size < state.buffer.occupied_len() {
                return Err(AxError::ResourceBusy);
            }
            let old_buffer = mem::replace(
                &mut state.buffer,
                HeapRb::try_new(new_size).map_err(|_| AxError::NoMemory)?,
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

    #[cfg(axtest)]
    fn duplicate_read_end_for_test(&self) -> Pipe {
        assert!(self.is_read());
        self.shared.state.lock().readers += 1;
        Pipe {
            read_side: true,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(false),
        }
    }
}

fn rounded_pipe_size(size: usize) -> AxResult<usize> {
    let page_count = size.div_ceil(PAGE_SIZE_4K).max(1);
    let page_count = page_count
        .checked_next_power_of_two()
        .ok_or(AxError::InvalidInput)?;
    let size = page_count
        .checked_mul(PAGE_SIZE_4K)
        .ok_or(AxError::InvalidInput)?;
    if size > RING_BUFFER_MAX_SIZE {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(size)
}

#[cfg(axtest)]
pub(crate) fn peer_close_with_multiple_readers_is_visible_for_test() -> bool {
    let (read_end, write_end) = Pipe::new();
    let second_reader = read_end.duplicate_read_end_for_test();

    drop(write_end);

    read_end.poll().contains(IoEvents::HUP) && second_reader.poll().contains(IoEvents::HUP)
}

#[cfg(axtest)]
pub(crate) fn resize_rejects_oversized_pipe_for_test() -> bool {
    let (read_end, _write_end) = Pipe::new();
    read_end.resize(1024 * 1024 + 1).is_err()
}

fn raise_pipe() {
    let curr = current();
    send_signal_to_process(
        curr.as_thread().proc_data.proc.pid(),
        Some(SignalInfo::new_kernel(Signo::SIGPIPE)),
    )
    .expect("Failed to send SIGPIPE");
}

impl FileLike for Pipe {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        if !self.is_read() {
            return Err(AxError::BadFileDescriptor);
        }
        if dst.is_full() {
            return Ok(0);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let (read, writers) = {
                let state = self.shared.state.lock();
                let (left, right) = state.buffer.as_slices();
                let mut count = dst.write(left)?;
                if count >= left.len() {
                    count += dst.write(right)?;
                }
                unsafe { state.buffer.advance_read_index(count) };
                (count, state.writers)
            };
            if read > 0 {
                // Pipe capacity was freed before waking writers.
                unsafe { self.shared.poll_tx.wake(IoEvents::OUT) };
                Ok(read)
            } else if writers == 0 {
                Ok(0)
            } else {
                Err(AxError::WouldBlock)
            }
        }))
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        if !self.is_write() {
            return Err(AxError::BadFileDescriptor);
        }
        let size = src.remaining();
        if size == 0 {
            return Ok(0);
        }

        let mut total_written = 0;

        block_on(poll_io(self, IoEvents::OUT, self.nonblocking(), || {
            enum WriteStep {
                Closed,
                Wrote(usize),
            }

            let step = {
                let mut state = self.shared.state.lock();
                if state.readers == 0 {
                    WriteStep::Closed
                } else {
                    let (left, right) = state.buffer.vacant_slices_mut();
                    let mut count = src.read(unsafe { left.assume_init_mut() })?;
                    if count >= left.len() {
                        count += src.read(unsafe { right.assume_init_mut() })?;
                    }
                    unsafe { state.buffer.advance_write_index(count) };
                    WriteStep::Wrote(count)
                }
            };

            let WriteStep::Wrote(written) = step else {
                if total_written > 0 {
                    return Ok(total_written);
                }
                raise_pipe();
                return Err(AxError::BrokenPipe);
            };

            if written > 0 {
                // Pipe bytes were committed before waking readers.
                unsafe { self.shared.poll_rx.wake(IoEvents::IN) };
                total_written += written;
                if total_written == size || self.nonblocking() {
                    return Ok(total_written);
                }
            }
            Err(AxError::WouldBlock)
        }))
    }

    fn stat(&self) -> AxResult<Kstat> {
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

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult {
        self.non_blocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            FIONREAD => {
                (arg as *mut u32).vm_write(self.shared.state.lock().buffer.occupied_len() as u32)?;
                Ok(0)
            }
            _ => Err(AxError::NotATty),
        }
    }
}

impl Pollable for Pipe {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        let state = self.shared.state.lock();
        if self.read_side {
            events.set(IoEvents::IN, state.buffer.occupied_len() > 0);
            events.set(IoEvents::HUP, state.writers == 0);
        } else {
            events.set(IoEvents::ERR, state.readers == 0);
            events.set(
                IoEvents::OUT,
                state.readers > 0 && state.buffer.vacant_len() > 0,
            );
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let mut interests = if self.read_side {
            events & (IoEvents::IN | IoEvents::HUP)
        } else {
            events & (IoEvents::OUT | IoEvents::ERR)
        };
        if self.read_side && events.contains(IoEvents::IN) {
            interests.insert(IoEvents::HUP);
        }
        if !self.read_side && events.contains(IoEvents::OUT) {
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
