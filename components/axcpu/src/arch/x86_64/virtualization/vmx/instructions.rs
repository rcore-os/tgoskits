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

//! Local invalidation of translations derived from EPT.

use crate::virtualization::VirtualizationError;

/// Scope of a local EPT translation invalidation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
pub enum EptInvalidation {
    /// Invalidate translations associated with one EPT root.
    SingleContext = 1,
    /// Invalidate translations associated with every EPT root on this CPU.
    AllContexts   = 2,
}

/// Invalidates this CPU's cached EPT translations.
///
/// # Safety
/// The caller must own a VMX-enabled, pinned CPU and ensure the selected
/// invalidation type is supported by IA32_VMX_EPT_VPID_CAP. For a single
/// context, `ept_pointer` must be a valid EPTP. This operation does not replace
/// remote-CPU invalidation before freeing guest translation structures.
pub unsafe fn invalidate_ept(
    scope: EptInvalidation,
    ept_pointer: u64,
) -> Result<(), VirtualizationError> {
    let descriptor = [ept_pointer, 0u64];
    let failed: u8;
    // SAFETY: the descriptor is initialized and valid throughout this
    // synchronous instruction. Capture both failure flags in the same asm
    // block, before compiler-generated instructions can overwrite RFLAGS.
    unsafe {
        core::arch::asm!(
            "invept {scope}, [{descriptor}]",
            "setbe {failed}",
            scope = in(reg) scope as u64,
            descriptor = in(reg) descriptor.as_ptr(),
            failed = lateout(reg_byte) failed,
            options(nostack),
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(VirtualizationError::InstructionFailed)
    }
}
