use alloc::{boxed::Box, string::String, vec::Vec};

use dma_api::DeviceDma;
use rdif_display::GpuDisplay;
use rdif_gpu::GpuDevice;
use rdrive::DriverGeneric;

use crate::{
    BindingInfo, BindingIrq, Error,
    registration::{BoundDevice, TakeRegistered, register_bound_device, take_registered_device},
};

/// Exactly one registered owner of a GPU and its optional scanout capability.
pub enum RegisteredGpuDevice {
    GpuOnly(Box<dyn GpuDevice>),
    WithDisplay(Box<dyn GpuDisplay>),
}

impl RegisteredGpuDevice {
    fn name(&self) -> &str {
        match self {
            Self::GpuOnly(device) => device.name(),
            Self::WithDisplay(device) => device.name(),
        }
    }
}

pub struct PlatformGpuDevice {
    name: String,
    info: BindingInfo,
    device: Option<RegisteredGpuDevice>,
    dma: Option<DeviceDma>,
}

impl PlatformGpuDevice {
    fn new(device: RegisteredGpuDevice, dma: DeviceDma, info: BindingInfo) -> Self {
        Self {
            name: device.name().into(),
            info,
            device: Some(device),
            dma: Some(dma),
        }
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }

    pub fn irq_cloned(&self) -> Option<BindingIrq> {
        self.info.irq_cloned()
    }
}

impl DriverGeneric for PlatformGpuDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformGpuDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

pub struct TakenGpuDevice {
    pub device: RegisteredGpuDevice,
    pub dma: DeviceDma,
    pub irq: Option<BindingIrq>,
}

impl TakeRegistered for PlatformGpuDevice {
    type Output = TakenGpuDevice;

    fn take_registered(&mut self) -> Option<Self::Output> {
        Some(TakenGpuDevice {
            device: self.device.take()?,
            dma: self.dma.take()?,
            irq: self.info.irq_cloned(),
        })
    }
}

pub trait PlatformDeviceGpu {
    fn register_gpu_with_info<T>(
        self,
        device: T,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: GpuDevice + 'static;

    fn register_gpu_display_with_info<T>(
        self,
        device: T,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: GpuDisplay + 'static;
}

impl PlatformDeviceGpu for rdrive::PlatformDevice {
    fn register_gpu_with_info<T>(
        self,
        device: T,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: GpuDevice + 'static,
    {
        register_gpu_with_info(
            self,
            RegisteredGpuDevice::GpuOnly(Box::new(device)),
            dma,
            info,
        )
    }

    fn register_gpu_display_with_info<T>(
        self,
        device: T,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: GpuDisplay + 'static,
    {
        register_gpu_with_info(
            self,
            RegisteredGpuDevice::WithDisplay(Box::new(device)),
            dma,
            info,
        )
    }
}

fn register_gpu_with_info(
    platform: rdrive::PlatformDevice,
    device: RegisteredGpuDevice,
    dma: DeviceDma,
    info: BindingInfo,
) -> Option<usize> {
    register_bound_device(platform, PlatformGpuDevice::new(device, dma, info))
}

pub fn take_gpu_devices() -> crate::Result<Vec<TakenGpuDevice>> {
    let mut devices = Vec::new();
    for device in rdrive::get_list::<PlatformGpuDevice>() {
        devices.push(take_registered_device(device).ok_or(Error::DeviceUnavailable)?);
    }
    Ok(devices)
}
