//! Checked MMIO devices for GICv3 Distributor, Redistributor, and ITS frames.

use alloc::{sync::Arc, vec, vec::Vec};
use core::any::Any;

use arm_vgic::{GicV3Controller, GicV3MmioRegion, GicVcpuId};
use axdevice_base::{BusAccess, BusKind, BusResponse, Device, DeviceError, Resource};

use super::error::device_error;

pub(super) fn configured_mmio_devices(controller: Arc<GicV3Controller>) -> Vec<Arc<dyn Device>> {
    let config = controller.config();
    let mut devices: Vec<Arc<dyn Device>> = vec![
        Arc::new(GicV3MmioDevice::new(
            controller.clone(),
            GicV3Frame::Distributor(config.distributor()),
        )),
        Arc::new(GicV3MmioDevice::new(
            controller.clone(),
            GicV3Frame::Redistributors(config.redistributors()),
        )),
    ];
    if let Some(region) = config.its() {
        devices.push(Arc::new(GicV3MmioDevice::new(
            controller,
            GicV3Frame::Its(region),
        )));
    }
    devices
}

#[derive(Clone, Copy)]
enum GicV3Frame {
    Distributor(GicV3MmioRegion),
    Redistributors(GicV3MmioRegion),
    Its(GicV3MmioRegion),
}

impl GicV3Frame {
    const fn region(self) -> GicV3MmioRegion {
        match self {
            Self::Distributor(region) | Self::Redistributors(region) | Self::Its(region) => region,
        }
    }
}

struct GicV3MmioDevice {
    controller: Arc<GicV3Controller>,
    frame: GicV3Frame,
    resources: Vec<Resource>,
}

impl GicV3MmioDevice {
    fn new(controller: Arc<GicV3Controller>, frame: GicV3Frame) -> Self {
        let region = frame.region();
        Self {
            controller,
            frame,
            resources: vec![Resource::MmioRange {
                base: region.base(),
                size: region.size(),
            }],
        }
    }

    fn handle_mmio(&self, access: &BusAccess, offset: u64) -> Result<BusResponse, DeviceError> {
        if access.is_read {
            let value = match self.frame {
                GicV3Frame::Distributor(_) => {
                    self.controller.read_distributor(offset, access.width)
                }
                GicV3Frame::Redistributors(_) => {
                    let stride = self.controller.config().redistributor_stride();
                    self.controller.read_redistributor(
                        GicVcpuId::new((offset / stride) as usize),
                        offset % stride,
                        access.width,
                    )
                }
                GicV3Frame::Its(_) => self.controller.read_its(offset, access.width),
            }
            .map_err(device_error)?;
            Ok(BusResponse::Read { value })
        } else {
            match self.frame {
                GicV3Frame::Distributor(_) => {
                    self.controller
                        .write_distributor(offset, access.width, access.data)
                }
                GicV3Frame::Redistributors(_) => {
                    let stride = self.controller.config().redistributor_stride();
                    self.controller.write_redistributor(
                        GicVcpuId::new((offset / stride) as usize),
                        offset % stride,
                        access.width,
                        access.data,
                    )
                }
                GicV3Frame::Its(_) => self.controller.write_its(offset, access.width, access.data),
            }
            .map_err(device_error)?;
            Ok(BusResponse::Write)
        }
    }
}

impl Device for GicV3MmioDevice {
    fn name(&self) -> &str {
        match self.frame {
            GicV3Frame::Distributor(_) => "gicv3-distributor",
            GicV3Frame::Redistributors(_) => "gicv3-redistributors",
            GicV3Frame::Its(_) => "gicv3-its",
        }
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::InvalidInput {
                operation: "access GICv3 device",
                detail: alloc::format!("expected MMIO access, got {:?}", access.kind),
            });
        }
        let region = self.frame.region();
        let offset = access
            .addr
            .checked_sub(region.base())
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        if offset
            .checked_add(access.width.size() as u64)
            .is_none_or(|end| end > region.size())
        {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        self.handle_mmio(access, offset)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
