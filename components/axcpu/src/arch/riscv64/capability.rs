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

//! RISC-V hardware capability probes that preserve the caller's trap binding.

use core::{
    arch::{asm, naked_asm},
    mem::offset_of,
};

use riscv::register::{
    sstatus,
    stvec::{self, Stvec, TrapMode},
};

/// Probes whether the current supervisor can access the H-extension CSRs.
///
/// The probe temporarily installs a local vector with interrupts disabled.
/// Both the supported and illegal-instruction paths restore the caller's
/// scratch register, vector and interrupt-enable state before returning.
/// This reports the current execution environment, not an ISA firmware claim.
pub fn has_hypervisor_extension() -> bool {
    with_detect_trap() == 0
}

/// Probes the H-extension CSR while preserving the kernel register contract.
///
/// The caller owns the meaning of `sscratch`. The probe temporarily points
/// it at stack-owned state only for
/// the three-instruction assembly window that can trap; both the success path
/// and the trap vector restore the exact entry value before Rust runs again.
#[inline]
fn with_detect_trap() -> usize {
    let (sie, stvec) = init_detect_trap();
    let mut state = DetectState::new(super::registers::read_sscratch());
    run_h_extension_probe(&mut state);
    restore_detect_trap(sie, stvec);
    state.result
}

#[inline]
fn run_h_extension_probe(state: &mut DetectState) {
    let saved_sscratch = state.saved_sscratch;
    // SAFETY: init_detect_trap installed the matching direct vector with local
    // interrupts disabled. `state` remains live across the complete assembly
    // window, and both normal and exceptional paths restore the entry CSR.
    unsafe {
        asm!(
            "csrw sscratch, {state}",
            "csrr {probe_value}, 0x680",
            "csrw sscratch, {saved_sscratch}",
            state = in(reg) state,
            saved_sscratch = in(reg) saved_sscratch,
            probe_value = out(reg) _,
            options(nostack)
        )
    }
}

// Initialize environment for trap detection and filter in exception only
#[inline]
fn init_detect_trap() -> (bool, Stvec) {
    // clear SIE to handle exception only
    let stored_sie = sstatus::read().sie();
    // SAFETY: the previous SIE value is retained below and restored only after
    // the original stvec has been reinstalled.
    unsafe {
        sstatus::clear_sie();
    }
    // use detect trap handler to handle exceptions
    let stored_stvec = stvec::read();
    let trap_addr = on_detect_trap as *const () as usize;
    assert_eq!(
        trap_addr & 0b11,
        0,
        "H-extension probe trap vector must be four-byte aligned"
    );
    let mut stvec = Stvec::from_bits(0);
    stvec.set_address(trap_addr);
    stvec.set_trap_mode(TrapMode::Direct);

    // SAFETY: local interrupts are disabled and on_detect_trap is an aligned
    // direct-mode vector that handles the single fixed-width probe.
    unsafe { stvec::write(stvec) }
    (stored_sie, stored_stvec)
}

// Restore previous hardware states before trap detection
#[inline]
fn restore_detect_trap(sie: bool, stvec: Stvec) {
    // SAFETY: this is the inverse of init_detect_trap. The probe assembly has
    // already restored sscratch, so subsequent traps may use the host vector.
    unsafe {
        asm!("csrw  stvec, {}", in(reg) stvec.bits(), options(nomem, nostack));
        if sie {
            sstatus::set_sie();
        };
    }
}

/// Stack-owned state shared with the temporary probe trap vector.
#[repr(C)]
struct DetectState {
    saved_sscratch: usize,
    result: usize,
    saved_t0: usize,
    saved_t1: usize,
}

impl DetectState {
    const fn new(saved_sscratch: usize) -> Self {
        Self {
            saved_sscratch,
            result: 0,
            saved_t0: 0,
            saved_t1: 0,
        }
    }
}

/// Trap vector used only by the fixed-width H-extension CSR probe.
///
/// Local interrupts are disabled while this vector is installed. It records
/// the exception cause, advances past the four-byte CSR instruction, and
/// restores `sscratch` before returning to the probe assembly window. No Rust
/// code runs while `sscratch` contains the temporary state pointer.
#[unsafe(naked)]
unsafe extern "C" fn on_detect_trap() -> ! {
    naked_asm!(
        ".p2align 2",
        "csrrw  t0, sscratch, t0",
        "sd     t1, {saved_t1}(t0)",
        "csrr   t1, sscratch",
        "sd     t1, {saved_t0}(t0)",
        "csrr   t1, scause",
        "sd     t1, {result}(t0)",
        "csrr   t1, sepc",
        "addi   t1, t1, 4",
        "csrw   sepc, t1",
        "ld     t1, {saved_sscratch}(t0)",
        "csrw   sscratch, t1",
        "ld     t1, {saved_t1}(t0)",
        "ld     t0, {saved_t0}(t0)",
        "sret",
        saved_sscratch = const offset_of!(DetectState, saved_sscratch),
        result = const offset_of!(DetectState, result),
        saved_t0 = const offset_of!(DetectState, saved_t0),
        saved_t1 = const offset_of!(DetectState, saved_t1),
    )
}
