//! Guest-visible VGIC devices registered through the architecture-neutral bus.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use axdevice_base::{BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, Resource};

use crate::{ArmVgicConfig, GicVcpuId, ItsConfig, VgicCore, VgicMmioRegion, VgicResult};

/// Architecture-owned source of the vCPU issuing a trapped register access.
pub trait VgicAccessContext: Send + Sync {
    /// Returns the current VM-local vCPU identifier.
    fn current_vcpu(&self) -> Option<usize>;
}

/// Complete set of bus devices exposing one [`VgicCore`].
pub struct VgicDeviceSet {
    devices: Vec<Arc<dyn Device>>,
}

impl VgicDeviceSet {
    /// Creates all MMIO frontends from the same immutable controller config.
    pub fn new(
        core: Arc<VgicCore>,
        access_context: Arc<dyn VgicAccessContext>,
    ) -> VgicResult<Self> {
        let mut devices: Vec<Arc<dyn Device>> = Vec::new();
        match core.config() {
            ArmVgicConfig::V2(config) => {
                devices.push(Arc::new(VgicDistributorDevice::new(
                    core.clone(),
                    access_context.clone(),
                    config.distributor(),
                    DistributorVersion::V2,
                )));
                devices.push(Arc::new(VgicV2CpuInterfaceDevice::new(
                    core.clone(),
                    access_context,
                    config.cpu_interface(),
                )));
            }
            ArmVgicConfig::V3(config) => {
                devices.push(Arc::new(VgicDistributorDevice::new(
                    core.clone(),
                    access_context,
                    config.distributor(),
                    DistributorVersion::V3,
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

#[derive(Clone, Copy)]
enum DistributorVersion {
    V2,
    V3,
}

struct VgicDistributorDevice {
    core: Arc<VgicCore>,
    access_context: Arc<dyn VgicAccessContext>,
    region: VgicMmioRegion,
    resources: Box<[Resource]>,
    version: DistributorVersion,
}

impl VgicDistributorDevice {
    fn new(
        core: Arc<VgicCore>,
        access_context: Arc<dyn VgicAccessContext>,
        region: VgicMmioRegion,
        version: DistributorVersion,
    ) -> Self {
        Self {
            core,
            access_context,
            region,
            resources: mmio_resources(region),
            version,
        }
    }
}

impl Device for VgicDistributorDevice {
    fn name(&self) -> &str {
        "arm-vgic-distributor"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        let result = match (self.version, access.is_read) {
            (DistributorVersion::V2, true) => self
                .core
                .read_v2_distributor(current_vcpu(&*self.access_context)?, offset, access.width)
                .map(|value| BusResponse::Read { value }),
            (DistributorVersion::V2, false) => self
                .core
                .write_v2_distributor(
                    current_vcpu(&*self.access_context)?,
                    offset,
                    access.width,
                    access.data,
                )
                .map(|()| BusResponse::Write),
            (DistributorVersion::V3, true) => self
                .core
                .controller()
                .read_distributor(offset, access.width)
                .map(|value| BusResponse::Read { value }),
            (DistributorVersion::V3, false) => self
                .core
                .controller()
                .write_distributor(offset, access.width, access.data)
                .map(|()| BusResponse::Write),
        };
        result.map_err(Into::into)
    }
}

struct VgicV2CpuInterfaceDevice {
    core: Arc<VgicCore>,
    access_context: Arc<dyn VgicAccessContext>,
    region: VgicMmioRegion,
    resources: Box<[Resource]>,
}

impl VgicV2CpuInterfaceDevice {
    fn new(
        core: Arc<VgicCore>,
        access_context: Arc<dyn VgicAccessContext>,
        region: VgicMmioRegion,
    ) -> Self {
        Self {
            core,
            access_context,
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

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        let offset = mmio_offset(access, self.region)?;
        let vcpu = current_vcpu(&*self.access_context)?;
        if access.is_read {
            self.core
                .read_v2_cpu_interface(vcpu, offset, access.width)
                .map(|value| BusResponse::Read { value })
                .map_err(Into::into)
        } else {
            self.core
                .write_v2_cpu_interface(vcpu, offset, access.width, access.data)
                .map(|()| BusResponse::Write)
                .map_err(Into::into)
        }
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

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        let region_offset = mmio_offset(access, self.region)?;
        let vcpu = self.first_vcpu + (region_offset / self.stride) as usize;
        if vcpu >= self.vcpu_count {
            return Ok(if access.is_read {
                BusResponse::Read { value: 0 }
            } else {
                BusResponse::Write
            });
        }
        let frame_offset = region_offset % self.stride;
        if access.is_read {
            self.core
                .controller()
                .read_redistributor(GicVcpuId::new(vcpu), frame_offset, access.width)
                .map(|value| BusResponse::Read { value })
                .map_err(Into::into)
        } else {
            self.core
                .controller()
                .write_redistributor(
                    GicVcpuId::new(vcpu),
                    frame_offset,
                    access.width,
                    access.data,
                )
                .map(|()| BusResponse::Write)
                .map_err(Into::into)
        }
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

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        let offset = mmio_offset(access, self.config.registers())?;
        if access.is_read {
            self.core
                .controller()
                .read_its_for(self.config.id(), offset, access.width)
                .map(|value| BusResponse::Read { value })
                .map_err(Into::into)
        } else {
            self.core
                .controller()
                .write_its_for(self.config.id(), offset, access.width, access.data)
                .map(|()| BusResponse::Write)
                .map_err(Into::into)
        }
    }
}

fn mmio_resources(region: VgicMmioRegion) -> Box<[Resource]> {
    alloc::vec![Resource::MmioRange {
        base: region.base(),
        size: region.size(),
    }]
    .into_boxed_slice()
}

fn mmio_offset(access: &BusAccess, region: VgicMmioRegion) -> Result<u64, DeviceError> {
    if access.kind != BusKind::Mmio || !region.contains(access.addr, access.width.size()) {
        return Err(DeviceError::OutOfRange { addr: access.addr });
    }
    Ok(access.addr - region.base())
}

fn current_vcpu(context: &dyn VgicAccessContext) -> Result<GicVcpuId, DeviceError> {
    context
        .current_vcpu()
        .map(GicVcpuId::new)
        .ok_or_else(|| DeviceError::InvalidState {
            operation: "access per-vCPU VGIC register",
            detail: "no current vCPU is installed in the architecture runtime".into(),
        })
}
