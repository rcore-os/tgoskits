//! This module provides read and write buffers for interacting with the VM's
//! memory.

use core::mem;

use axio::{Buf, BufMut, Read, Write};

use crate::{vm_read_slice, vm_write_slice};

/// A read-only buffer in the VM's memory.
///
/// It implements the `axio::Read` trait, allowing it to be used with other I/O
/// operations.
pub struct VmBytes {
    /// The pointer to the start of the buffer in the VM's memory.
    pub ptr: *const u8,
    /// The length of the buffer.
    pub len: usize,
}

impl VmBytes {
    /// Creates a new `VmBytes` from a raw pointer and a length.
    pub fn new(ptr: *const u8, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Casts the `VmBytes` to a mutable `VmBytesMut`.
    pub fn cast_mut(&self) -> VmBytesMut {
        VmBytesMut::new(self.ptr as *mut u8, self.len)
    }
}

impl Read for VmBytes {
    /// Reads bytes from the VM's memory into the provided buffer.
    fn read(&mut self, buf: &mut [u8]) -> axio::Result<usize> {
        let len = self.len.min(buf.len());
        vm_read_slice(self.ptr, unsafe { mem::transmute(&mut buf[..len]) })?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }
}

impl Buf for VmBytes {
    fn remaining(&self) -> usize {
        self.len
    }
}

/// A mutable buffer in the VM's memory.
///
/// It implements the `axio::Write` trait, allowing it to be used with other I/O
/// operations.
pub struct VmBytesMut {
    /// The pointer to the start of the buffer in the VM's memory.
    pub ptr: *mut u8,
    /// The length of the buffer.
    pub len: usize,
}

impl VmBytesMut {
    /// Creates a new `VmBytesMut` from a raw pointer and a length.
    pub fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Casts the `VmBytesMut` to a read-only `VmBytes`.
    pub fn cast_const(&self) -> VmBytes {
        VmBytes::new(self.ptr, self.len)
    }
}

impl Write for VmBytesMut {
    /// Writes bytes from the provided buffer into the VM's memory.
    fn write(&mut self, buf: &[u8]) -> axio::Result<usize> {
        let len = self.len.min(buf.len());
        vm_write_slice(self.ptr, &buf[..len])?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }

    /// Flushes the buffer. This is a no-op for `VmBytesMut`.
    fn flush(&mut self) -> axio::Result {
        Ok(())
    }
}

impl BufMut for VmBytesMut {
    fn remaining_mut(&self) -> usize {
        self.len
    }
}
