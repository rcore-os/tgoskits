use alloc::sync::Arc;
use core::{
    any::Any,
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use axerrno::{LinuxError, LinuxResult};
use axio::PollState;
use axsync::Mutex;
use axtask::future::try_block_on;
use event_listener::{Event, listener};
use linux_raw_sys::general::S_IFIFO;
use memory_addr::PAGE_SIZE_4K;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};

use super::{FileLike, Kstat};

const RING_BUFFER_INIT_SIZE: usize = 65536; // 64 KiB

struct Shared {
    buffer: Mutex<HeapRb<u8>>,
    // TODO: better poll
    read_avail: Event,
    write_avail: Event,
}

pub struct Pipe {
    read_side: bool,
    shared: Arc<Shared>,
    non_blocking: AtomicBool,
}
impl Drop for Pipe {
    fn drop(&mut self) {
        if self.read_side {
            self.shared.read_avail.notify(usize::MAX);
            self.shared.write_avail.notify(usize::MAX);
        }
    }
}

impl Pipe {
    pub fn new() -> (Pipe, Pipe) {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(HeapRb::new(RING_BUFFER_INIT_SIZE)),
            read_avail: Event::new(),
            write_avail: Event::new(),
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

impl FileLike for Pipe {
    fn read(&self, buf: &mut [u8]) -> LinuxResult<usize> {
        if !self.is_read() {
            return Err(LinuxError::EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        let non_blocking = self.nonblocking();
        loop {
            let read = self.shared.buffer.lock().pop_slice(buf);
            if read > 0 {
                self.shared.write_avail.notify(usize::MAX);
                return Ok(read);
            } else if self.closed() {
                return Ok(0);
            }

            if non_blocking {
                return Err(LinuxError::EAGAIN);
            }

            try_block_on(async {
                if self.closed() {
                    return Ok(());
                }
                listener!(self.shared.read_avail => listener);
                if self.closed() {
                    return Ok(());
                }
                listener.await;
                Ok(())
            })?;

            // For restart and listener wake up, continue reading
        }
    }

    fn write(&self, buf: &[u8]) -> LinuxResult<usize> {
        if !self.is_write() {
            return Err(LinuxError::EBADF);
        }
        if self.closed() {
            return Err(LinuxError::EPIPE);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        let mut total_written = 0;
        let non_blocking = self.nonblocking();
        loop {
            let written = self.shared.buffer.lock().push_slice(&buf[total_written..]);
            if written > 0 {
                self.shared.read_avail.notify(usize::MAX);
                total_written += written;
                if total_written == buf.len() || non_blocking {
                    break;
                }
            } else if self.closed() {
                break;
            }

            if non_blocking {
                return Err(LinuxError::EAGAIN);
            }

            try_block_on(async {
                if self.closed() {
                    return Ok(());
                }
                listener!(self.shared.write_avail => listener);
                if self.closed() {
                    return Ok(());
                }
                listener.await;
                Ok(())
            })?;
        }

        Ok(total_written)
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

    fn poll(&self) -> LinuxResult<PollState> {
        let buf = self.shared.buffer.lock();

        match self.read_side {
            true => {
                if buf.is_empty() && self.closed() {
                    return Err(LinuxError::EPIPE);
                }
                Ok(PollState {
                    readable: buf.occupied_len() > 0,
                    writable: false,
                })
            }
            false => Ok(PollState {
                readable: false,
                writable: buf.vacant_len() > 0,
            }),
        }
    }
}
