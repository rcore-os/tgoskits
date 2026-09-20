//! Guest-visible VGIC devices registered through the architecture-neutral bus.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use axdevice_base::{BusKind, Device, DeviceAccess, DeviceContext, DeviceError, Resource};

use crate::{ArmVgicConfig, GicVcpuId, ItsConfig, VgicCore, VgicMmioRegion, VgicResult};

/// Complete set of bus devices exposing one [`VgicCore`].
pub struct VgicDeviceSet {
    devices: Vec<Arc<dyn Device>>,
}

impl VgicDeviceSet {
    /// Creates all MMIO frontends from the same immutable controller config.
    pub fn new(core: Arc<VgicCore>) -> VgicResult<Self> {
        let mut devices: Vec<Arc<dyn Device>> = Vec::new();
        match core.config() {
            ArmVgicConfig::V2(config) => {
                devices.push(Arc::new(BankedDistributorDevice::new(
                    core.clone(),
                    config.distributor(),
                )));
                devices.push(Arc::new(VgicV2CpuInterfaceDevice::new(
                    core.clone(),
                    config.cpu_interface(),
                )));
            }
            ArmVgicConfig::V3(config) => {
                devices.push(Arc::new(SharedDistributorDevice::new(
                    core.clone(),
                    config.distributor(),
                )));
                let mut first_vcpu = 0;
                for (region_index, region) in config.redistributors().iter().copied().enumerate() {
                    let frame_count = (region.size() / config.redistributor_stride()) as usize;
                    devices.push(Arc::new(VgicRedistributorDevice::new(
                        core.clone(),
                        region,
                        config.redistributor_stride(),
                        first_vcpu,
                        config.vcpu_affinities().len(),
                        region_index,
                    )));
                    first_vcpu += frame_count;
                }
                for its in config.its() {
                    devices.push(Arc::new(VgicItsDevice::new(core.clone(), *its)));
                }
            }
        }
        Ok(Self { devices })
    }

    /// Returns every frontend in atomic-registration order.
    pub fn devices(&self) -> &[Arc<dyn Device>] {
        &self.devices
    }

    /// Consumes the set and returns the devices.
    pub fn into_devices(self) -> Vec<Arc<dyn Device>> {
        self.devices
    }
}

/// Frontend for a distributor whose SGI/PPI registers are banked by accessor.
struct BankedDistributorDevice {
    core: Arc<VgicCore>,
    region: VgicMmioRegion,
    resources: Box<[Resource]>,
}

impl BankedDistributorDevice {
    fn new(core: Arc<VgicCore>, region: VgicMmioRegion) -> Self {
        Self {
            core,
            region,
            resources: mmio_resources(region),
        }
    }
}

impl Device for BankedDistributorDevice {
    fn name(&self) -> &str {
        "arm-vgic-distributor"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        self.core
            .read_v2_distributor(source_vcpu(access), offset, access.width())
            .map_err(Into::into)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        self.core
            .write_v2_distributor(source_vcpu(access), offset, access.width(), value)
            .map_err(Into::into)
    }
}

/// Frontend for a distributor whose state is shared and routes SPIs by affinity.
struct SharedDistributorDevice {
    core: Arc<VgicCore>,
    region: VgicMmioRegion,
    resources: Box<[Resource]>,
}

impl SharedDistributorDevice {
    fn new(core: Arc<VgicCore>, region: VgicMmioRegion) -> Self {
        Self {
            core,
            region,
            resources: mmio_resources(region),
        }
    }
}

impl Device for SharedDistributorDevice {
    fn name(&self) -> &str {
        "arm-vgic-distributor"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        self.core
            .controller()
            .read_distributor(offset, access.width())
            .map_err(Into::into)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        self.core
            .controller()
            .write_distributor(offset, access.width(), value)
            .map_err(Into::into)
    }
}

struct VgicV2CpuInterfaceDevice {
    core: Arc<VgicCore>,
    region: VgicMmioRegion,
    resources: Box<[Resource]>,
}

impl VgicV2CpuInterfaceDevice {
    fn new(core: Arc<VgicCore>, region: VgicMmioRegion) -> Self {
        Self {
            core,
            region,
            resources: mmio_resources(region),
        }
    }
}

impl Device for VgicV2CpuInterfaceDevice {
    fn name(&self) -> &str {
        "arm-vgic-v2-cpu-interface"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        self.core
            .read_v2_cpu_interface(source_vcpu(access), offset, access.width())
            .map_err(Into::into)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        self.core
            .write_v2_cpu_interface(source_vcpu(access), offset, access.width(), value)
            .map_err(Into::into)
    }
}

struct VgicRedistributorDevice {
    core: Arc<VgicCore>,
    region: VgicMmioRegion,
    stride: u64,
    first_vcpu: usize,
    vcpu_count: usize,
    name: String,
    resources: Box<[Resource]>,
}

impl VgicRedistributorDevice {
    fn new(
        core: Arc<VgicCore>,
        region: VgicMmioRegion,
        stride: u64,
        first_vcpu: usize,
        vcpu_count: usize,
        region_index: usize,
    ) -> Self {
        Self {
            core,
            region,
            stride,
            first_vcpu,
            vcpu_count,
            name: alloc::format!("arm-vgic-redistributor-{region_index}"),
            resources: mmio_resources(region),
        }
    }
}

impl Device for VgicRedistributorDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        let region_offset = mmio_offset(access, self.region)?;
        let vcpu = self.first_vcpu + (region_offset / self.stride) as usize;
        if vcpu >= self.vcpu_count {
            return Ok(0);
        }
        let frame_offset = region_offset % self.stride;
        self.core
            .controller()
            .read_redistributor(GicVcpuId::new(vcpu), frame_offset, access.width())
            .map_err(Into::into)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        let region_offset = mmio_offset(access, self.region)?;
        let vcpu = self.first_vcpu + (region_offset / self.stride) as usize;
        if vcpu >= self.vcpu_count {
            return Ok(());
        }
        let frame_offset = region_offset % self.stride;
        self.core
            .controller()
            .write_redistributor(GicVcpuId::new(vcpu), frame_offset, access.width(), value)
            .map_err(Into::into)
    }
}

struct VgicItsDevice {
    core: Arc<VgicCore>,
    config: ItsConfig,
    name: String,
    resources: Box<[Resource]>,
}

impl VgicItsDevice {
    fn new(core: Arc<VgicCore>, config: ItsConfig) -> Self {
        Self {
            core,
            config,
            name: alloc::format!("arm-vgic-its-{}", config.id().value()),
            resources: mmio_resources(config.registers()),
        }
    }
}

impl Device for VgicItsDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(
        &self,
        access: &DeviceAccess,
        _context: &mut dyn DeviceContext,
    ) -> Result<u64, DeviceError> {
        let offset = mmio_offset(access, self.config.registers())?;
        self.core
            .controller()
            .read_its_for(self.config.id(), offset, access.width())
            .map_err(Into::into)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> Result<(), DeviceError> {
        let offset = mmio_offset(access, self.config.registers())?;
        self.core
            .controller()
            .write_its_for(self.config.id(), offset, access.width(), value)
            .map_err(Into::into)
    }
}

fn mmio_resources(region: VgicMmioRegion) -> Box<[Resource]> {
    alloc::vec![Resource::MmioRange {
        base: region.base(),
        size: region.size(),
    }]
    .into_boxed_slice()
}

fn mmio_offset(access: &DeviceAccess, region: VgicMmioRegion) -> Result<u64, DeviceError> {
    if access.bus() != BusKind::Mmio || !region.contains(access.address(), access.width().size()) {
        return Err(DeviceError::OutOfRange {
            addr: access.address(),
        });
    }
    Ok(access.address() - region.base())
}

const fn source_vcpu(access: &DeviceAccess) -> GicVcpuId {
    GicVcpuId::new(access.source_vcpu().as_usize())
}
