use alloc::{collections::btree_set::BTreeSet, vec::Vec};
use core::ops::{Deref, DerefMut};

use ::pcie::*;
pub use ::pcie::{Endpoint, PciCapability, PciIntxRoute, PcieGeneric};
use ax_kspin::SpinRaw as Mutex;
use mmio_api::{MapError, MmioOp};
pub use rdif_pcie::{DriverGeneric, PciAddress, PciMem32, PciMem64, PcieController};
use spin::Once;

use crate::{
    Descriptor, Device, PlatformDevice, ProbeError, get_list,
    probe::OnProbeError,
    register::{DriverRegister, ProbeKind},
};

static PCIE: Once<Mutex<Vec<PcieEnumterator>>> = Once::new();

pub type FnOnProbe = fn(ProbePci<'_>) -> Result<(), OnProbeError>;

pub fn new_driver_generic(
    mmio_base: usize,
    mmio_size: usize,
    mmio_op: &'static dyn MmioOp,
) -> Result<PcieController, MapError> {
    Ok(PcieController::new(PcieGeneric::new(
        mmio_base, mmio_size, mmio_op,
    )?))
}

fn pcie() -> &'static Mutex<Vec<PcieEnumterator>> {
    PCIE.call_once(|| {
        let ctrl_ls = get_list::<PcieController>();
        let mut vec = Vec::new();
        for ctrl in ctrl_ls.into_iter() {
            vec.push(PcieEnumterator {
                ctrl,
                probed: BTreeSet::new(),
            });
        }
        Mutex::new(vec)
    })
}
pub(crate) fn probe_with(
    registers: &[DriverRegister],
    stop_if_fail: bool,
) -> Result<(), ProbeError> {
    let mut pcie_ls = pcie().lock();
    for ctrl in pcie_ls.iter_mut() {
        ctrl.probe(registers, stop_if_fail)?;
    }
    Ok(())
}

pub struct EndpointRc(Option<Endpoint>);

impl EndpointRc {
    fn new(ep: Endpoint) -> Self {
        Self(Some(ep))
    }

    pub fn take(&mut self) -> Endpoint {
        self.0.take().unwrap()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciInfo {
    pub address: PciAddress,
    pub interrupt_pin: u8,
    pub interrupt_line: u8,
    pub intx_route: Option<PciIntxRoute>,
}

impl PciInfo {
    fn from_endpoint(endpoint: &EndpointRc, intx_route: Option<PciIntxRoute>) -> Self {
        Self {
            address: endpoint.address(),
            interrupt_pin: endpoint.interrupt_pin(),
            interrupt_line: endpoint.interrupt_line(),
            intx_route,
        }
    }
}

pub struct ProbePci<'a> {
    info: PciInfo,
    endpoint: &'a mut EndpointRc,
    platform: PlatformDevice,
}

impl<'a> ProbePci<'a> {
    pub(crate) fn new(
        info: PciInfo,
        endpoint: &'a mut EndpointRc,
        platform: PlatformDevice,
    ) -> Self {
        Self {
            info,
            endpoint,
            platform,
        }
    }

    pub const fn info(&self) -> PciInfo {
        self.info
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.endpoint
    }

    pub fn endpoint_mut(&mut self) -> &mut EndpointRc {
        self.endpoint
    }

    pub fn take_endpoint(&mut self) -> Endpoint {
        self.endpoint.take()
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (PciInfo, &'a mut EndpointRc, PlatformDevice) {
        (self.info, self.endpoint, self.platform)
    }
}

impl Deref for EndpointRc {
    type Target = Endpoint;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl DerefMut for EndpointRc {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}

struct PcieEnumterator {
    ctrl: Device<PcieController>,
    probed: BTreeSet<PciAddress>,
}

impl PcieEnumterator {
    fn probe(
        &mut self,
        registers: &[DriverRegister],
        stop_if_fail: bool,
    ) -> Result<(), ProbeError> {
        let mut g = self.ctrl.lock().unwrap();

        for ep in enumerate_by_controller_with_info(&mut g, None) {
            debug!("PCIe endpiont: {}", ep.endpoint);
            match self.probe_one(ep, registers, stop_if_fail) {
                Ok(_) => {} // Successfully probed, move to the next
                Err(e) => {
                    if stop_if_fail {
                        return Err(e);
                    } else {
                        warn!("Probe failed: {e}");
                    }
                }
            }
        }

        Ok(())
    }

    fn probe_one(
        &mut self,
        endpoint: EnumeratedEndpoint,
        registers: &[DriverRegister],
        stop_if_fail: bool,
    ) -> Result<(), ProbeError> {
        let intx_route = endpoint.intx_route;
        let endpoint = endpoint.endpoint;
        let address = endpoint.address();
        if self.probed.contains(&address) {
            return Ok(());
        }

        let mut endpoint = EndpointRc::new(endpoint);

        for register in registers {
            let Some(pci_probe) = register.probe_kinds.iter().find_map(|probe| {
                if let ProbeKind::Pci { on_probe } = probe {
                    Some(on_probe)
                } else {
                    None
                }
            }) else {
                continue;
            };
            let mut desc = Descriptor::new();
            desc.name = register.name;
            desc.irq_parent = self.ctrl.descriptor().irq_parent;

            let info = PciInfo::from_endpoint(&endpoint, intx_route);
            let plat_dev = PlatformDevice::new(desc);
            match (pci_probe)(ProbePci::new(info, &mut endpoint, plat_dev)) {
                Ok(_) => {
                    self.probed.insert(address);
                    return Ok(());
                }
                Err(e) => match e {
                    OnProbeError::NotMatch => continue,
                    e => {
                        if stop_if_fail {
                            return Err(ProbeError::from(e));
                        }
                        warn!("Probe failed: {e}");
                    }
                },
            }
        }

        Ok(())
    }
}
