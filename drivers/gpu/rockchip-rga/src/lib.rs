//! Low-level building blocks for Rockchip RGA 2D accelerators.
//!
//! This crate intentionally starts with the smallest hardware-facing shape:
//! mapped RGA cores plus a DMA capability. Operation submission will be added
//! once the register programming path is verified against RK3588 hardware.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::ptr::NonNull;

use dma_api::DeviceDma;
use rdif_base::DriverGeneric;

use crate::{
    backend::{RgaBackend, RgaStatus, rga2::Rga2Backend, rga3::Rga3Backend},
    capabilities::CoreCapabilities,
    error::{Result, RgaError},
    operation::RgaOperation,
};

pub mod backend;
pub mod buffer;
pub mod capabilities;
pub mod error;
pub mod operation;
pub mod selftest;

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

/// Raw hardware version decoded from the RGA version register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgaHardwareVersion {
    pub raw: u32,
    pub major: u8,
    pub minor: u8,
}

/// Lifecycle state of one RGA core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreState {
    Idle,
    Running,
    Recovering,
    Offline,
}

/// One mapped RGA core: a generation-specific backend plus its lifecycle state.
pub struct RgaCore {
    config: RgaCoreConfig,
    backend: Box<dyn RgaBackend>,
    caps: CoreCapabilities,
    state: CoreState,
}

impl RgaCore {
    pub fn new(resource: RgaCoreResource, dma: DeviceDma) -> Self {
        let backend: Box<dyn RgaBackend> = match resource.config.version {
            RgaVersion::Rga2 => Box::new(Rga2Backend::new(resource.base, dma)),
            RgaVersion::Rga3 => Box::new(Rga3Backend::new(resource.base, dma)),
        };
        let version = backend.read_version();
        let caps = CoreCapabilities::detect(resource.config.version, version);
        Self {
            config: resource.config,
            backend,
            caps,
            state: CoreState::Idle,
        }
    }

    pub fn config(&self) -> RgaCoreConfig {
        self.config
    }

    pub fn capabilities(&self) -> &CoreCapabilities {
        &self.caps
    }

    pub fn state(&self) -> CoreState {
        self.state
    }

    pub fn version(&self) -> RgaHardwareVersion {
        self.caps.version
    }

    /// Start a validated op (non-blocking). Caller then polls `poll_status()`.
    pub fn start(&mut self, op: &RgaOperation) -> Result<()> {
        if self.state != CoreState::Idle {
            return Err(RgaError::Busy);
        }
        op.validate()?;
        self.backend.supports(op)?;
        self.backend.submit(op)?;
        self.state = CoreState::Running;
        Ok(())
    }

    pub fn poll_status(&self) -> RgaStatus {
        self.backend.poll()
    }

    /// Call after `poll_status()` reports Done/Error.
    pub fn finish(&mut self) {
        self.backend.ack();
        self.state = CoreState::Idle;
    }

    pub fn recover(&mut self) -> Result<()> {
        self.state = CoreState::Recovering;
        let r = self.backend.reset();
        self.state = if r.is_ok() {
            CoreState::Idle
        } else {
            CoreState::Offline
        };
        r
    }
}

/// Rockchip RGA device containing one or more hardware cores.
pub struct RockchipRga {
    cores: Vec<RgaCore>,
    dma: DeviceDma,
}

impl RockchipRga {
    pub fn new(resources: &[RgaCoreResource], dma: DeviceDma) -> Self {
        let cores = resources
            .iter()
            .copied()
            .map(|r| RgaCore::new(r, dma.clone()))
            .collect();
        Self { cores, dma }
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

    pub fn cores_mut(&mut self) -> &mut [RgaCore] {
        &mut self.cores
    }

    pub fn core_mut(&mut self, i: usize) -> Option<&mut RgaCore> {
        self.cores.get_mut(i)
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
