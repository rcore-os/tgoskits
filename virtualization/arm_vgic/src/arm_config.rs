//! Immutable configuration shared by VGIC construction and guest firmware.

use alloc::vec::Vec;

use axdevice_base::{HostIrqId, InterruptControllerId, InterruptTrigger, ItsId};

use crate::{
    GicAffinity, GicV3Config, GicV3MmioRegion, GicV3SpiOwnership, LPI_INTID_MAX, SpiId, VgicError,
    VgicResult,
};

/// One sanitized guest-visible register region.
pub type VgicMmioRegion = GicV3MmioRegion;

/// One physical SPI binding fixed before controller creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssignedSpiConfig {
    intid: SpiId,
    host_irq: HostIrqId,
    target_vcpu: usize,
    trigger: InterruptTrigger,
}

impl AssignedSpiConfig {
    /// Creates an identity-mapped physical SPI binding.
    pub fn new(
        intid: SpiId,
        host_irq: HostIrqId,
        target_vcpu: usize,
        trigger: InterruptTrigger,
    ) -> VgicResult<Self> {
        if intid.raw() as usize != host_irq.value() {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "physical SPI identity mapping requires guest INTID {} to equal host IRQ {}",
                    intid.raw(),
                    host_irq.value()
                ),
            });
        }
        Ok(Self {
            intid,
            host_irq,
            target_vcpu,
            trigger,
        })
    }

    /// Returns the guest-visible SPI.
    pub const fn intid(self) -> SpiId {
        self.intid
    }

    /// Returns the immutable host interrupt.
    pub const fn host_irq(self) -> HostIrqId {
        self.host_irq
    }

    /// Returns the fixed target vCPU.
    pub const fn target_vcpu(self) -> usize {
        self.target_vcpu
    }

    /// Returns the immutable physical trigger.
    pub const fn trigger(self) -> InterruptTrigger {
        self.trigger
    }
}

/// One guest-visible ITS instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItsConfig {
    id: ItsId,
    registers: VgicMmioRegion,
}

impl ItsConfig {
    /// Creates an ITS descriptor.
    pub const fn new(id: ItsId, registers: VgicMmioRegion) -> Self {
        Self { id, registers }
    }

    /// Returns the VM-local ITS ID.
    pub const fn id(self) -> ItsId {
        self.id
    }

    /// Returns the register aperture.
    pub const fn registers(self) -> VgicMmioRegion {
        self.registers
    }
}

/// Complete GICv2 configuration for one VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VgicV2Config {
    controller_id: InterruptControllerId,
    distributor: VgicMmioRegion,
    cpu_interface: VgicMmioRegion,
    vcpu_affinities: Vec<GicAffinity>,
    spi_count: usize,
    list_register_count: usize,
    priority_bits: u8,
    assigned_spis: Vec<AssignedSpiConfig>,
}

impl VgicV2Config {
    /// Creates a GICv2 configuration with architectural defaults.
    pub fn new(
        controller_id: InterruptControllerId,
        distributor: VgicMmioRegion,
        cpu_interface: VgicMmioRegion,
        vcpu_affinities: Vec<GicAffinity>,
    ) -> VgicResult<Self> {
        let config = Self {
            controller_id,
            distributor,
            cpu_interface,
            vcpu_affinities,
            spi_count: 988,
            list_register_count: 4,
            priority_bits: 5,
            assigned_spis: Vec::new(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Sets the implemented SPI count.
    pub fn with_spi_count(mut self, count: usize) -> VgicResult<Self> {
        self.spi_count = count;
        self.validate()?;
        Ok(self)
    }

    /// Sets the host LR count.
    pub fn with_list_register_count(mut self, count: usize) -> VgicResult<Self> {
        self.list_register_count = count;
        self.validate()?;
        Ok(self)
    }

    /// Sets the implemented priority width.
    pub fn with_priority_bits(mut self, bits: u8) -> VgicResult<Self> {
        self.priority_bits = bits;
        self.validate()?;
        Ok(self)
    }

    /// Adds prevalidated identity-backed SPIs.
    pub fn with_assigned_spis(mut self, assigned_spis: Vec<AssignedSpiConfig>) -> VgicResult<Self> {
        self.assigned_spis = assigned_spis;
        self.validate()?;
        Ok(self)
    }

    /// Returns the controller ID.
    pub const fn controller_id(&self) -> InterruptControllerId {
        self.controller_id
    }

    /// Returns the Distributor region.
    pub const fn distributor(&self) -> VgicMmioRegion {
        self.distributor
    }

    /// Returns the CPU-interface region.
    pub const fn cpu_interface(&self) -> VgicMmioRegion {
        self.cpu_interface
    }

    /// Returns vCPU affinities in vCPU-ID order.
    pub fn vcpu_affinities(&self) -> &[GicAffinity] {
        &self.vcpu_affinities
    }

    /// Returns assigned physical SPIs.
    pub fn assigned_spis(&self) -> &[AssignedSpiConfig] {
        &self.assigned_spis
    }

    /// Returns the implemented priority width.
    pub const fn priority_bits(&self) -> u8 {
        self.priority_bits
    }

    /// Returns the implemented SPI count.
    pub const fn spi_count(&self) -> usize {
        self.spi_count
    }

    /// Returns the configured LR count.
    pub const fn list_register_count(&self) -> usize {
        self.list_register_count
    }

    fn validate(&self) -> VgicResult {
        if self.vcpu_affinities.is_empty() || self.vcpu_affinities.len() > 8 {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "GICv2 requires 1..=8 vCPUs, got {}",
                    self.vcpu_affinities.len()
                ),
            });
        }
        validate_unique_affinities(&self.vcpu_affinities)?;
        validate_common_capabilities(self.spi_count, self.list_register_count, self.priority_bits)?;
        validate_assigned_spis(
            &self.assigned_spis,
            self.spi_count,
            self.vcpu_affinities.len(),
        )
    }
}

/// Complete GICv3 configuration for one VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VgicV3Config {
    controller_id: InterruptControllerId,
    distributor: VgicMmioRegion,
    redistributors: Vec<VgicMmioRegion>,
    redistributor_stride: u64,
    vcpu_affinities: Vec<GicAffinity>,
    spi_count: usize,
    lpi_limit: u32,
    list_register_count: usize,
    priority_bits: u8,
    its: Vec<ItsConfig>,
    assigned_spis: Vec<AssignedSpiConfig>,
}

impl VgicV3Config {
    /// Creates a GICv3 configuration with architectural defaults.
    pub fn new(
        controller_id: InterruptControllerId,
        distributor: VgicMmioRegion,
        redistributors: Vec<VgicMmioRegion>,
        redistributor_stride: u64,
        vcpu_affinities: Vec<GicAffinity>,
    ) -> VgicResult<Self> {
        let config = Self {
            controller_id,
            distributor,
            redistributors,
            redistributor_stride,
            vcpu_affinities,
            spi_count: 988,
            lpi_limit: LPI_INTID_MAX,
            list_register_count: 16,
            priority_bits: 5,
            its: Vec::new(),
            assigned_spis: Vec::new(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Sets the implemented SPI count.
    pub fn with_spi_count(mut self, count: usize) -> VgicResult<Self> {
        self.spi_count = count;
        self.validate()?;
        Ok(self)
    }

    /// Sets the highest implemented LPI.
    pub fn with_lpi_limit(mut self, limit: u32) -> VgicResult<Self> {
        self.lpi_limit = limit;
        self.validate()?;
        Ok(self)
    }

    /// Sets the host LR count.
    pub fn with_list_register_count(mut self, count: usize) -> VgicResult<Self> {
        self.list_register_count = count;
        self.validate()?;
        Ok(self)
    }

    /// Sets the implemented priority width.
    pub fn with_priority_bits(mut self, bits: u8) -> VgicResult<Self> {
        self.priority_bits = bits;
        self.validate()?;
        Ok(self)
    }

    /// Adds guest-visible ITS instances discovered in host firmware.
    pub fn with_its(mut self, its: Vec<ItsConfig>) -> VgicResult<Self> {
        self.its = its;
        self.validate()?;
        Ok(self)
    }

    /// Adds prevalidated identity-backed SPIs.
    pub fn with_assigned_spis(mut self, assigned_spis: Vec<AssignedSpiConfig>) -> VgicResult<Self> {
        self.assigned_spis = assigned_spis;
        self.validate()?;
        Ok(self)
    }

    /// Returns the controller ID.
    pub const fn controller_id(&self) -> InterruptControllerId {
        self.controller_id
    }

    /// Returns the Distributor region.
    pub const fn distributor(&self) -> VgicMmioRegion {
        self.distributor
    }

    /// Returns all host-derived Redistributor regions.
    pub fn redistributors(&self) -> &[VgicMmioRegion] {
        &self.redistributors
    }

    /// Returns the Redistributor stride.
    pub const fn redistributor_stride(&self) -> u64 {
        self.redistributor_stride
    }

    /// Returns vCPU affinities in vCPU-ID order.
    pub fn vcpu_affinities(&self) -> &[GicAffinity] {
        &self.vcpu_affinities
    }

    /// Returns guest-visible ITS descriptors.
    pub fn its(&self) -> &[ItsConfig] {
        &self.its
    }

    /// Returns assigned physical SPIs.
    pub fn assigned_spis(&self) -> &[AssignedSpiConfig] {
        &self.assigned_spis
    }

    /// Returns the implemented priority width.
    pub const fn priority_bits(&self) -> u8 {
        self.priority_bits
    }

    /// Returns the implemented SPI count.
    pub const fn spi_count(&self) -> usize {
        self.spi_count
    }

    /// Returns the highest implemented LPI.
    pub const fn lpi_limit(&self) -> u32 {
        self.lpi_limit
    }

    /// Returns the configured LR count.
    pub const fn list_register_count(&self) -> usize {
        self.list_register_count
    }

    fn validate(&self) -> VgicResult {
        if self.vcpu_affinities.is_empty() {
            return Err(VgicError::InvalidConfig {
                detail: "GICv3 requires at least one vCPU".into(),
            });
        }
        validate_unique_affinities(&self.vcpu_affinities)?;
        validate_common_capabilities(self.spi_count, self.list_register_count, self.priority_bits)?;
        let frames = self
            .redistributors
            .iter()
            .try_fold(0usize, |count, region| {
                if !region.base().is_multiple_of(0x1_0000)
                    || !region.size().is_multiple_of(0x1_0000)
                {
                    return Err(VgicError::InvalidConfig {
                        detail: "GICv3 Redistributor regions must be 64-KiB aligned".into(),
                    });
                }
                Ok(count + (region.size() / self.redistributor_stride) as usize)
            })?;
        if frames < self.vcpu_affinities.len() {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "{} Redistributor frames are available for {} vCPUs",
                    frames,
                    self.vcpu_affinities.len()
                ),
            });
        }
        for (index, its) in self.its.iter().enumerate() {
            if self.its[..index]
                .iter()
                .any(|existing| existing.id() == its.id())
            {
                return Err(VgicError::InvalidConfig {
                    detail: alloc::format!("ITS ID {:?} is duplicated", its.id()),
                });
            }
        }
        validate_assigned_spis(
            &self.assigned_spis,
            self.spi_count,
            self.vcpu_affinities.len(),
        )?;
        self.internal_config().map(|_| ())
    }

    pub(crate) fn internal_config(&self) -> VgicResult<GicV3Config> {
        let mut internal = GicV3Config::new_with_redistributor_regions(
            GicV3SpiOwnership::AllGuestOwned,
            self.distributor,
            self.redistributors.clone(),
            self.redistributor_stride,
            self.vcpu_affinities.len(),
        )?
        .with_spi_count(self.spi_count)?
        .with_lpi_limit(self.lpi_limit)?
        .with_list_register_count(self.list_register_count)?;
        if !self.its.is_empty() {
            internal = internal.with_its_instances(
                self.its
                    .iter()
                    .map(|its| (its.id(), its.registers()))
                    .collect(),
            )?;
        }
        Ok(internal)
    }
}

/// Versioned immutable VGIC configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArmVgicConfig {
    /// GICv2 Distributor and memory-mapped CPU interface.
    V2(VgicV2Config),
    /// GICv3 Distributor, Redistributors, ICC interface, and optional ITS.
    V3(VgicV3Config),
}

impl ArmVgicConfig {
    /// Returns the controller ID.
    pub const fn controller_id(&self) -> InterruptControllerId {
        match self {
            Self::V2(config) => config.controller_id(),
            Self::V3(config) => config.controller_id(),
        }
    }

    /// Returns vCPU affinities in vCPU-ID order.
    pub fn vcpu_affinities(&self) -> &[GicAffinity] {
        match self {
            Self::V2(config) => config.vcpu_affinities(),
            Self::V3(config) => config.vcpu_affinities(),
        }
    }

    /// Returns assigned physical SPIs.
    pub fn assigned_spis(&self) -> &[AssignedSpiConfig] {
        match self {
            Self::V2(config) => config.assigned_spis(),
            Self::V3(config) => config.assigned_spis(),
        }
    }

    /// Returns the configured LR count.
    pub const fn list_register_count(&self) -> usize {
        match self {
            Self::V2(config) => config.list_register_count,
            Self::V3(config) => config.list_register_count,
        }
    }

    /// Returns the configured priority width.
    pub const fn priority_bits(&self) -> u8 {
        match self {
            Self::V2(config) => config.priority_bits,
            Self::V3(config) => config.priority_bits,
        }
    }

    pub(crate) fn internal_gicv3_config(&self) -> VgicResult<GicV3Config> {
        match self {
            Self::V3(config) => config.internal_config(),
            Self::V2(_) => Err(VgicError::Unsupported {
                operation: "construct GICv3 configuration",
                detail: "a GICv2 configuration has no GICv3 MMIO layout".into(),
            }),
        }
    }
}

fn validate_common_capabilities(
    spi_count: usize,
    list_register_count: usize,
    priority_bits: u8,
) -> VgicResult {
    if spi_count == 0
        || spi_count > 988
        || (spi_count != 988 && !(spi_count + 32).is_multiple_of(32))
    {
        return Err(VgicError::InvalidConfig {
            detail: alloc::format!("invalid implemented SPI count {spi_count}"),
        });
    }
    if !(1..=16).contains(&list_register_count) {
        return Err(VgicError::InvalidConfig {
            detail: alloc::format!("invalid list-register count {list_register_count}"),
        });
    }
    if !(4..=8).contains(&priority_bits) {
        return Err(VgicError::InvalidConfig {
            detail: alloc::format!("priority width {priority_bits} is outside 4..=8"),
        });
    }
    Ok(())
}

fn validate_unique_affinities(affinities: &[GicAffinity]) -> VgicResult {
    for (index, affinity) in affinities.iter().enumerate() {
        if affinities[..index].contains(affinity) {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!("vCPU affinity {affinity:?} is duplicated"),
            });
        }
    }
    Ok(())
}

fn validate_assigned_spis(
    assigned: &[AssignedSpiConfig],
    spi_count: usize,
    vcpu_count: usize,
) -> VgicResult {
    for (index, binding) in assigned.iter().enumerate() {
        if binding.intid().raw() >= 32 + spi_count as u32 {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "assigned SPI {} exceeds configured SPI capacity",
                    binding.intid().raw()
                ),
            });
        }
        if binding.target_vcpu() >= vcpu_count {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "assigned SPI {} targets missing vCPU {}",
                    binding.intid().raw(),
                    binding.target_vcpu()
                ),
            });
        }
        if assigned[..index].iter().any(|existing| {
            existing.intid() == binding.intid() || existing.host_irq() == binding.host_irq()
        }) {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "assigned SPI/host IRQ {} is duplicated",
                    binding.intid().raw()
                ),
            });
        }
    }
    Ok(())
}
