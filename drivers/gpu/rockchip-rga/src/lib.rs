//! Low-level building blocks for Rockchip RGA 2D accelerators.
//!
//! This crate intentionally starts with the smallest hardware-facing shape:
//! mapped RGA cores plus a DMA capability. Operation submission will be added
//! once the register programming path is verified against RK3588 hardware.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use core::ptr::NonNull;

use dma_api::DeviceDma;
use rdif_base::DriverGeneric;

use crate::backend::rga2::registers;

pub mod backend;

/// Rockchip RGA hardware generation known by this driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgaVersion {
    Rga2,
    Rga3,
}

/// Static description for one RGA core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgaCoreConfig {
    pub version: RgaVersion,
    pub core_index: u8,
}

impl RgaCoreConfig {
    pub const fn rga2(core_index: u8) -> Self {
        Self {
            version: RgaVersion::Rga2,
            core_index,
        }
    }

    pub const fn rga3(core_index: u8) -> Self {
        Self {
            version: RgaVersion::Rga3,
            core_index,
        }
    }
}

/// OS-glue supplied resource for one mapped RGA core.
#[derive(Debug, Clone, Copy)]
pub struct RgaCoreResource {
    pub base: NonNull<u8>,
    pub size: usize,
    pub irq: Option<usize>,
    pub config: RgaCoreConfig,
}

/// One mapped RGA core.
#[derive(Debug)]
pub struct RgaCore {
    base: NonNull<u8>,
    size: usize,
    irq: Option<usize>,
    config: RgaCoreConfig,
}

/// Raw hardware version decoded from the RGA version register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgaHardwareVersion {
    pub raw: u32,
    pub major: u8,
    pub minor: u8,
}

// The pointer is an MMIO base owned by platform glue. Access to mutable device
// state is kept behind `&mut self` in higher-level operations.
unsafe impl Send for RgaCore {}

impl RgaCore {
    pub fn new(resource: RgaCoreResource) -> Self {
        Self {
            base: resource.base,
            size: resource.size,
            irq: resource.irq,
            config: resource.config,
        }
    }

    pub fn base(&self) -> NonNull<u8> {
        self.base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn irq(&self) -> Option<usize> {
        self.irq
    }

    pub fn config(&self) -> RgaCoreConfig {
        self.config
    }

    pub fn read_version_info(&self) -> RgaHardwareVersion {
        let raw = self.read32(registers::VERSION_INFO);
        RgaHardwareVersion {
            raw,
            major: ((raw >> 24) & 0xff) as u8,
            minor: ((raw >> 20) & 0x0f) as u8,
        }
    }

    fn read32(&self, offset: usize) -> u32 {
        debug_assert_eq!(offset % core::mem::size_of::<u32>(), 0);
        unsafe { self.base.as_ptr().add(offset).cast::<u32>().read_volatile() }
    }
}

/// Rockchip RGA device containing one or more hardware cores.
pub struct RockchipRga {
    cores: Vec<RgaCore>,
    dma: DeviceDma,
}

impl RockchipRga {
    pub fn new(resources: &[RgaCoreResource], dma: DeviceDma) -> Self {
        Self {
            cores: resources.iter().copied().map(RgaCore::new).collect(),
            dma,
        }
    }

    pub fn core_count(&self) -> usize {
        self.cores.len()
    }

    pub fn core(&self, index: usize) -> Option<&RgaCore> {
        self.cores.get(index)
    }

    pub fn cores(&self) -> &[RgaCore] {
        &self.cores
    }

    pub fn dma(&self) -> &DeviceDma {
        &self.dma
    }
}

impl DriverGeneric for RockchipRga {
    fn name(&self) -> &str {
        "rockchip-rga"
    }
}
