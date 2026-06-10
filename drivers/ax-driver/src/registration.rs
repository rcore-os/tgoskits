#[cfg(any(feature = "display", feature = "input", feature = "vsock"))]
use rdrive::Device;
use rdrive::DriverGeneric;

use crate::BindingInfo;

pub trait BoundDevice: DriverGeneric {
    fn binding_info(&self) -> &BindingInfo;

    fn irq_num(&self) -> Option<usize> {
        self.binding_info().irq_num()
    }
}

pub fn register_bound_device<T>(plat_dev: rdrive::PlatformDevice, device: T) -> Option<usize>
where
    T: BoundDevice,
{
    let irq = device.irq_num();
    plat_dev.register(device);
    irq
}

#[cfg(any(feature = "display", feature = "input", feature = "vsock"))]
pub trait TakeRegistered {
    type Output;

    fn take_registered(&mut self) -> Option<Self::Output>;
}

#[cfg(any(feature = "display", feature = "input", feature = "vsock"))]
pub fn take_registered_device<T>(device: Device<T>) -> Option<T::Output>
where
    T: TakeRegistered + 'static,
{
    let mut device = device.lock().ok()?;
    device.take_registered()
}
