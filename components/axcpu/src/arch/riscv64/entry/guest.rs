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

use core::mem::offset_of;

use super::super::virtualization::{
    VectorBank, VectorCsrs, VectorEntry,
    context::{GuestCpuState, HypervisorCpuState, Vcpu},
};
use crate::{
    registers::{FpState, GprIndex},
    virtualization::StateMemory,
};

const fn hyp_gpr_offset(index: GprIndex) -> usize {
    offset_of!(Vcpu, hyp_regs) + offset_of!(HypervisorCpuState, gprs) + index.byte_offset()
}

const fn guest_gpr_offset(index: GprIndex) -> usize {
    offset_of!(Vcpu, guest_regs) + offset_of!(GuestCpuState, gprs) + index.byte_offset()
}

macro_rules! hyp_csr_offset {
    ($reg:tt) => {
        offset_of!(Vcpu, hyp_regs) + offset_of!(HypervisorCpuState, $reg)
    };
}

macro_rules! guest_csr_offset {
    ($reg:tt) => {
        offset_of!(Vcpu, guest_regs) + offset_of!(GuestCpuState, $reg)
    };
}

core::arch::global_asm!(
    include_fp_asm_macros!(),
    include_str!("vector.S"),
    include_str!("guest.S"),
    vector_entry = const offset_of!(Vcpu, vector_entry),
    vector_guest = const offset_of!(VectorEntry, guest),
    vector_host = const offset_of!(VectorEntry, host),
    vector_registers = const offset_of!(VectorBank, registers),
    vector_vstart = const offset_of!(VectorBank, csrs) + offset_of!(VectorCsrs, vstart),
    vector_vl = const offset_of!(VectorBank, csrs) + offset_of!(VectorCsrs, vl),
    vector_vtype = const offset_of!(VectorBank, csrs) + offset_of!(VectorCsrs, vtype),
    vector_vcsr = const offset_of!(VectorBank, csrs) + offset_of!(VectorCsrs, vcsr),
    trap_pc = const offset_of!(Vcpu, trap_csrs) + offset_of!(super::super::virtualization::context::Exit, pc),
    guest_fp = const offset_of!(Vcpu, fp),
    host_fp = const offset_of!(Vcpu, host_fp),
    fcsr = const offset_of!(FpState, fcsr),
    trap_scause = const offset_of!(Vcpu, trap_csrs) + offset_of!(super::super::virtualization::context::Exit, scause),
    trap_stval = const offset_of!(Vcpu, trap_csrs) + offset_of!(super::super::virtualization::context::Exit, stval),
    trap_htval = const offset_of!(Vcpu, trap_csrs) + offset_of!(super::super::virtualization::context::Exit, htval),
    trap_htinst = const offset_of!(Vcpu, trap_csrs) + offset_of!(super::super::virtualization::context::Exit, htinst),

    hyp_ra = const hyp_gpr_offset(GprIndex::RA),
    hyp_gp = const hyp_gpr_offset(GprIndex::GP),
    hyp_tp = const hyp_gpr_offset(GprIndex::TP),
    hyp_s0 = const hyp_gpr_offset(GprIndex::S0),
    hyp_s1 = const hyp_gpr_offset(GprIndex::S1),
    // hyp_a0 = const hyp_gpr_offset(GprIndex::A0),
    hyp_a1 = const hyp_gpr_offset(GprIndex::A1),
    hyp_a2 = const hyp_gpr_offset(GprIndex::A2),
    hyp_a3 = const hyp_gpr_offset(GprIndex::A3),
    hyp_a4 = const hyp_gpr_offset(GprIndex::A4),
    hyp_a5 = const hyp_gpr_offset(GprIndex::A5),
    hyp_a6 = const hyp_gpr_offset(GprIndex::A6),
    hyp_a7 = const hyp_gpr_offset(GprIndex::A7),
    hyp_s2 = const hyp_gpr_offset(GprIndex::S2),
    hyp_s3 = const hyp_gpr_offset(GprIndex::S3),
    hyp_s4 = const hyp_gpr_offset(GprIndex::S4),
    hyp_s5 = const hyp_gpr_offset(GprIndex::S5),
    hyp_s6 = const hyp_gpr_offset(GprIndex::S6),
    hyp_s7 = const hyp_gpr_offset(GprIndex::S7),
    hyp_s8 = const hyp_gpr_offset(GprIndex::S8),
    hyp_s9 = const hyp_gpr_offset(GprIndex::S9),
    hyp_s10 = const hyp_gpr_offset(GprIndex::S10),
    hyp_s11 = const hyp_gpr_offset(GprIndex::S11),
    hyp_sp = const hyp_gpr_offset(GprIndex::SP),
    hyp_sstatus = const hyp_csr_offset!(sstatus),
    hyp_hstatus = const hyp_csr_offset!(hstatus),
    hyp_scounteren = const hyp_csr_offset!(scounteren),
    hyp_stvec = const hyp_csr_offset!(stvec),
    hyp_sscratch = const hyp_csr_offset!(sscratch),
    guest_ra = const guest_gpr_offset(GprIndex::RA),
    guest_gp = const guest_gpr_offset(GprIndex::GP),
    guest_tp = const guest_gpr_offset(GprIndex::TP),
    guest_s0 = const guest_gpr_offset(GprIndex::S0),
    guest_s1 = const guest_gpr_offset(GprIndex::S1),
    guest_a0 = const guest_gpr_offset(GprIndex::A0),
    guest_a1 = const guest_gpr_offset(GprIndex::A1),
    guest_a2 = const guest_gpr_offset(GprIndex::A2),
    guest_a3 = const guest_gpr_offset(GprIndex::A3),
    guest_a4 = const guest_gpr_offset(GprIndex::A4),
    guest_a5 = const guest_gpr_offset(GprIndex::A5),
    guest_a6 = const guest_gpr_offset(GprIndex::A6),
    guest_a7 = const guest_gpr_offset(GprIndex::A7),
    guest_s2 = const guest_gpr_offset(GprIndex::S2),
    guest_s3 = const guest_gpr_offset(GprIndex::S3),
    guest_s4 = const guest_gpr_offset(GprIndex::S4),
    guest_s5 = const guest_gpr_offset(GprIndex::S5),
    guest_s6 = const guest_gpr_offset(GprIndex::S6),
    guest_s7 = const guest_gpr_offset(GprIndex::S7),
    guest_s8 = const guest_gpr_offset(GprIndex::S8),
    guest_s9 = const guest_gpr_offset(GprIndex::S9),
    guest_s10 = const guest_gpr_offset(GprIndex::S10),
    guest_s11 = const guest_gpr_offset(GprIndex::S11),
    guest_t0 = const guest_gpr_offset(GprIndex::T0),
    guest_t1 = const guest_gpr_offset(GprIndex::T1),
    guest_t2 = const guest_gpr_offset(GprIndex::T2),
    guest_t3 = const guest_gpr_offset(GprIndex::T3),
    guest_t4 = const guest_gpr_offset(GprIndex::T4),
    guest_t5 = const guest_gpr_offset(GprIndex::T5),
    guest_t6 = const guest_gpr_offset(GprIndex::T6),
    guest_sp = const guest_gpr_offset(GprIndex::SP),

    guest_sstatus = const guest_csr_offset!(sstatus),
    guest_hstatus = const guest_csr_offset!(hstatus),
    guest_scounteren = const guest_csr_offset!(scounteren),
    guest_sepc = const guest_csr_offset!(sepc),
);

unsafe extern "C" {
    // Assembly accesses only the fixed repr(C) register prefix. Its offsets
    // computed from Vcpu<NoVectorMemory> hold for every M: only the final field
    // depends on M. The actual Vcpu<M> pointer is passed directly as c_void;
    // no reference to a different Rust monomorphization is ever formed.
    fn __ax_cpu_riscv_run_guest(state: *mut core::ffi::c_void, vector: *mut VectorEntry);
}

/// Enters the guest and restores host integer, floating-point and CSR state.
///
/// # Safety
/// The caller must own this hart's H extension and bound VS CSR bank, keep
/// interrupts disabled, and provide valid guest translation and delegation.
/// The register image must remain exclusively accessible for the complete
/// call. The target must implement the F/D extensions used by this image.
/// Guest memory must not permit modifying host code, page tables or this image.
/// Host trap, stack, TLS and scratch bindings must be valid for restoration.
/// When vectors are enabled, the register length must equal this hart's `vlenb`
/// and all harts on which the guest executes must have a compatible vector ISA.
/// The host must not use vector instructions outside separately managed scopes.
pub unsafe fn enter_guest<M: StateMemory>(
    registers: &mut Vcpu<M>,
) -> super::super::virtualization::context::Exit {
    const VS_MASK: usize = 3 << 9;
    let mut vector = registers.vector.as_mut().map(|vector| vector.entry());
    let vector_entry = match vector.as_mut() {
        Some(entry) => {
            if registers.guest_regs.sstatus & VS_MASK == 0 {
                registers.guest_regs.sstatus |= 1 << 9;
            }
            entry as *mut VectorEntry
        }
        None => {
            // A guest without an owned bank must never execute vector state.
            registers.guest_regs.sstatus &= !VS_MASK;
            core::ptr::null_mut()
        }
    };
    // SAFETY: the exclusive Vcpu borrow keeps both owned vector banks live and
    // stable. The descriptor lives on this stack for the synchronous call;
    // assembly clears its temporary pointer before Rust can move or clone Vcpu.
    // The caller supplies the pinned CPU, translations and trap bindings.
    unsafe {
        super::super::virtualization::invalidate_gstage_translations();
        __ax_cpu_riscv_run_guest((registers as *mut Vcpu<M>).cast(), vector_entry);
    }
    if let (Some(state), Some(entry)) = (&mut registers.vector, vector) {
        state.controls = entry.guest.csrs;
    }
    registers.trap_csrs
}
