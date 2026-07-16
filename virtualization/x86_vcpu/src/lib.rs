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

#![no_std]
#![doc = include_str!("../README.md")]

#[cfg(any(feature = "vmx", feature = "svm"))]
#[macro_use]
extern crate log;
#[cfg(not(any(feature = "vmx", feature = "svm")))]
extern crate log;

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

#[cfg(all(feature = "vmx", feature = "svm"))]
compile_error!("features `vmx` and `svm` are mutually exclusive");

#[cfg(test)]
mod test_utils;

mod types;

pub use types::{
    X86AccessFlags, X86AccessWidth, X86GuestPhysAddr, X86GuestVirtAddr, X86HostPhysAddr,
    X86HostVirtAddr, X86MsrAddr, X86NestedPageFaultInfo, X86NestedPagingConfig, X86Port,
    X86VcpuError, X86VcpuResult, X86VmExit,
};

macro_rules! x86_err {
    ($kind:ident) => {
        Err($crate::X86VcpuError::$kind)
    };
    ($kind:ident, $msg:expr) => {{
        let _ = &$msg;
        Err($crate::X86VcpuError::$kind)
    }};
}

#[cfg(any(feature = "vmx", feature = "svm"))]
macro_rules! x86_err_type {
    ($kind:ident) => {
        $crate::X86VcpuError::$kind
    };
    ($kind:ident, $msg:expr) => {{
        let _ = &$msg;
        $crate::X86VcpuError::$kind
    }};
}

/// Maximum number of x86 host I/O port ranges configured for one vCPU.
pub const X86_MAX_PASSTHROUGH_PORT_RANGES: usize = 16;

/// x86 vCPU creation configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct X86VCpuCreateConfig;

/// x86 host I/O port range that should trap and be handled by the VMM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct X86PassthroughPortRange {
    /// First port in the range.
    pub base: u16,
    /// Number of ports in the range.
    pub length: u16,
}

/// Guest RAM region backing for x86 vCPU helpers.
#[derive(Clone, Copy, Debug)]
pub struct X86GuestMemoryRegion {
    /// Guest physical start address.
    pub gpa: X86GuestPhysAddr,
    /// Host virtual start address backing the guest memory.
    pub hva: X86HostVirtAddr,
    /// Region size in bytes.
    pub size: usize,
}

/// Guest MMIO range whose accesses are decoded and returned to the VMM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X86EmulatedMmioRegion {
    base: X86GuestPhysAddr,
    size: usize,
}

impl X86EmulatedMmioRegion {
    /// Creates a checked, non-empty guest MMIO range.
    pub fn new(base: X86GuestPhysAddr, size: usize) -> X86VcpuResult<Self> {
        if size == 0 || base.as_usize().checked_add(size).is_none() {
            return Err(X86VcpuError::InvalidInput);
        }
        Ok(Self { base, size })
    }

    /// Returns the first guest physical address in the range.
    pub const fn base(self) -> X86GuestPhysAddr {
        self.base
    }

    /// Returns the range size in bytes.
    pub const fn size(self) -> usize {
        self.size
    }

    /// Returns whether the range contains a guest physical address.
    pub fn contains(self, addr: X86GuestPhysAddr) -> bool {
        let offset = addr.as_usize().wrapping_sub(self.base.as_usize());
        offset < self.size
    }
}

/// x86 vCPU setup configuration.
#[derive(Clone, Debug)]
pub struct X86VCpuSetupConfig {
    emulate_com1: bool,
    passthrough_ports: [Option<X86PassthroughPortRange>; X86_MAX_PASSTHROUGH_PORT_RANGES],
    guest_memory_regions: Vec<X86GuestMemoryRegion>,
    emulated_mmio_regions: Vec<X86EmulatedMmioRegion>,
}

impl Default for X86VCpuSetupConfig {
    fn default() -> Self {
        Self {
            emulate_com1: false,
            passthrough_ports: [None; X86_MAX_PASSTHROUGH_PORT_RANGES],
            guest_memory_regions: Vec::new(),
            emulated_mmio_regions: Vec::new(),
        }
    }
}

impl X86VCpuSetupConfig {
    /// Selects whether COM1 port accesses are returned to the VMM.
    pub const fn set_com1_emulation(&mut self, enabled: bool) {
        self.emulate_com1 = enabled;
    }

    /// Returns whether COM1 port accesses are returned to the VMM.
    pub const fn emulates_com1(&self) -> bool {
        self.emulate_com1
    }

    /// Replaces the guest RAM regions used by instruction decoders.
    pub fn set_guest_memory_regions(
        &mut self,
        regions: Vec<X86GuestMemoryRegion>,
    ) -> X86VcpuResult {
        if regions.iter().any(|region| {
            region.size == 0
                || region.gpa.as_usize().checked_add(region.size).is_none()
                || region.hva.as_usize().checked_add(region.size).is_none()
        }) {
            return Err(X86VcpuError::InvalidInput);
        }
        self.guest_memory_regions = regions;
        Ok(())
    }

    /// Returns the guest RAM regions used by instruction decoders.
    pub fn guest_memory_regions(&self) -> &[X86GuestMemoryRegion] {
        &self.guest_memory_regions
    }

    /// Adds one guest MMIO range whose accesses must be decoded for the VMM.
    pub fn add_emulated_mmio_region(
        &mut self,
        base: X86GuestPhysAddr,
        size: usize,
    ) -> X86VcpuResult {
        let region = X86EmulatedMmioRegion::new(base, size)?;
        if !self.emulated_mmio_regions.contains(&region) {
            self.emulated_mmio_regions.push(region);
        }
        Ok(())
    }

    /// Returns the configured emulated MMIO ranges.
    pub fn emulated_mmio_regions(&self) -> &[X86EmulatedMmioRegion] {
        &self.emulated_mmio_regions
    }

    /// Adds one host I/O port range to the vCPU I/O intercept list.
    pub fn add_passthrough_port_range(&mut self, base: u16, length: u16) -> X86VcpuResult {
        if length == 0 {
            return Err(X86VcpuError::InvalidInput);
        }
        if base.checked_add(length - 1).is_none() {
            return Err(X86VcpuError::InvalidInput);
        }

        let range = X86PassthroughPortRange { base, length };
        if self.passthrough_ports.contains(&Some(range)) {
            return Ok(());
        }

        if let Some(slot) = self
            .passthrough_ports
            .iter_mut()
            .find(|slot| slot.is_none())
        {
            *slot = Some(range);
            return Ok(());
        }

        Err(X86VcpuError::NoMemory)
    }

    /// Iterates over configured host I/O port ranges.
    pub fn passthrough_port_ranges(&self) -> impl Iterator<Item = X86PassthroughPortRange> + '_ {
        self.passthrough_ports.iter().filter_map(|range| *range)
    }
}

pub mod host;
pub use host::X86HostOps;
pub(crate) mod msr;
#[cfg(feature = "vmx")]
#[macro_use]
pub(crate) mod regs;
mod ept;
#[cfg(not(feature = "vmx"))]
pub(crate) mod regs;
#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) mod xstate;

#[cfg(any(feature = "vmx", feature = "svm", test))]
const X86_RESET_VECTOR_GPA: usize = 0xffff_fff0;
#[cfg(any(feature = "vmx", feature = "svm", test))]
const X86_RESET_CS_SELECTOR: u16 = 0xf000;
#[cfg(any(feature = "vmx", feature = "svm", test))]
const X86_RESET_CS_BASE: usize = 0xffff_0000;

#[cfg(any(feature = "vmx", feature = "svm", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct X86RealModeEntryState {
    pub(crate) cs_selector: u16,
    pub(crate) cs_base: usize,
    pub(crate) rip: usize,
}

#[cfg(any(feature = "vmx", feature = "svm", test))]
pub(crate) fn x86_real_mode_entry_state(entry: X86GuestPhysAddr) -> X86RealModeEntryState {
    if entry.as_usize() == X86_RESET_VECTOR_GPA {
        return X86RealModeEntryState {
            cs_selector: X86_RESET_CS_SELECTOR,
            cs_base: X86_RESET_CS_BASE,
            rip: X86_RESET_VECTOR_GPA - X86_RESET_CS_BASE,
        };
    }

    X86RealModeEntryState {
        cs_selector: 0,
        cs_base: 0,
        rip: entry.as_usize(),
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "vmx")] {
        mod vmx;
        use vmx as vendor;
        pub use vmx::{VmxExitInfo, VmxExitReason, VmxInterruptInfo, VmxIoExitInfo};

        pub use vendor::{
            VmxArchPerCpuState, VmxArchPerCpuState as X86ArchPerCpuState, VmxArchVCpu,
            VmxArchVCpu as X86ArchVCpu, X86_APIC_ACCESS_GPA, x86_apic_access_page_addr,
        };
    } else if #[cfg(feature = "svm")] {
        mod svm;
        use svm as vendor;

        pub use svm::{SvmExitCode, SvmExitInfo, SvmIntercept};
        pub use vendor::{
            SvmArchPerCpuState, SvmArchPerCpuState as X86ArchPerCpuState, SvmArchVCpu,
            SvmArchVCpu as X86ArchVCpu,
        };
    } else {
        // Fallback stub types for builds without any hypervisor backend
        // (e.g. host-fs-only). Stubs implement the required traits so that
        // downstream crates can still compile; they are never instantiated.
        mod no_backend;
        pub use no_backend::{X86ArchPerCpuState, X86ArchVCpu};
    }
}

pub use ept::GuestPageWalkInfo;
pub use regs::GeneralRegisters;
#[cfg(any(feature = "vmx", feature = "svm"))]
pub use vendor::has_hardware_support;

#[cfg(not(any(feature = "vmx", feature = "svm")))]
pub fn has_hardware_support() -> bool {
    false
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn restore_host_interrupt_flag(host_rflags: u64) {
    if host_rflags & x86_64::registers::rflags::RFlags::INTERRUPT_FLAG.bits() != 0 {
        x86_64::instructions::interrupts::enable();
    } else {
        x86_64::instructions::interrupts::disable();
    }
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn host_tsc_frequency_mhz<H: X86HostOps>() -> Option<u32> {
    u32::try_from(host::nanos_to_ticks::<H>(1_000))
        .ok()
        .filter(|&freq| freq > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_mode_entry_keeps_normal_entry_flat() {
        assert_eq!(
            x86_real_mode_entry_state(X86GuestPhysAddr::from(0x8000)),
            X86RealModeEntryState {
                cs_selector: 0,
                cs_base: 0,
                rip: 0x8000,
            }
        );
    }

    #[test]
    fn real_mode_entry_maps_reset_vector_to_reset_cs_state() {
        assert_eq!(
            x86_real_mode_entry_state(X86GuestPhysAddr::from(0xffff_fff0)),
            X86RealModeEntryState {
                cs_selector: 0xf000,
                cs_base: 0xffff_0000,
                rip: 0xfff0,
            }
        );
    }

    #[test]
    fn setup_config_records_passthrough_port_ranges() {
        let mut config = X86VCpuSetupConfig::default();

        config.add_passthrough_port_range(0x6000, 0x80).unwrap();
        config.add_passthrough_port_range(0x6000, 0x80).unwrap();

        let ranges = config
            .passthrough_port_ranges()
            .collect::<std::vec::Vec<_>>();
        assert_eq!(
            ranges,
            std::vec![X86PassthroughPortRange {
                base: 0x6000,
                length: 0x80
            }]
        );
    }

    #[test]
    fn setup_config_rejects_invalid_or_excess_passthrough_port_ranges() {
        let mut config = X86VCpuSetupConfig::default();

        assert!(config.add_passthrough_port_range(0x6000, 0).is_err());
        assert!(config.add_passthrough_port_range(0xfff0, 0x20).is_err());

        for index in 0..X86_MAX_PASSTHROUGH_PORT_RANGES {
            config
                .add_passthrough_port_range((0x1000 + index * 0x10) as u16, 1)
                .unwrap();
        }
        assert!(config.add_passthrough_port_range(0x3000, 1).is_err());
    }

    #[test]
    fn setup_config_records_checked_emulated_mmio_regions_once() {
        let mut config = X86VCpuSetupConfig::default();
        let base = X86GuestPhysAddr::from_usize(0xfed8_0000);

        config.add_emulated_mmio_region(base, 0x1_0000).unwrap();
        config.add_emulated_mmio_region(base, 0x1_0000).unwrap();

        assert_eq!(
            config.emulated_mmio_regions(),
            &[X86EmulatedMmioRegion::new(base, 0x1_0000).unwrap()]
        );
        assert!(
            config.emulated_mmio_regions()[0].contains(X86GuestPhysAddr::from_usize(0xfed8_03c0))
        );
        assert!(config.add_emulated_mmio_region(base, 0).is_err());
        assert!(
            config
                .add_emulated_mmio_region(X86GuestPhysAddr::from_usize(usize::MAX), 2)
                .is_err()
        );
    }

    #[test]
    fn setup_config_validates_guest_memory_regions() {
        let mut config = X86VCpuSetupConfig::default();
        assert!(
            config
                .set_guest_memory_regions(std::vec![X86GuestMemoryRegion {
                    gpa: X86GuestPhysAddr::from_usize(0x1000),
                    hva: X86HostVirtAddr::from_usize(0x2000),
                    size: 0,
                }])
                .is_err()
        );
    }
}
