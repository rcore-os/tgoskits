use alloc::{collections::btree_set::BTreeSet, vec::Vec};

use spin::{Mutex, Once};

use crate::{
    Descriptor, DeviceId, PlatformDevice,
    error::DriverError,
    probe::{OnProbeError, ProbeError},
    register::{DriverRegister, ProbeKind},
};

static SYSTEM: Once<System> = Once::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticDeviceDesc {
    pub name: &'static str,
    pub irq_parent: Option<DeviceId>,
}

impl StaticDeviceDesc {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            irq_parent: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticInfo {
    desc: StaticDeviceDesc,
}

impl StaticInfo {
    pub const fn desc(&self) -> StaticDeviceDesc {
        self.desc
    }

    pub const fn name(&self) -> &'static str {
        self.desc.name
    }

    pub const fn irq_parent(&self) -> Option<DeviceId> {
        self.desc.irq_parent
    }
}

pub type FnOnProbe = fn(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError>;

pub fn init(devices: &'static [StaticDeviceDesc]) -> Result<(), DriverError> {
    SYSTEM.call_once(|| System::new(devices));
    Ok(())
}

pub(crate) fn try_probe_register(
    register: &DriverRegister,
) -> Option<Result<Vec<Result<(), OnProbeError>>, ProbeError>> {
    SYSTEM.get().map(|system| system.probe_register(register))
}

struct System {
    devices: &'static [StaticDeviceDesc],
    probed_names: Mutex<BTreeSet<(&'static str, &'static str)>>,
}

impl System {
    fn new(devices: &'static [StaticDeviceDesc]) -> Self {
        Self {
            devices,
            probed_names: Mutex::new(BTreeSet::new()),
        }
    }
}

impl System {
    fn probe_register(
        &self,
        register: &DriverRegister,
    ) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
        let mut out = Vec::new();
        for probe in register.probe_kinds {
            let ProbeKind::Static { on_probe } = probe else {
                continue;
            };

            for device in self.devices {
                let probed_key = (register.name, device.name);
                if self.probed_names.lock().contains(&probed_key) {
                    continue;
                }

                let mut descriptor = Descriptor::new();
                descriptor.name = device.name;
                descriptor.irq_parent = device.irq_parent;
                let info = StaticInfo { desc: *device };
                let res = on_probe(info, PlatformDevice::new(descriptor));

                if res.is_ok() {
                    self.probed_names.lock().insert(probed_key);
                }

                out.push(res);
            }
        }

        Ok(out)
    }
}
