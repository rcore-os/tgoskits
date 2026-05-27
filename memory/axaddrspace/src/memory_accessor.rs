// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Unified guest memory access interface
//!
//! This module provides a safe and consistent way to access guest memory
//! from VirtIO device implementations, handling address translation and
//! memory safety concerns.
use ax_errno::{AxError, AxResult};
use ax_memory_addr::PhysAddr;

use crate::GuestPhysAddr;

/// A stateful accessor to the memory space of a guest
pub trait GuestMemoryAccessor {
    /// Translate a guest physical address to host physical address and get access limit
    ///
    /// Returns a tuple of (host_physical_address, accessible_size) if the translation
    /// is successful. The accessible_size indicates how many bytes can be safely
    /// accessed starting from the given guest address.
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)>;

    /// Read a value of type V from guest memory
    ///
    /// # Returns
    ///
    /// Returns `Err(AxError::InvalidInput)` in the following cases:
    /// - The guest address cannot be translated to a valid host address
    /// - The accessible memory region starting from the guest address is smaller
    ///   than the size of type V (insufficient space for the read operation)
    ///
    /// # Safety
    ///
    /// This function uses volatile memory access to ensure the read operation
    /// is not optimized away by the compiler, which is important for device
    /// register access and shared memory scenarios.
    fn read_obj<V: Copy>(&self, guest_addr: GuestPhysAddr) -> AxResult<V> {
        let (host_addr, limit) = self
            .translate_and_get_limit(guest_addr)
            .ok_or(AxError::InvalidInput)?;

        // Check if we have enough space to read the object
        if limit < core::mem::size_of::<V>() {
            return Err(AxError::InvalidInput);
        }

        unsafe {
            let ptr = host_addr.as_usize() as *const V;
            Ok(core::ptr::read_volatile(ptr))
        }
    }

    /// Write a value of type V to guest memory
    ///
    /// # Returns
    ///
    /// Returns `Err(AxError::InvalidInput)` in the following cases:
    /// - The guest address cannot be translated to a valid host address
    /// - The accessible memory region starting from the guest address is smaller
    ///   than the size of type V (insufficient space for the write operation)
    ///
    /// # Safety
    ///
    /// This function uses volatile memory access to ensure the write operation
    /// is not optimized away by the compiler, which is important for device
    /// register access and shared memory scenarios.
    fn write_obj<V: Copy>(&self, guest_addr: GuestPhysAddr, val: V) -> AxResult<()> {
        let (host_addr, limit) = self
            .translate_and_get_limit(guest_addr)
            .ok_or(AxError::InvalidInput)?;

        // Check if we have enough space to write the object
        if limit < core::mem::size_of::<V>() {
            return Err(AxError::InvalidInput);
        }

        unsafe {
            let ptr = host_addr.as_usize() as *mut V;
            core::ptr::write_volatile(ptr, val);
        }
        Ok(())
    }

    /// Read a buffer from guest memory
    fn read_buffer(&self, guest_addr: GuestPhysAddr, buffer: &mut [u8]) -> AxResult<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        let (host_addr, accessible_size) = self
            .translate_and_get_limit(guest_addr)
            .ok_or(AxError::InvalidInput)?;

        // Check if we can read the entire buffer from this accessible region
        if accessible_size >= buffer.len() {
            // Simple case: entire buffer fits within accessible region
            unsafe {
                let src_ptr = host_addr.as_usize() as *const u8;
                core::ptr::copy_nonoverlapping(src_ptr, buffer.as_mut_ptr(), buffer.len());
            }
            return Ok(());
        }

        // Complex case: buffer spans multiple regions, handle region by region
        let mut current_guest_addr = guest_addr;
        let mut remaining_buffer = buffer;

        while !remaining_buffer.is_empty() {
            let (current_host_addr, current_accessible_size) = self
                .translate_and_get_limit(current_guest_addr)
                .ok_or(AxError::InvalidInput)?;

            let bytes_to_read = remaining_buffer.len().min(current_accessible_size);

            // Read from current accessible region
            unsafe {
                let src_ptr = current_host_addr.as_usize() as *const u8;
                core::ptr::copy_nonoverlapping(
                    src_ptr,
                    remaining_buffer.as_mut_ptr(),
                    bytes_to_read,
                );
            }

            // Move to next region
            current_guest_addr =
                GuestPhysAddr::from_usize(current_guest_addr.as_usize() + bytes_to_read);
            remaining_buffer = &mut remaining_buffer[bytes_to_read..];
        }

        Ok(())
    }

    /// Write a buffer to guest memory
    fn write_buffer(&self, guest_addr: GuestPhysAddr, buffer: &[u8]) -> AxResult<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        let (host_addr, accessible_size) = self
            .translate_and_get_limit(guest_addr)
            .ok_or(AxError::InvalidInput)?;

        // Check if we can write the entire buffer to this accessible region
        if accessible_size >= buffer.len() {
            // Simple case: entire buffer fits within accessible region
            unsafe {
                let dst_ptr = host_addr.as_usize() as *mut u8;
                core::ptr::copy_nonoverlapping(buffer.as_ptr(), dst_ptr, buffer.len());
            }
            return Ok(());
        }

        // Complex case: buffer spans multiple regions, handle region by region
        let mut current_guest_addr = guest_addr;
        let mut remaining_buffer = buffer;

        while !remaining_buffer.is_empty() {
            let (current_host_addr, current_accessible_size) = self
                .translate_and_get_limit(current_guest_addr)
                .ok_or(AxError::InvalidInput)?;

            let bytes_to_write = remaining_buffer.len().min(current_accessible_size);

            // Write to current accessible region
            unsafe {
                let dst_ptr = current_host_addr.as_usize() as *mut u8;
                core::ptr::copy_nonoverlapping(remaining_buffer.as_ptr(), dst_ptr, bytes_to_write);
            }

            // Move to next region
            current_guest_addr =
                GuestPhysAddr::from_usize(current_guest_addr.as_usize() + bytes_to_write);
            remaining_buffer = &remaining_buffer[bytes_to_write..];
        }

        Ok(())
    }

    /// Read a volatile value from guest memory (for device registers)
    fn read_volatile<V: Copy>(&self, guest_addr: GuestPhysAddr) -> AxResult<V> {
        self.read_obj(guest_addr)
    }

    /// Write a volatile value to guest memory (for device registers)
    fn write_volatile<V: Copy>(&self, guest_addr: GuestPhysAddr, val: V) -> AxResult<()> {
        self.write_obj(guest_addr, val)
    }
}
