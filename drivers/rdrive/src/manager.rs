use alloc::{collections::btree_map::BTreeMap, vec::Vec};

use rdif_base::DriverGeneric;

use crate::{
    Descriptor, Device, DeviceId, DeviceOwner, GetDeviceError,
    error::DriverError,
    probe::ProbeError,
    register::{DriverRegister, RegisterContainer},
};

pub struct Manager {
    pub registers: RegisterContainer,
    pub(crate) dev_container: DeviceContainer,
    // pub(crate) enum_system: EnumSystem,
}

impl Manager {
    pub fn new() -> Result<Self, DriverError> {
        Ok(Self {
            // enum_system: EnumSystem::new(platform)?,
            registers: RegisterContainer::default(),
            dev_container: DeviceContainer::default(),
        })
    }

    pub fn unregistered(&mut self) -> Result<Vec<DriverRegister>, ProbeError> {
        let mut out = self.registers.unregistered();
        out.sort_by_key(|a| (a.level, a.priority));
        Ok(out)
    }
}

#[derive(Default)]
pub(crate) struct DeviceContainer {
    devices: BTreeMap<DeviceId, Vec<DeviceOwner>>,
}

impl DeviceContainer {
    pub fn insert<T: DriverGeneric>(&mut self, descriptor: Descriptor, device: T) {
        let device_id = descriptor.device_id;
        let devices = self.devices.entry(device_id).or_default();
        if devices.iter().any(DeviceOwner::is::<T>) {
            panic!(
                "duplicate device interface {} for device {:?}",
                core::any::type_name::<T>(),
                device_id
            );
        }
        devices.push(DeviceOwner::new(descriptor, device));
    }

    pub fn get_typed<T: DriverGeneric>(&self, id: DeviceId) -> Result<Device<T>, GetDeviceError> {
        let devices = self.devices.get(&id).ok_or(GetDeviceError::NotFound)?;
        for dev in devices {
            if let Ok(device) = dev.weak() {
                return Ok(device);
            }
        }
        Err(GetDeviceError::TypeNotMatch)
    }

    pub fn get_one<T: DriverGeneric>(&self) -> Option<Device<T>> {
        for dev in self.devices.values().flatten() {
            if let Ok(val) = dev.weak::<T>() {
                return Some(val);
            }
        }
        None
    }

    pub fn devices<T: DriverGeneric>(&self) -> Vec<Device<T>> {
        let mut result = Vec::new();
        for dev in self.devices.values().flatten() {
            if let Ok(val) = dev.weak::<T>() {
                result.push(val);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::driver::{DriverGeneric, Empty};

    struct DeviceTest;

    impl DriverGeneric for DeviceTest {
        fn name(&self) -> &str {
            "DeviceTest"
        }
    }

    #[test]
    fn test_device_container() {
        let mut container = DeviceContainer::default();
        let desc = Descriptor::new();
        let id = desc.device_id;
        container.insert(desc, Empty);
        let weak = container.get_typed::<Empty>(id).unwrap();

        {
            let device = weak.lock().unwrap();
            assert_eq!(device.name(), "Empty Driver");
        }

        {
            let device = weak.lock().unwrap();
            assert_eq!(device.name(), "Empty Driver");
        }
    }
    #[test]
    fn test_get_one() {
        let mut container = DeviceContainer::default();
        container.insert(Descriptor::new(), Empty);
        container.insert(Descriptor::new(), DeviceTest);

        let weak = container.get_one::<Empty>().unwrap();
        {
            let device = weak.lock().unwrap();
            assert_eq!(device.name(), "Empty Driver");
        }
    }

    #[test]
    fn test_devices() {
        let mut container = DeviceContainer::default();
        container.insert(Descriptor::new(), Empty);
        container.insert(Descriptor::new(), Empty);
        container.insert(Descriptor::new(), DeviceTest);
        let devices = container.devices::<Empty>();
        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn same_device_id_can_expose_multiple_outer_driver_types() {
        let mut container = DeviceContainer::default();
        let desc = Descriptor::new();
        let id = desc.device_id;

        container.insert(desc.clone(), Empty);
        container.insert(desc, DeviceTest);

        assert!(container.get_typed::<Empty>(id).is_ok());
        assert!(container.get_typed::<DeviceTest>(id).is_ok());
    }

    #[test]
    #[should_panic(expected = "duplicate device interface")]
    fn same_device_id_rejects_duplicate_outer_driver_type() {
        let mut container = DeviceContainer::default();
        let desc = Descriptor::new();

        container.insert(desc.clone(), Empty);
        container.insert(desc, Empty);
    }

    #[test]
    fn test_not_found() {
        let container = DeviceContainer::default();
        let dev = container.get_one::<TestDevice>();
        assert!(dev.is_none(), "Expected no devices found");
    }

    trait TestInterface: DriverGeneric {
        fn is_ok(&mut self) -> bool;
    }

    struct TestDevice(Box<dyn TestInterface>);

    impl TestDevice {
        fn new<T: TestInterface>(driver: T) -> Self {
            Self(Box::new(driver))
        }

        fn typed_ref<T: TestInterface>(&self) -> Option<&T> {
            self.raw_any()?.downcast_ref()
        }
    }

    impl DriverGeneric for TestDevice {
        fn name(&self) -> &str {
            self.0.name()
        }

        fn raw_any(&self) -> Option<&dyn core::any::Any> {
            Some(self.0.as_ref() as &dyn core::any::Any)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
            Some(self.0.as_mut() as &mut dyn core::any::Any)
        }
    }

    struct IrqTest;

    impl TestInterface for IrqTest {
        fn is_ok(&mut self) -> bool {
            true
        }
    }

    impl crate::DriverGeneric for IrqTest {
        fn name(&self) -> &str {
            "IrqTest"
        }
    }

    #[test]
    fn test_inner_type() {
        let mut container = DeviceContainer::default();
        let desc = Descriptor::new();
        container.insert(desc, TestDevice::new(IrqTest));

        let weak = container.get_one::<TestDevice>().unwrap();
        {
            let device = weak.lock().unwrap();
            let intc = device.typed_ref::<IrqTest>();
            assert!(intc.is_some(), "Expected to find IrqTest device");
        }
    }

    #[test]
    fn test_device_downcast() {
        let mut container = DeviceContainer::default();
        let desc = Descriptor::new();
        container.insert(desc, TestDevice::new(IrqTest));

        let weak = container.get_one::<TestDevice>().unwrap();
        let intc_typed = weak.downcast::<IrqTest>().unwrap();
        let mut device = intc_typed.lock().unwrap();
        assert!(device.is_ok(), "Expected device to be ok");
    }

    #[test]
    fn test_locked_device() {
        let mut container = DeviceContainer::default();
        let desc = Descriptor::new();
        let id = desc.device_id;
        container.insert(desc, Empty);

        let weak = container.get_typed::<Empty>(id).unwrap();
        let device = weak.lock().unwrap();
        let r = weak.try_lock();
        assert!(
            r.is_err(),
            "Expected error when trying to lock an already locked device"
        );
        let _ = device;
    }
}
