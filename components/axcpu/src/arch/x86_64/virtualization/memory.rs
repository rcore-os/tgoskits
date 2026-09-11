//! Checked control-memory leases; no allocation policy.

use core::ptr::NonNull;

use crate::{
    PhysAddr,
    virtualization::{ControlMemory, VirtualizationError},
};

pub(super) struct ControlRegion<M: ControlMemory> {
    memory: Option<M>,
    physical: PhysAddr,
    pointer: NonNull<u8>,
    size: usize,
}

impl<M: ControlMemory> ControlRegion<M> {
    pub(super) fn new(
        memory: M,
        minimum_size: usize,
        alignment: usize,
    ) -> Result<Self, VirtualizationError> {
        let physical = memory.physical_address();
        let pointer = memory.virtual_address();
        let size = memory.byte_len();
        if !alignment.is_power_of_two()
            || size == 0
            || size < minimum_size
            || physical.as_usize() & (alignment - 1) != 0
            || pointer.as_ptr() as usize & (alignment - 1) != 0
            || physical.as_usize().checked_add(size - 1).is_none()
            || (pointer.as_ptr() as usize).checked_add(size - 1).is_none()
        {
            return Err(VirtualizationError::InvalidControlMemory);
        }
        Ok(Self {
            memory: Some(memory),
            physical,
            pointer,
            size,
        })
    }

    pub(super) fn physical_address(&self) -> PhysAddr {
        self.physical
    }

    pub(super) fn pointer(&self) -> NonNull<u8> {
        self.pointer
    }

    pub(super) fn clear(&mut self) {
        self.fill(0);
    }

    pub(super) fn fill(&mut self, value: u8) {
        // SAFETY: the owning lease covers this exclusively borrowed region.
        unsafe { core::ptr::write_bytes(self.pointer.as_ptr(), value, self.size) };
    }

    pub(super) fn set_bit(&mut self, bit: usize, enabled: bool) {
        let byte = bit / 8;
        assert!(byte < self.size, "validated control bitmap bit");
        // SAFETY: the checked byte belongs to the exclusive initialized lease.
        // No guest is executing while its owner mutates interception policy.
        unsafe {
            let pointer = self.pointer.as_ptr().add(byte);
            let old = pointer.read_volatile();
            let mask = 1u8 << (bit % 8);
            pointer.write_volatile(if enabled { old | mask } else { old & !mask });
        }
    }

    pub(super) fn write_revision(&mut self, revision: u32) {
        // SAFETY: VMX callers validate a page-aligned region of at least 4 KiB;
        // hardware has not yet taken ownership of the control page.
        unsafe { self.pointer.cast::<u32>().as_ptr().write(revision) };
    }

    pub(super) fn into_memory(mut self) -> M {
        self.memory
            .take()
            .expect("control lease is present until retirement")
    }

    pub(super) fn retain_on_failed_retirement(&mut self) {
        // ControlMemory's contract guarantees that forgetting its owning lease
        // retains the allocation and mapping even after the CPU object is dropped.
        if let Some(memory) = self.memory.take() {
            core::mem::forget(memory);
        }
    }
}
