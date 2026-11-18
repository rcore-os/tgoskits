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

use crate::{
    registers::{FpState, GeneralRegisters},
    virtualization::StateMemory,
};

/// Uninhabited storage policy for vCPUs without vector banks.
#[derive(Debug, Clone, Copy)]
pub enum NoVectorMemory {}

// SAFETY: this type has no inhabitants and can never supply a memory lease.
unsafe impl StateMemory for NoVectorMemory {
    fn virtual_address(&self) -> core::ptr::NonNull<u8> {
        match *self {}
    }
    fn byte_len(&self) -> usize {
        match *self {}
    }
}

/// Hypervisor GPR and CSR state which must be saved/restored when entering/exiting virtualization.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct HypervisorCpuState {
    /// Saved `gprs` register state.
    pub gprs: GeneralRegisters,
    /// Saved `sstatus` register state.
    pub sstatus: usize,
    /// Saved `hstatus` register state.
    pub hstatus: usize,
    /// Saved `scounteren` register state.
    pub scounteren: usize,
    /// Saved `stvec` register state.
    pub stvec: usize,
    /// Saved `sscratch` register state.
    pub sscratch: usize,
}

/// Guest GPR and CSR state which must be saved/restored when exiting/entering virtualization.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GuestCpuState {
    /// Saved `gprs` register state.
    pub gprs: GeneralRegisters,
    /// Saved `sstatus` register state.
    pub sstatus: usize,
    /// Saved `hstatus` register state.
    pub hstatus: usize,
    /// Saved `scounteren` register state.
    pub scounteren: usize,
    /// Saved `sepc` register state.
    pub sepc: usize,
}

/// Guest CSR state saved and restored whenever ownership of the VS bank changes.
///
/// This includes `senvcfg`, which is shared between HS and VS execution.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GuestVsCsrs {
    /// Saved `senvcfg` userspace environment controls.
    pub senvcfg: usize,
    /// Saved `htimedelta` register state.
    pub htimedelta: usize,
    /// Saved `vsstatus` register state.
    pub vsstatus: usize,
    /// Saved `vsie` register state.
    pub vsie: usize,
    /// Saved `vstvec` register state.
    pub vstvec: usize,
    /// Saved `vsscratch` register state.
    pub vsscratch: usize,
    /// Saved `vsepc` register state.
    pub vsepc: usize,
    /// Saved `vscause` register state.
    pub vscause: usize,
    /// Saved `vstval` register state.
    pub vstval: usize,
    /// Saved `vsatp` register state.
    pub vsatp: usize,
    /// Saved `vstimecmp` register state.
    pub vstimecmp: usize,
}

/// Virtualized HS-level CSRs that are used to emulate (part of) the hypervisor extension for the
/// guest.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GuestVirtualHsCsrs {
    /// Saved `hie` register state.
    pub hie: usize,
    // hvip lives at HS level, but its pending bits directly determine which
    // virtual interrupts the guest will observe after the next VM entry.
    /// Saved `hvip` register state.
    pub hvip: usize,
    /// Saved `hgeie` register state.
    pub hgeie: usize,
    /// Saved `hgatp` register state.
    pub hgatp: usize,
}

/// CSRs written on an exit from virtualization that are used by the hypervisor to determine the cause
/// of the trap.
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct Exit {
    /// Saved `scause` register state.
    pub scause: usize,
    /// Saved `stval` register state.
    pub stval: usize,
    /// Saved `htval` register state.
    pub htval: usize,
    /// Saved `htinst` register state.
    pub htinst: usize,
    /// Guest instruction pointer at this exit.
    pub pc: usize,
}

impl Exit {
    /// Returns the guest physical address that caused a guest page fault.
    ///
    /// Valid after the owning register image has returned from guest entry.
    pub fn gpt_page_fault_addr(&self) -> usize {
        guest_page_fault_addr(self.htval, self.stval)
    }
}

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
///
/// Vector banks are host-provided [`StateMemory`] leases. `Clone` is available
/// when `M: Clone` and relies on the lease contract to preserve bank contents
/// and alignment with independent backing.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct Vcpu<M: StateMemory = NoVectorMemory> {
    // CPU state that's shared between our's and the guest's execution environment. Saved/restored
    // when entering/exiting a VM.
    /// Saved `hyp_regs` register state.
    pub hyp_regs: HypervisorCpuState,
    /// Saved `guest_regs` register state.
    pub guest_regs: GuestCpuState,

    /// CPU state that only applies when V=1, e.g. the VS-level CSRs. Saved/restored on activation of
    /// the vCPU. This field IS NOT automatically saved/restored on VM entry/exit, users must do it
    /// manually.
    /// Saved `vs_csrs` register state.
    pub vs_csrs: GuestVsCsrs,

    /// Virtualized HS-level CPU state. This field IS NOT automatically saved/restored on VM
    /// entry/exit, users must do it manually.
    /// Saved `virtual_hs_csrs` register state.
    pub virtual_hs_csrs: GuestVirtualHsCsrs,

    /// Trap-related CSRs captured by the exit owner before dispatch.
    /// Saved `trap_csrs` register state.
    pub trap_csrs: Exit,

    /// Guest floating-point image, independent of the host thread FP bank.
    pub fp: FpState,

    /// Host FP storage used only during one assembly entry transaction.
    pub(in crate::arch::riscv64) host_fp: FpState,

    // Installed and cleared by assembly within one exclusive entry call.
    // Always zero whenever Rust can clone or move this register image.
    pub(in crate::arch::riscv64) vector_entry: usize,

    // Keep this generic field last: assembly offsets use Vcpu<NoVectorMemory>
    // and access only the fixed repr(C) prefix above for every M. No borrowed
    // pointer is retained in the owned vector storage between entries.
    pub(in crate::arch::riscv64) vector: Option<super::vector::VectorState<M>>,
}

impl<M: StateMemory> Default for Vcpu<M> {
    fn default() -> Self {
        Self {
            hyp_regs: HypervisorCpuState::default(),
            guest_regs: GuestCpuState::default(),
            vs_csrs: GuestVsCsrs::default(),
            virtual_hs_csrs: GuestVirtualHsCsrs::default(),
            trap_csrs: Exit::default(),
            fp: FpState::default(),
            host_fp: FpState::default(),
            vector_entry: 0,
            vector: None,
        }
    }
}

impl<M: StateMemory> Vcpu<M> {
    /// Creates a vCPU using host-owned vector storage.
    ///
    /// `vlenb` must be a power of two from 16 through 8192. Each lease must
    /// contain at least `32 * vlenb` bytes aligned for `usize`. The used range
    /// is initialized to zero. Use `Default` to disable vector state.
    pub fn with_vector_memory(
        vlenb: usize,
        host: M,
        guest: M,
    ) -> Result<Self, super::VirtualizationError> {
        Ok(Self {
            vector: Some(super::vector::VectorState::new(vlenb, host, guest)?),
            ..Self::default()
        })
    }

    /// Returns the vector register length in bytes, or zero when disabled.
    pub fn vector_register_bytes(&self) -> usize {
        self.vector
            .as_ref()
            .map_or(0, |vector| vector.register_bytes())
    }
}

/// Reconstructs a guest physical fault address from HS trap register values.
pub const fn guest_page_fault_addr(htval: usize, stval: usize) -> usize {
    (htval << 2) | (stval & 3)
}
