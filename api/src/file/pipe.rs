use alloc::sync::Arc;
use core::{
    any::Any,
    mem,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use axerrno::{LinuxError, LinuxResult};
use axio::{IoEvents, PollSet, Pollable};
use axsync::Mutex;
use axtask::{current, future::Poller};
use linux_raw_sys::{general::S_IFIFO, ioctl::FIONREAD};
use memory_addr::PAGE_SIZE_4K;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use starry_core::task::{AsThread, send_signal_to_process};
use starry_signal::{SignalInfo, Signo};

use super::{FileLike, Kstat};
use crate::mm::UserPtr;

const RING_BUFFER_INIT_SIZE: usize = 65536; // 64 KiB

struct Shared {
    buffer: Mutex<HeapRb<u8>>,
    poll_rx: PollSet,
    poll_tx: PollSet,
    poll_close: PollSet,
}

pub struct Pipe {
    read_side: bool,
    shared: Arc<Shared>,
    non_blocking: AtomicBool,
}
impl Drop for Pipe {
    fn drop(&mut self) {
        self.shared.poll_close.wake();
    }
}

impl Pipe {
    pub fn new() -> (Pipe, Pipe) {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(HeapRb::new(RING_BUFFER_INIT_SIZE)),
            poll_rx: PollSet::new(),
            poll_tx: PollSet::new(),
            poll_close: PollSet::new(),
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

    pub fn closed(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }

    pub fn capacity(&self) -> usize {
        self.shared.buffer.lock().capacity().get()
    }

    pub fn resize(&self, new_size: usize) -> LinuxResult<()> {
        let new_size = new_size.div_ceil(PAGE_SIZE_4K).max(1) * PAGE_SIZE_4K;

        let mut buffer = self.shared.buffer.lock();
        if new_size == buffer.capacity().get() {
            return Ok(());
        }
        if new_size < buffer.occupied_len() {
            return Err(LinuxError::EBUSY);
        }
        let old_buffer = mem::replace(&mut *buffer, HeapRb::new(new_size));
        let (left, right) = old_buffer.as_slices();
        buffer.push_slice(left);
        buffer.push_slice(right);
        Ok(())
    }
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
    fn read(&self, buf: &mut [u8]) -> LinuxResult<usize> {
        if !self.is_read() {
            return Err(LinuxError::EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        Poller::new(self, IoEvents::IN)
            .non_blocking(self.nonblocking())
            .poll(|| {
                let read = self.shared.buffer.lock().pop_slice(buf);
                if read > 0 {
                    self.shared.poll_tx.wake();
                    Ok(read)
                } else if self.closed() {
                    Ok(0)
                } else {
                    Err(LinuxError::EAGAIN)
                }
            })
    }

    fn write(&self, buf: &[u8]) -> LinuxResult<usize> {
        if !self.is_write() {
            return Err(LinuxError::EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        let mut total_written = 0;
        let non_blocking = self.nonblocking();
        Poller::new(self, IoEvents::OUT)
            .non_blocking(non_blocking)
            .poll(|| {
                if self.closed() {
                    raise_pipe();
                    return Err(LinuxError::EPIPE);
                }

                let written = self.shared.buffer.lock().push_slice(&buf[total_written..]);
                if written > 0 {
                    self.shared.poll_rx.wake();
                    total_written += written;
                    if total_written == buf.len() || non_blocking {
                        return Ok(total_written);
                    }
                }
                Err(LinuxError::EAGAIN)
            })
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(Kstat {
            mode: S_IFIFO | if self.is_read() { 0o444 } else { 0o222 },
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn set_nonblocking(&self, nonblocking: bool) -> LinuxResult {
        self.non_blocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> LinuxResult<usize> {
        match cmd {
            FIONREAD => {
                *UserPtr::<u32>::from(arg).get_as_mut()? =
                    self.shared.buffer.lock().occupied_len() as _;
                Ok(0)
            }
            _ => Err(LinuxError::ENOTTY),
        }
    }
}

impl Pollable for Pipe {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        let buf = self.shared.buffer.lock();
        if self.read_side {
            events.set(IoEvents::IN, buf.occupied_len() > 0);
            events.set(IoEvents::HUP, self.closed());
        } else {
            events.set(IoEvents::OUT, buf.vacant_len() > 0);
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.shared.poll_rx.register(context.waker());
        }
        if events.contains(IoEvents::OUT) {
            self.shared.poll_tx.register(context.waker());
        }
        self.shared.poll_close.register(context.waker());
    }
}
