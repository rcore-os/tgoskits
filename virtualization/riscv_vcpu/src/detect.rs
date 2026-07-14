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

//! Detect instruction sets (ISA extensions) by trap-and-return procedure
//!
//! First, it disables all S-level interrupts. Remaining traps in RISC-V core
//! are all exceptions.
//! Then, it filters out illegal instruction from exceptions.
//! ref: <https://github.com/luojia65/zihai/blob/main/zihai/src/detect.rs>

use core::{
    arch::{asm, naked_asm},
    mem::offset_of,
};

use ax_cpu_local::CpuPin;
use riscv::register::{
    sstatus,
    stvec::{self, Stvec, TrapMode},
};
use riscv_h::register::hgatp::{self, Hgatp, HgatpValues};

/// Detect if hypervisor extension exists on current hart environment
///
/// This function tries to read hgatp and returns false if the read operation failed.
pub fn detect_h_extension(cpu_pin: &CpuPin) -> bool {
    let ans = with_detect_trap(cpu_pin);
    // return the answer from output flag. 0 => success; any trap means unsupported.
    ans == 0
}

/// Returns the maximum supported RISC-V G-stage page-table levels.
pub fn max_guest_page_table_levels(cpu_pin: &CpuPin) -> usize {
    if !detect_h_extension(cpu_pin) {
        return 0;
    }
    if detect_hgatp_mode(cpu_pin, HgatpValues::Sv48x4) {
        4
    } else if detect_hgatp_mode(cpu_pin, HgatpValues::Sv39x4) {
        3
    } else {
        0
    }
}

fn detect_hgatp_mode(cpu_pin: &CpuPin, mode: HgatpValues) -> bool {
    let _irq_guard = LocalIrqGuard::new(cpu_pin);
    let saved = hgatp::read();
    let mut candidate = Hgatp::from_bits(0);
    candidate.set_mode(mode);
    unsafe {
        candidate.write();
    }
    let supported = ((hgatp::read().bits() >> 60) & 0xf) == mode as usize;
    unsafe {
        saved.write();
    }
    supported
}

/// Probes the H-extension CSR while preserving the kernel register contract.
///
/// `sscratch` normally contains the current CPU anchor. The probe temporarily
/// points it at stack-owned state only for the three-instruction assembly
/// window that can trap; both the success path and the trap vector restore the
/// anchor before control reaches Rust again.
#[inline]
fn with_detect_trap(cpu_pin: &CpuPin) -> usize {
    let (sie, stvec) = init_detect_trap(cpu_pin);
    let mut state = DetectState::new(read_cpu_anchor());
    run_h_extension_probe(&mut state);
    restore_detect_trap(sie, stvec);
    state.result
}

#[inline]
fn read_cpu_anchor() -> usize {
    let anchor;
    // SAFETY: H-extension detection runs in supervisor context, where reading
    // the host scratch CSR is permitted and has no side effects.
    unsafe { asm!("csrr {}, sscratch", out(reg) anchor, options(nomem, nostack)) };
    anchor
}

#[inline]
fn run_h_extension_probe(state: &mut DetectState) {
    let anchor = state.cpu_anchor;
    // SAFETY: init_detect_trap installed the matching direct vector with local
    // interrupts disabled. `state` remains live across the complete assembly
    // window, and both normal and exceptional paths restore the CPU anchor.
    unsafe {
        asm!(
            "csrw sscratch, {state}",
            "csrr {probe_value}, 0x680",
            "csrw sscratch, {anchor}",
            state = in(reg) state,
            anchor = in(reg) anchor,
            probe_value = out(reg) _,
            options(nostack)
        )
    }
}

// Initialize environment for trap detection and filter in exception only
#[inline]
fn init_detect_trap(_cpu_pin: &CpuPin) -> (bool, Stvec) {
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

struct LocalIrqGuard {
    restore_sie: bool,
}

impl LocalIrqGuard {
    fn new(_cpu_pin: &CpuPin) -> Self {
        let restore_sie = sstatus::read().sie();
        // SAFETY: CpuPin prevents migration for the complete CSR transaction;
        // Drop restores the entry SIE state on the same CPU.
        unsafe { sstatus::clear_sie() };
        Self { restore_sie }
    }
}

impl Drop for LocalIrqGuard {
    fn drop(&mut self) {
        if self.restore_sie {
            // SAFETY: this is the inverse of LocalIrqGuard::new and CpuPin is
            // still borrowed by the surrounding operation.
            unsafe { sstatus::set_sie() };
        }
    }
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
    cpu_anchor: usize,
    result: usize,
    saved_t0: usize,
    saved_t1: usize,
}

impl DetectState {
    const fn new(cpu_anchor: usize) -> Self {
        Self {
            cpu_anchor,
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
        "ld     t1, {cpu_anchor}(t0)",
        "csrw   sscratch, t1",
        "ld     t1, {saved_t1}(t0)",
        "ld     t0, {saved_t0}(t0)",
        "sret",
        cpu_anchor = const offset_of!(DetectState, cpu_anchor),
        result = const offset_of!(DetectState, result),
        saved_t0 = const offset_of!(DetectState, saved_t0),
        saved_t1 = const offset_of!(DetectState, saved_t1),
    )
}
