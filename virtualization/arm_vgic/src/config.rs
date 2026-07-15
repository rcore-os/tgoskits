//! Validated per-VM GICv3 configuration.

use crate::{LPI_INTID_BASE, LPI_INTID_MAX, PrivateInterruptMask, VgicError, VgicResult};

/// GICv3 implementation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GicV3Mode {
    /// Distributor, Redistributors, ITS, and delivery state are modeled in software.
    Emulated,
    /// Assigned resources are delivered directly through a checked physical backend.
    Passthrough,
}

/// Validated capabilities reported by a physical GICv3 Distributor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GicV3HardwareCapabilities {
    spi_count: usize,
}

impl GicV3HardwareCapabilities {
    /// Decodes the implemented SPI range from `GICD_TYPER.ITLinesNumber`.
    pub fn from_distributor_typer(typer: u32) -> VgicResult<Self> {
        let implemented_intids = ((typer & 0x1f) as usize + 1) * 32;
        let spi_count = implemented_intids
            .min(1020)
            .checked_sub(32)
            .filter(|count| *count != 0)
            .ok_or_else(|| VgicError::InvalidConfig {
                detail: alloc::format!("GICD_TYPER {typer:#x} exposes no SPIs"),
            })?;
        Ok(Self { spi_count })
    }

    /// Returns the number of implemented SPIs.
    pub const fn spi_count(self) -> usize {
        self.spi_count
    }
}

/// One guest-visible MMIO register frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GicV3MmioRegion {
    base: u64,
    size: u64,
}

impl GicV3MmioRegion {
    /// Creates a non-empty, non-wrapping register frame.
    pub fn new(base: u64, size: u64) -> VgicResult<Self> {
        if size == 0 {
            return Err(VgicError::InvalidConfig {
                detail: "GICv3 MMIO region must not be empty".into(),
            });
        }
        if base.checked_add(size).is_none() {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "GICv3 MMIO region [{base:#x}, +{size:#x}) wraps the address space"
                ),
            });
        }
        Ok(Self { base, size })
    }

    /// Returns the guest physical base address.
    pub const fn base(self) -> u64 {
        self.base
    }

    /// Returns the frame size in bytes.
    pub const fn size(self) -> u64 {
        self.size
    }

    /// Returns whether the region contains an entire access.
    pub fn contains(self, address: u64, length: usize) -> bool {
        address >= self.base
            && address
                .checked_add(length as u64)
                .is_some_and(|end| end <= self.base + self.size)
    }
}

/// Complete configuration for one VM-local GICv3 controller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GicV3Config {
    mode: GicV3Mode,
    distributor: GicV3MmioRegion,
    redistributors: GicV3MmioRegion,
    redistributor_stride: u64,
    vcpu_count: usize,
    its: Option<GicV3MmioRegion>,
    spi_count: usize,
    lpi_limit: u32,
    list_register_count: usize,
    its_command_budget: usize,
    passthrough_private_interrupts: PrivateInterruptMask,
}

impl GicV3Config {
    /// Creates a GICv3 configuration with architectural defaults.
    pub fn new(
        mode: GicV3Mode,
        distributor: GicV3MmioRegion,
        redistributors: GicV3MmioRegion,
        redistributor_stride: u64,
        vcpu_count: usize,
    ) -> VgicResult<Self> {
        let config = Self {
            mode,
            distributor,
            redistributors,
            redistributor_stride,
            vcpu_count,
            its: None,
            spi_count: 988,
            lpi_limit: LPI_INTID_MAX,
            list_register_count: 16,
            its_command_budget: 256,
            passthrough_private_interrupts: PrivateInterruptMask::SGIS,
        };
        config.validate()?;
        Ok(config)
    }

    /// Adds a guest-visible ITS frame.
    pub fn with_its(mut self, its: GicV3MmioRegion) -> VgicResult<Self> {
        self.its = Some(its);
        self.validate()?;
        Ok(self)
    }

    /// Sets the implemented SPI count.
    pub fn with_spi_count(mut self, spi_count: usize) -> VgicResult<Self> {
        self.spi_count = spi_count;
        self.validate()?;
        Ok(self)
    }

    /// Sets the highest implemented LPI INTID.
    pub fn with_lpi_limit(mut self, lpi_limit: u32) -> VgicResult<Self> {
        self.lpi_limit = lpi_limit;
        self.validate()?;
        Ok(self)
    }

    /// Sets the physical list-register count exposed by the backend.
    pub fn with_list_register_count(mut self, count: usize) -> VgicResult<Self> {
        self.list_register_count = count;
        self.validate()?;
        Ok(self)
    }

    /// Sets the maximum ITS commands processed by one CWRITER update.
    pub fn with_its_command_budget(mut self, budget: usize) -> VgicResult<Self> {
        self.its_command_budget = budget;
        self.validate()?;
        Ok(self)
    }

    /// Selects private interrupts that are context-switched for a passthrough guest.
    ///
    /// SGIs are always included because guest SGI delivery is trapped and
    /// multiplexed independently of the host's use of the same INTIDs.
    pub fn with_passthrough_private_interrupts(
        mut self,
        interrupts: PrivateInterruptMask,
    ) -> VgicResult<Self> {
        if self.mode != GicV3Mode::Passthrough {
            return Err(VgicError::InvalidConfig {
                detail: "private physical interrupt ownership requires passthrough mode".into(),
            });
        }
        self.passthrough_private_interrupts = interrupts.union(PrivateInterruptMask::SGIS);
        self.validate()?;
        Ok(self)
    }

    /// Returns the controller mode.
    pub const fn mode(&self) -> GicV3Mode {
        self.mode
    }

    /// Returns the Distributor frame.
    pub const fn distributor(&self) -> GicV3MmioRegion {
        self.distributor
    }

    /// Returns the complete Redistributor frame range.
    pub const fn redistributors(&self) -> GicV3MmioRegion {
        self.redistributors
    }

    /// Returns the distance between Redistributor frames.
    pub const fn redistributor_stride(&self) -> u64 {
        self.redistributor_stride
    }

    /// Returns the configured vCPU count.
    pub const fn vcpu_count(&self) -> usize {
        self.vcpu_count
    }

    /// Returns the optional ITS frame.
    pub const fn its(&self) -> Option<GicV3MmioRegion> {
        self.its
    }

    /// Returns the implemented SPI count.
    pub const fn spi_count(&self) -> usize {
        self.spi_count
    }

    /// Returns the exclusive upper bound of implemented SPI INTIDs.
    pub const fn spi_limit(&self) -> u32 {
        32 + self.spi_count as u32
    }

    /// Returns the highest implemented LPI.
    pub const fn lpi_limit(&self) -> u32 {
        self.lpi_limit
    }

    /// Returns the list-register count.
    pub const fn list_register_count(&self) -> usize {
        self.list_register_count
    }

    /// Returns the ITS submission budget.
    pub const fn its_command_budget(&self) -> usize {
        self.its_command_budget
    }

    /// Returns the guest-visible private interrupt set.
    pub const fn guest_private_interrupts(&self) -> PrivateInterruptMask {
        match self.mode {
            GicV3Mode::Emulated => PrivateInterruptMask::ALL,
            GicV3Mode::Passthrough => self.passthrough_private_interrupts,
        }
    }

    pub(crate) const fn exposes_guest_lpis(&self) -> bool {
        matches!(self.mode, GicV3Mode::Emulated) && self.its.is_some()
    }

    fn validate(&self) -> VgicResult {
        const GICD_MIN_SIZE: u64 = 0x1_0000;
        const GIC_FRAME_ALIGNMENT: u64 = 0x1_0000;
        const GICR_MIN_STRIDE: u64 = 0x2_0000;

        validate_frame_alignment("Distributor", self.distributor, GIC_FRAME_ALIGNMENT)?;
        validate_frame_alignment("Redistributor", self.redistributors, GIC_FRAME_ALIGNMENT)?;
        if self.distributor.size() < GICD_MIN_SIZE {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "Distributor frame must be at least {GICD_MIN_SIZE:#x} bytes"
                ),
            });
        }
        if self.vcpu_count == 0 {
            return Err(VgicError::InvalidConfig {
                detail: "GICv3 requires at least one vCPU".into(),
            });
        }
        if self.vcpu_count > u16::MAX as usize + 1 {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "GICv3 vCPU count {} exceeds the 16-bit Processor_Number namespace",
                    self.vcpu_count
                ),
            });
        }
        if self.redistributor_stride < GICR_MIN_STRIDE
            || !self.redistributor_stride.is_multiple_of(0x1_0000)
        {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "Redistributor stride {:#x} must be a 64-KiB-aligned value of at least \
                     {GICR_MIN_STRIDE:#x}",
                    self.redistributor_stride
                ),
            });
        }
        let required_redistributor_size = self
            .redistributor_stride
            .checked_mul(self.vcpu_count as u64)
            .ok_or_else(|| VgicError::InvalidConfig {
                detail: "Redistributor frame size overflows".into(),
            })?;
        if self.redistributors.size() < required_redistributor_size {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "Redistributor region has size {:#x}, but {:#x} is required for {} vCPUs",
                    self.redistributors.size(),
                    required_redistributor_size,
                    self.vcpu_count
                ),
            });
        }
        if regions_overlap(self.distributor, self.redistributors) {
            return Err(VgicError::InvalidConfig {
                detail: "Distributor and Redistributor MMIO regions overlap".into(),
            });
        }
        if let Some(its) = self.its {
            validate_frame_alignment("ITS", its, GIC_FRAME_ALIGNMENT)?;
            if its.size() < GICD_MIN_SIZE {
                return Err(VgicError::InvalidConfig {
                    detail: alloc::format!("ITS frame must be at least {GICD_MIN_SIZE:#x} bytes"),
                });
            }
            if regions_overlap(its, self.distributor) || regions_overlap(its, self.redistributors) {
                return Err(VgicError::InvalidConfig {
                    detail: "ITS MMIO region overlaps another GICv3 frame".into(),
                });
            }
        }
        if self.spi_count == 0
            || self.spi_count > 988
            || (self.spi_count != 988 && !(self.spi_count + 32).is_multiple_of(32))
        {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "SPI count {} must be a non-zero multiple of 32, or the architectural maximum \
                     988",
                    self.spi_count
                ),
            });
        }
        if !(LPI_INTID_BASE..=LPI_INTID_MAX).contains(&self.lpi_limit) {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!("invalid LPI limit {}", self.lpi_limit),
            });
        }
        if !(1..=16).contains(&self.list_register_count) {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "list-register count {} must be in 1..=16",
                    self.list_register_count
                ),
            });
        }
        if self.its_command_budget == 0 {
            return Err(VgicError::InvalidConfig {
                detail: "ITS command budget must be non-zero".into(),
            });
        }
        Ok(())
    }
}

fn validate_frame_alignment(
    name: &'static str,
    region: GicV3MmioRegion,
    alignment: u64,
) -> VgicResult {
    if region.base().is_multiple_of(alignment) && region.size().is_multiple_of(alignment) {
        Ok(())
    } else {
        Err(VgicError::InvalidConfig {
            detail: alloc::format!(
                "{name} MMIO base {:#x} and size {:#x} must be {alignment:#x}-byte aligned",
                region.base(),
                region.size()
            ),
        })
    }
}

fn regions_overlap(left: GicV3MmioRegion, right: GicV3MmioRegion) -> bool {
    left.base() < right.base() + right.size() && right.base() < left.base() + left.size()
}
