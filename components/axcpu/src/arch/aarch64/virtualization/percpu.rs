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

//! Exclusive ownership of the current CPU's EL2 enable and vector state.

use core::marker::PhantomData;

use aarch64_cpu::registers::{HCR_EL2, Readable, VBAR_EL2, Writeable};

use crate::{VirtAddr, virtualization::VirtualizationError};

#[derive(Debug)]
struct HostState {
    vector: u64,
    control: u64,
}

/// EL2 state owned by one pinned host CPU.
///
/// The owner must disable virtualization on that CPU before retiring this value.
/// IRQ routing, vector memory, and CPU-local storage remain caller-owned.
#[derive(Debug, Default)]
pub struct PerCpu {
    host: Option<HostState>,
    _not_send_sync: PhantomData<*mut ()>,
}

impl PerCpu {
    /// Constructs an inactive owner without reading CPU registers.
    pub const fn new() -> Self {
        Self {
            host: None,
            _not_send_sync: PhantomData,
        }
    }

    /// Reports ownership by this object, independent of the previous host's VM bit.
    pub const fn is_enabled(&self) -> bool {
        self.host.is_some()
    }

    /// Installs a guest-capable EL2 vector and enables AArch64 stage-2 translation.
    ///
    /// # Safety
    /// The caller exclusively owns EL2 on a pinned CPU with IRQs masked and no
    /// live guest. `vector` must reference a complete executable 2 KiB vector
    /// table whose handlers and backing memory remain valid until `disable`.
    /// The caller must disable on this CPU before releasing hardware ownership.
    pub unsafe fn enable(&mut self, vector: VirtAddr) -> Result<(), VirtualizationError> {
        if self.host.is_some() {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        if crate::registers::current_exception_level() != 2 {
            return Err(VirtualizationError::Unavailable);
        }
        if !vector.as_usize().is_multiple_of(2048) {
            return Err(VirtualizationError::InvalidVector);
        }
        let host = HostState {
            vector: VBAR_EL2.get(),
            control: HCR_EL2.get(),
        };
        VBAR_EL2.set(vector.as_usize() as u64);
        // SAFETY: caller owns the IRQ-masked EL2 transition. Publish the vector
        // before enabling virtualization and synchronize before any guest entry.
        unsafe {
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
        HCR_EL2.set(host.control | (1 << 0) | (1 << 31) | (1 << 19));
        unsafe {
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
        self.host = Some(host);
        Ok(())
    }

    /// Restores the exact host control and vector values saved by `enable`.
    ///
    /// # Safety
    /// The caller owns the original pinned CPU with IRQs masked and has unloaded
    /// every guest. No entry may overlap restoration or use the retired vector.
    pub unsafe fn disable(&mut self) -> Result<(), VirtualizationError> {
        let host = self.host.take().ok_or(VirtualizationError::NotEnabled)?;
        HCR_EL2.set(host.control);
        VBAR_EL2.set(host.vector);
        // SAFETY: the original live vector and host controls were saved together;
        // synchronization completes before IRQ delivery or CPU ownership resumes.
        unsafe {
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
        Ok(())
    }
}
