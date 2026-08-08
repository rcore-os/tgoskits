//! Fixed-capacity RT output ring buffer.

use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

const RT_OUTPUT_CAPACITY: usize = 1024;

static RT_OUTPUT: RtOutputBuffer = RtOutputBuffer::new();

struct RtOutputBuffer {
    write: AtomicUsize,
    read: AtomicUsize,
    dropped: AtomicU64,
    bytes: [AtomicU8; RT_OUTPUT_CAPACITY],
}

impl RtOutputBuffer {
    const fn new() -> Self {
        Self {
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            bytes: [const { AtomicU8::new(0) }; RT_OUTPUT_CAPACITY],
        }
    }

    fn push_bytes(&self, bytes: &[u8]) {
        for &byte in bytes {
            self.push_byte(byte);
        }
    }

    fn push_byte(&self, byte: u8) {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= RT_OUTPUT_CAPACITY {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.bytes[write % RT_OUTPUT_CAPACITY].store(byte, Ordering::Release);
        self.write.store(write.wrapping_add(1), Ordering::Release);
    }

    fn pop_byte(&self) -> Option<u8> {
        let read = self.read.load(Ordering::Acquire);
        let write = self.write.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let byte = self.bytes[read % RT_OUTPUT_CAPACITY].load(Ordering::Acquire);
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(byte)
    }
}

/// Appends bytes to the RT output ring buffer.
pub fn rt_output_write(bytes: &[u8]) {
    RT_OUTPUT.push_bytes(bytes);
}

/// Appends an unsigned decimal value to the RT output ring buffer.
pub fn rt_output_write_decimal(mut value: u64) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    if value == 0 {
        rt_output_write(b"0");
        return;
    }
    while value != 0 {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    rt_output_write(&buffer[index..]);
}

/// Copies pending RT output into `out` and returns the copied length.
pub fn rt_read_output(out: &mut [u8]) -> usize {
    let mut copied = 0;
    while copied < out.len() {
        let Some(byte) = RT_OUTPUT.pop_byte() else {
            break;
        };
        out[copied] = byte;
        copied += 1;
    }
    copied
}
