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

//! Per-hart H-extension ownership and hardware capability discovery.

use core::{arch::asm, marker::PhantomData};

use super::VirtualizationError;

macro_rules! read_csr {
    ($csr:literal) => {{
        let value;
        // SAFETY: the enclosing transaction owns privileged access on this hart.
        unsafe { asm!(concat!("csrr {}, ", $csr), out(reg) value, options(nostack)); }
        value
    }};
}
macro_rules! write_csr {
    ($csr:literal, $value:expr) => {{
        // SAFETY: the transaction owns this CSR and supplies a saved or defined value.
        unsafe { asm!(concat!("csrw ", $csr, ", {}"), in(reg) $value, options(nostack)); }
    }};
}

#[derive(Debug)]
struct HostState {
    hedeleg: usize,
    hideleg: usize,
    hcounteren: usize,
    hvip: usize,
    hgatp: usize,
}

/// H-extension state belonging to one host hart.
///
/// This object neither enables physical IRQ sources nor owns a CPU-local area.
/// Its owner must disable it on the original hart before retiring it.
#[derive(Debug, Default)]
pub struct PerCpu {
    host: Option<HostState>,
    max_levels: usize,
    _not_send_sync: PhantomData<*mut ()>,
}

impl PerCpu {
    /// Constructs an inactive per-hart owner without touching hardware.
    pub const fn new() -> Self {
        Self {
            host: None,
            max_levels: 0,
            _not_send_sync: PhantomData,
        }
    }

    /// Reports whether this owner has installed its delegation policy.
    pub const fn is_enabled(&self) -> bool {
        self.host.is_some()
    }

    /// Enables the existing VS exception/interrupt delegation and counter access.
    ///
    /// # Safety
    /// The caller must exclusively own the hart's virtualization hardware,
    /// remain pinned with IRQs disabled, and have no active guest binding.
    /// It must retain this owner and call `disable` on this same hart before
    /// releasing hardware ownership or retiring the owner.
    pub unsafe fn enable(&mut self) -> Result<(), VirtualizationError> {
        if self.host.is_some() {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        if !crate::capability::has_hypervisor_extension() {
            return Err(VirtualizationError::Unavailable);
        }
        let host = HostState {
            hedeleg: read_csr!("hedeleg"),
            hideleg: read_csr!("hideleg"),
            hcounteren: read_csr!("hcounteren"),
            hvip: read_csr!("hvip"),
            hgatp: read_csr!("hgatp"),
        };
        let mut max_levels = 0;
        for (mode, levels) in [(9usize, 4), (8, 3)] {
            write_csr!("hgatp", mode << 60);
            let observed: usize = read_csr!("hgatp");
            if observed >> 60 == mode {
                max_levels = levels;
                break;
            }
        }
        write_csr!("hgatp", host.hgatp);
        // SAFETY: this exclusive HS transaction restores the original root
        // before invalidating translations possibly retained during probing.
        unsafe {
            fence_guest_translations();
        }
        if max_levels == 0 {
            return Err(VirtualizationError::UnsupportedPaging);
        }
        // Preserve the established guest-visible delegation and counter policy.
        write_csr!(
            "hedeleg",
            (1usize << 0) | (1 << 2) | (1 << 3) | (1 << 8) | (1 << 12) | (1 << 13) | (1 << 15)
        );
        write_csr!("hideleg", (1usize << 2) | (1 << 6) | (1 << 10));
        write_csr!("hvip", 0usize);
        write_csr!("hcounteren", usize::MAX);
        self.max_levels = max_levels;
        self.host = Some(host);
        Ok(())
    }

    /// Restores the hardware state saved by `enable`.
    ///
    /// # Safety
    /// The caller must be on the original hart with IRQs disabled and own all
    /// H-extension state. Every guest must be unbound before this operation.
    pub unsafe fn disable(&mut self) -> Result<(), VirtualizationError> {
        let host = self.host.take().ok_or(VirtualizationError::NotEnabled)?;
        write_csr!("hgatp", host.hgatp);
        // SAFETY: all guests have been unbound and the host root is restored.
        unsafe {
            fence_guest_translations();
        }
        write_csr!("hvip", host.hvip);
        write_csr!("hcounteren", host.hcounteren);
        write_csr!("hideleg", host.hideleg);
        write_csr!("hedeleg", host.hedeleg);
        self.max_levels = 0;
        Ok(())
    }

    /// Returns the capability recorded during successful hardware enable.
    pub const fn max_guest_page_table_levels(&self) -> usize {
        self.max_levels
    }

    /// Returns the corresponding implemented G-stage input address width.
    pub const fn guest_phys_addr_bits(&self) -> usize {
        match self.max_levels {
            3 => 41,
            4 => 50,
            _ => 0,
        }
    }
}

unsafe fn fence_guest_translations() {
    // SAFETY: the caller owns the current HS translation state.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +h",
            "hfence.gvma",
            ".option pop",
            options(nostack)
        );
    }
}
