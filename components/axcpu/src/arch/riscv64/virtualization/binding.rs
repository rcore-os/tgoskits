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

//! A pinned hardware binding with explicit guest capture and host restoration.

use core::{arch::asm, marker::PhantomData};

use super::{GuestVirtualHsCsrs, GuestVsCsrs, Vcpu, VirtualizationError};

macro_rules! read_csr {
    ($csr:literal) => {{ let value;
        // SAFETY: the enclosing binding owns this hart's privileged CSR bank.
        unsafe { asm!(concat!("csrr {}, ", $csr), out(reg) value, options(nostack)); }
        value
    }};
}
macro_rules! write_csr {
    ($csr:literal, $value:expr) => {{
        // SAFETY: the enclosing binding owns this hart and a valid saved value.
        unsafe { asm!(concat!("csrw ", $csr, ", {}"), in(reg) $value, options(nostack)); }
    }};
}

/// Loaded VS hardware state, confined to one pinned host execution scope.
///
/// `unload` saves the guest before restoring the host. Dropping without unloading
/// restores the host but discards unsaved guest changes, suitable for aborting
/// a failed entry scope. Neither operation releases page-table memory.
#[must_use = "the hardware binding must remain alive until guest execution finishes"]
pub struct GuestBinding {
    host_vs: GuestVsCsrs,
    host_hs: GuestVirtualHsCsrs,
    #[cfg(feature = "riscv-sstc")]
    host_henvcfg: usize,
    _not_send_sync: PhantomData<*mut ()>,
}

impl GuestBinding {
    /// Loads a validated guest register image into the current hart.
    ///
    /// # Safety
    /// The caller must own enabled HS virtualization and retain a CPU pin
    /// through destruction of the returned
    /// binding. No other guest binding may overlap it. Guest roots and referenced
    /// memory must remain valid and owned until the binding has been destroyed.
    /// All register values must satisfy the implemented CSR modes and permissions.
    pub unsafe fn load(registers: &Vcpu) -> Result<Self, VirtualizationError> {
        let _irqs = LocalIrqs::disable();
        #[cfg(feature = "riscv-sstc")]
        let host_henvcfg = {
            let saved: usize = read_csr!("henvcfg");
            write_csr!("henvcfg", saved | (1usize << 63));
            let actual: usize = read_csr!("henvcfg");
            if actual & (1usize << 63) == 0 {
                write_csr!("henvcfg", saved);
                return Err(VirtualizationError::UnsupportedTimer);
            }
            saved
        };
        // SAFETY: the caller owns the live HS and VS CSR banks exclusively.
        let (host_vs, host_hs) = unsafe { capture_banks() };
        let binding = Self {
            host_vs,
            host_hs,
            #[cfg(feature = "riscv-sstc")]
            host_henvcfg,
            _not_send_sync: PhantomData,
        };
        // SAFETY: the caller validated the guest register image and its roots.
        unsafe {
            install_banks(&registers.vs_csrs, &registers.virtual_hs_csrs);
        }
        Ok(binding)
    }

    /// Saves the guest bank and restores the host before returning.
    ///
    /// # Safety
    /// The caller must remain pinned on the binding's original hart.
    /// `registers` must be the exclusively owned image loaded by this binding.
    pub unsafe fn unload(self, registers: &mut Vcpu) {
        let _irqs = LocalIrqs::disable();
        // SAFETY: the caller retains this binding's exclusive current-hart scope.
        let (vs, hs) = unsafe { capture_banks() };
        registers.vs_csrs = vs;
        registers.virtual_hs_csrs = hs;
        drop(self);
    }
}

impl Drop for GuestBinding {
    fn drop(&mut self) {
        // SAFETY: load's contract retains the CPU pin through Drop. Excluding
        // IRQs also covers error unwinding outside the final entry window.
        let enabled = crate::interrupt::irqs_enabled();
        crate::interrupt::disable_irqs();
        unsafe {
            install_banks(&self.host_vs, &self.host_hs);
        }
        #[cfg(feature = "riscv-sstc")]
        write_csr!("henvcfg", self.host_henvcfg);
        if enabled {
            crate::interrupt::enable_irqs();
        }
    }
}

unsafe fn capture_banks() -> (GuestVsCsrs, GuestVirtualHsCsrs) {
    (
        GuestVsCsrs {
            htimedelta: read_csr!("htimedelta"),
            vsstatus: read_csr!("vsstatus"),
            vsie: read_csr!("vsie"),
            vstvec: read_csr!("vstvec"),
            vsscratch: read_csr!("vsscratch"),
            vsepc: read_csr!("vsepc"),
            vscause: read_csr!("vscause"),
            vstval: read_csr!("vstval"),
            vsatp: read_csr!("vsatp"),
            vstimecmp: {
                #[cfg(feature = "riscv-sstc")]
                {
                    read_csr!("vstimecmp")
                }
                #[cfg(not(feature = "riscv-sstc"))]
                {
                    0
                }
            },
        },
        GuestVirtualHsCsrs {
            hie: read_csr!("hie"),
            hvip: read_csr!("hvip"),
            hgeie: read_csr!("hgeie"),
            hgatp: read_csr!("hgatp"),
        },
    )
}

unsafe fn install_banks(vs: &GuestVsCsrs, hs: &GuestVirtualHsCsrs) {
    write_csr!("htimedelta", vs.htimedelta);
    write_csr!("vsstatus", vs.vsstatus);
    write_csr!("vsie", vs.vsie);
    write_csr!("vstvec", vs.vstvec);
    write_csr!("vsscratch", vs.vsscratch);
    write_csr!("vsepc", vs.vsepc);
    write_csr!("vscause", vs.vscause);
    write_csr!("vstval", vs.vstval);
    write_csr!("vsatp", vs.vsatp);
    #[cfg(feature = "riscv-sstc")]
    write_csr!("vstimecmp", vs.vstimecmp);
    write_csr!("hie", hs.hie);
    write_csr!("hvip", hs.hvip);
    write_csr!("hgeie", hs.hgeie);
    write_csr!("hgatp", hs.hgatp);
    // SAFETY: installing another VS root/VMID requires local two-stage
    // translation invalidation before any subsequent guest memory access.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +h",
            "hfence.gvma",
            "hfence.vvma",
            ".option pop",
            options(nostack)
        );
    }
}

pub(super) struct LocalIrqs(bool);
impl LocalIrqs {
    pub(super) fn disable() -> Self {
        let enabled = crate::interrupt::irqs_enabled();
        crate::interrupt::disable_irqs();
        Self(enabled)
    }
}
impl Drop for LocalIrqs {
    fn drop(&mut self) {
        if self.0 {
            crate::interrupt::enable_irqs();
        }
    }
}
