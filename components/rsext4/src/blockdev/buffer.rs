//! Fixed-size block buffer utilities.

use alloc::{boxed::Box, vec};

use crate::config::runtime_block_size;

/// Single-block scratch buffer used by the cached block device wrapper.
pub struct BlockBuffer {
    buffer: Box<[u8]>,
}

impl BlockBuffer {
    /// Creates a zero-initialized block buffer.
    pub fn new(block_size: usize) -> Self {
        Self {
            buffer: vec![0u8; block_size].into_boxed_slice(),
        }
    }

    /// Returns the buffer as an immutable byte slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    /// Returns the buffer as a mutable byte slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    /// Returns the number of bytes in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Returns whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Fills the buffer with zeros.
    pub fn clear(&mut self) {
        self.buffer.fill(0);
    }
}

impl Default for BlockBuffer {
    fn default() -> Self {
        Self::new(runtime_block_size())
    }
}
