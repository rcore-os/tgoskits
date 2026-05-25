use alloc::{collections::btree_set::BTreeSet, vec::Vec};

use spin::{Mutex, Once};

use crate::{
    Descriptor, DeviceId, PlatformDevice,
    error::DriverError,
    probe::{
        OnProbeError, ProbeError,
        pci::{PciMem32, PciMem64},
    },
    register::{DriverRegister, ProbeKind},
};

static SYSTEM: Once<System> = Once::new();

pub type StaticMmioRegion = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPciEcam {
    pub base: usize,
    pub size: usize,
    pub ranges: &'static [StaticMmioRegion],
    pub mem32: Option<PciMem32>,
    pub mem64: Option<PciMem64>,
}

impl StaticPciEcam {
    pub const fn new(base: usize, size: usize) -> Self {
        Self {
            base,
            size,
            ranges: &[],
            mem32: None,
            mem64: None,
        }
    }

    pub const fn with_ranges(mut self, ranges: &'static [StaticMmioRegion]) -> Self {
        self.ranges = ranges;
        self
    }

    pub const fn with_mem32(mut self, mem32: Option<PciMem32>) -> Self {
        self.mem32 = mem32;
        self
    }

    pub const fn with_mem64(mut self, mem64: Option<PciMem64>) -> Self {
        self.mem64 = mem64;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticDeviceDesc {
    pub name: &'static str,
    pub irq_parent: Option<DeviceId>,
    pub regs: &'static [StaticMmioRegion],
    pub irqs: &'static [usize],
    pub pci_ecam: Option<StaticPciEcam>,
    pub probe_each_reg: bool,
}

impl StaticDeviceDesc {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            irq_parent: None,
            regs: &[],
            irqs: &[],
            pci_ecam: None,
            probe_each_reg: false,
        }
    }

    pub const fn with_regs(mut self, regs: &'static [StaticMmioRegion]) -> Self {
        self.regs = regs;
        self
    }

    pub const fn with_irqs(mut self, irqs: &'static [usize]) -> Self {
        self.irqs = irqs;
        self
    }

    pub const fn with_irq_parent(mut self, irq_parent: Option<DeviceId>) -> Self {
        self.irq_parent = irq_parent;
        self
    }

    pub const fn with_pci_ecam(mut self, pci_ecam: StaticPciEcam) -> Self {
        self.pci_ecam = Some(pci_ecam);
        self
    }

    pub const fn with_probe_each_reg(mut self) -> Self {
        self.probe_each_reg = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticInfo {
    index: usize,
    resource_index: Option<usize>,
    desc: StaticDeviceDesc,
}

impl StaticInfo {
    pub const fn index(&self) -> usize {
        self.index
    }

    pub const fn resource_index(&self) -> Option<usize> {
        self.resource_index
    }

    pub const fn desc(&self) -> StaticDeviceDesc {
        self.desc
    }

    pub const fn name(&self) -> &'static str {
        self.desc.name
    }

    pub const fn irq_parent(&self) -> Option<DeviceId> {
        self.desc.irq_parent
    }

    pub const fn regs(&self) -> &'static [StaticMmioRegion] {
        self.desc.regs
    }

    pub const fn irqs(&self) -> &'static [usize] {
        self.desc.irqs
    }

    pub const fn pci_ecam(&self) -> Option<StaticPciEcam> {
        self.desc.pci_ecam
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
    probed_names: Mutex<BTreeSet<(&'static str, usize, Option<usize>)>>,
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
            let on_probe = *on_probe;

            for (index, device) in self.devices.iter().enumerate() {
                if device.probe_each_reg && !device.regs.is_empty() {
                    for resource_index in 0..device.regs.len() {
                        out.push(self.probe_one(
                            register,
                            on_probe,
                            index,
                            Some(resource_index),
                            *device,
                        ));
                    }
                } else {
                    out.push(self.probe_one(register, on_probe, index, None, *device));
                }
            }
        }

        Ok(out)
    }

    fn probe_one(
        &self,
        register: &DriverRegister,
        on_probe: FnOnProbe,
        index: usize,
        resource_index: Option<usize>,
        device: StaticDeviceDesc,
    ) -> Result<(), OnProbeError> {
        let probed_key = (register.name, index, resource_index);
        if self.probed_names.lock().contains(&probed_key) {
            return Err(OnProbeError::NotMatch);
        }

        let mut descriptor = Descriptor::new();
        descriptor.name = device.name;
        descriptor.irq_parent = device.irq_parent;
        let info = StaticInfo {
            index,
            resource_index,
            desc: device,
        };
        let res = on_probe(info, PlatformDevice::new(descriptor));

        if res.is_ok() {
            self.probed_names.lock().insert(probed_key);
        }

        res
    }
}
