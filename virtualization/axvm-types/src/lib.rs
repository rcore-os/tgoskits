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

//! Shared base types for AxVM and virtualization capability components.
//!
//! This crate intentionally contains only small value types and aliases. It is
//! not a host capability API and must not depend on any OS-specific crate.

#![no_std]

use ax_memory_addr::{AddrRange, PhysAddr, VirtAddr, def_usize_addr, def_usize_addr_formatter};

/// Virtual machine identifier.
pub type VMId = usize;

/// Virtual CPU identifier within a VM.
pub type VCpuId = usize;

/// Interrupt vector number injected into a guest.
pub type InterruptVector = u8;

/// The maximum number of virtual CPUs supported in a virtual machine.
pub const MAX_VCPU_NUM: usize = 64;

/// A set of virtual CPUs.
pub type VCpuSet = ax_cpumask::CpuMask<MAX_VCPU_NUM>;

/// Host virtual address.
pub type HostVirtAddr = VirtAddr;

/// Host physical address.
pub type HostPhysAddr = PhysAddr;

def_usize_addr! {
    /// Guest virtual address.
    pub type GuestVirtAddr;

    /// Guest physical address.
    pub type GuestPhysAddr;
}

def_usize_addr_formatter! {
    GuestVirtAddr = "GVA:{}";
    GuestPhysAddr = "GPA:{}";
}

/// Guest virtual address range.
pub type GuestVirtAddrRange = AddrRange<GuestVirtAddr>;

/// Guest physical address range.
pub type GuestPhysAddrRange = AddrRange<GuestPhysAddr>;

/// Common AxVM result type.
pub type AxVmResult<T = ()> = ax_errno::AxResult<T>;

/// Common AxVM error type.
pub type AxVmError = ax_errno::AxError;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
impl ax_page_table_multiarch::riscv::SvVirtAddr for GuestPhysAddr {
    /// Flushes the TLB for the entire address space.
    ///
    /// `hfence.vvma` does not access host memory.
    fn flush_tlb(_vaddr: Option<Self>) {
        unsafe {
            core::arch::asm!("hfence.vvma", options(nostack, nomem, preserves_flags));
        }
    }
}
