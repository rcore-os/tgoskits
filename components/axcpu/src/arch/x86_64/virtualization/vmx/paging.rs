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

//! EPT root geometry and current-CPU capability validation.

use crate::{
    PhysAddr,
    virtualization::{Backend, VirtualizationError},
};

/// A four-level, write-back EPT pointer validated against a VMX CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EptPointer(u64);

impl EptPointer {
    /// Encodes a four-level root using this CPU's EPT capability registers.
    ///
    /// # Safety
    /// Execute at ring 0 with readable VMX capability MSRs. The page-table owner
    /// separately retains all mapped table memory before installing this value.
    pub unsafe fn for_current_cpu(
        root: PhysAddr,
        accessed_dirty: bool,
    ) -> Result<Self, VirtualizationError> {
        // SAFETY: the caller grants privileged access to this CPU's VMX MSRs.
        unsafe { EptCapabilities::current() }?.pointer(root, accessed_dirty)
    }

    /// Returns the complete validated EPTP operand.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Invalidates this root on the current CPU using a supported INVEPT scope.
    ///
    /// # Safety
    /// Retain an enabled VMX CPU and pin for the full operation. The EPTP must
    /// remain valid on this CPU. Remote invalidation and table lifetime belong
    /// to the caller's VM ownership protocol.
    pub unsafe fn invalidate(self) -> Result<(), VirtualizationError> {
        // SAFETY: the caller owns an enabled privileged VMX CPU.
        let capability = unsafe { x86::msr::rdmsr(0x48c) };
        let scope = invalidation_scope(capability)?;
        // SAFETY: the selected scope is implemented and the caller retains VMX ownership.
        unsafe { super::invalidate_ept(scope, self.0) }
    }
}

/// Immutable hardware facts retained only through a pinned VMCS binding.
#[derive(Clone, Copy)]
pub(crate) struct EptCapabilities {
    capability: u64,
    address_bits: u32,
}

impl EptCapabilities {
    /// Captures immutable hardware facts for one binding.
    ///
    /// # Safety
    /// The caller runs at ring 0 and pins this CPU throughout use of the result.
    pub(crate) unsafe fn current() -> Result<Self, VirtualizationError> {
        if Backend::detect() != Some(Backend::Vmx) {
            return Err(VirtualizationError::Unavailable);
        }
        // SAFETY: CPUID established VMX support on this privileged CPU.
        let capability = unsafe { x86::msr::rdmsr(0x48c) };
        Ok(Self {
            capability,
            address_bits: super::super::percpu::physical_address_bits().min(52),
        })
    }

    fn pointer(
        self,
        root: PhysAddr,
        accessed_dirty: bool,
    ) -> Result<EptPointer, VirtualizationError> {
        if self.capability & ((1 << 6) | (1 << 14)) != ((1 << 6) | (1 << 14))
            || (accessed_dirty && self.capability & (1 << 21) == 0)
        {
            return Err(VirtualizationError::UnsupportedPaging);
        }
        let address = root.as_usize() as u64;
        if address & 4095 != 0
            || !(12..64).contains(&self.address_bits)
            || address > (1u64 << self.address_bits) - 4096
        {
            return Err(VirtualizationError::InvalidRoot);
        }
        Ok(EptPointer(
            address | 6 | (3 << 3) | (u64::from(accessed_dirty) << 6),
        ))
    }

    /// Validates the current operand and completes its local invalidation.
    ///
    /// # Safety
    /// The same CPU must remain pinned and VMX enabled; table memory stays owned.
    pub(crate) unsafe fn invalidate(self, bits: u64) -> Result<(), VirtualizationError> {
        let root = PhysAddr::from_usize((bits & 0x000f_ffff_ffff_f000) as usize);
        let pointer = self.pointer(root, bits & (1 << 6) != 0)?;
        if pointer.bits() != bits {
            return Err(VirtualizationError::InvalidRoot);
        }
        let scope = invalidation_scope(self.capability)?;
        // SAFETY: the live binding retains this CPU's capability facts and the
        // actual EPTP has just been validated, including all reserved fields.
        unsafe { super::invalidate_ept(scope, bits) }
    }
}

fn invalidation_scope(capability: u64) -> Result<super::EptInvalidation, VirtualizationError> {
    if capability & (1 << 20) == 0 {
        Err(VirtualizationError::UnsupportedPaging)
    } else if capability & (1 << 25) != 0 {
        Ok(super::EptInvalidation::SingleContext)
    } else if capability & (1 << 26) != 0 {
        Ok(super::EptInvalidation::AllContexts)
    } else {
        Err(VirtualizationError::UnsupportedPaging)
    }
}
