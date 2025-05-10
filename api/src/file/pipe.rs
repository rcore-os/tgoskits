use core::any::Any;

use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axio::PollState;
use axsync::Mutex;
use linux_raw_sys::general::S_IFIFO;

use super::{FileLike, Kstat};

#[derive(Copy, Clone, PartialEq)]
enum RingBufferStatus {
    Full,
    Empty,
    Normal,
}

const RING_BUFFER_SIZE: usize = 256;

struct PipeRingBuffer {
    arr: [u8; RING_BUFFER_SIZE],
    head: usize,
    tail: usize,
    status: RingBufferStatus,
}

impl PipeRingBuffer {
    const fn new() -> Self {
        Self {
            arr: [0; RING_BUFFER_SIZE],
            head: 0,
            tail: 0,
            status: RingBufferStatus::Empty,
        }
    }

    fn write_byte(&mut self, byte: u8) {
        self.status = RingBufferStatus::Normal;
        self.arr[self.tail] = byte;
        self.tail = (self.tail + 1) % RING_BUFFER_SIZE;
        if self.tail == self.head {
            self.status = RingBufferStatus::Full;
        }
    }

    fn read_byte(&mut self) -> u8 {
        self.status = RingBufferStatus::Normal;
        let c = self.arr[self.head];
        self.head = (self.head + 1) % RING_BUFFER_SIZE;
        if self.head == self.tail {
            self.status = RingBufferStatus::Empty;
        }
        c
    }

    /// Get the length of remaining data in the buffer
    const fn available_read(&self) -> usize {
        if matches!(self.status, RingBufferStatus::Empty) {
            0
        } else if self.tail > self.head {
            self.tail - self.head
        } else {
            self.tail + RING_BUFFER_SIZE - self.head
        }
    }

    /// Get the length of remaining space in the buffer
    const fn available_write(&self) -> usize {
        if matches!(self.status, RingBufferStatus::Full) {
            0
        } else {
            RING_BUFFER_SIZE - self.available_read()
        }
    }
}

pub struct Pipe {
    readable: bool,
    buffer: Arc<Mutex<PipeRingBuffer>>,
}

impl Pipe {
    pub fn new() -> (Pipe, Pipe) {
        let buffer = Arc::new(Mutex::new(PipeRingBuffer::new()));
        let read_end = Pipe {
            readable: true,
            buffer: buffer.clone(),
        };
        let write_end = Pipe {
            readable: false,
            buffer,
        };
        (read_end, write_end)
    }

    pub const fn readable(&self) -> bool {
        self.readable
    }

    pub const fn writable(&self) -> bool {
        !self.readable
    }

    pub fn closed(&self) -> bool {
        Arc::strong_count(&self.buffer) == 1
    }
}

impl FileLike for Pipe {
    fn read(&self, buf: &mut [u8]) -> LinuxResult<usize> {
        if !self.readable() {
            return Err(LinuxError::EPERM);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            let mut ring_buffer = self.buffer.lock();
            let read_size = ring_buffer.available_read().min(buf.len());
            if read_size == 0 {
                if self.closed() {
                    return Ok(0);
                }
                drop(ring_buffer);
                // Data not ready, wait for write end
                axtask::yield_now(); // TODO: use synconize primitive
                continue;
            }
            for c in buf.iter_mut().take(read_size) {
                *c = ring_buffer.read_byte();
            }
            return Ok(read_size);
        }
    }

    fn write(&self, buf: &[u8]) -> LinuxResult<usize> {
        if !self.writable() {
            return Err(LinuxError::EPERM);
        }
        if self.closed() {
            return Err(LinuxError::EPIPE);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        let mut write_size = 0usize;
        let total_len = buf.len();
        loop {
            let mut ring_buffer = self.buffer.lock();
            let loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                if self.closed() {
                    return Ok(write_size);
                }
                drop(ring_buffer);
                // Buffer is full, wait for read end to consume
                axtask::yield_now(); // TODO: use synconize primitive
                continue;
            }
            for _ in 0..loop_write {
                if write_size == total_len {
                    return Ok(write_size);
                }
                ring_buffer.write_byte(buf[write_size]);
                write_size += 1;
            }
        }
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(Kstat {
            mode: S_IFIFO | 0o600u32, // rw-------
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn poll(&self) -> LinuxResult<PollState> {
        let buf = self.buffer.lock();

        match self.readable {
            true => {
                if buf.available_read() == 0 && self.closed() {
                    return Err(LinuxError::EPIPE);
                }
                Ok(PollState {
                    readable: buf.available_read() > 0,
                    writable: false,
                })
            }
            false => Ok(PollState {
                readable: false,
                writable: buf.available_write() > 0,
            }),
        }
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> LinuxResult {
        Ok(())
    }
}
