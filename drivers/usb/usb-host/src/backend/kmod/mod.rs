use crate::{
    Mmio, USBHost,
    backend::kmod::hub::{Hub, HubInfo},
};

mod dwc;
mod dwc2;
mod ehci;
mod hub;
mod kcore;
pub mod osal;
pub(crate) mod queue;
mod transfer;
mod xhci;

use alloc::{boxed::Box, collections::btree_map::BTreeMap};

use dwc::Dwc;
pub use dwc::{
    DwcNewParams, DwcParams, NamedResetLine, ResetLine, UdphyParam, Usb2PhyParam,
    UsbPhyInterfaceMode, usb2phy::Usb2PhyPortId,
};
use dwc2::Dwc2;
pub use dwc2::{Dwc2FifoSizes, Dwc2HostParams, Dwc2NewParams, Dwc2Quirks, Dwc2UtmiWidth};
use ehci::Ehci;
pub use ehci::EhciNewParams;
use id_arena::Id;
use kcore::*;
pub use osal::*;
use usb_if::Speed;
use xhci::Xhci;

use crate::err::*;

impl USBHost {
    pub fn new_xhci(mmio: Mmio, kernel: &'static dyn KernelOp) -> Result<USBHost> {
        Ok(USBHost::new(Xhci::new(mmio, kernel)?))
    }

    pub fn new_dwc(params: DwcNewParams<'_>) -> Result<USBHost> {
        Ok(USBHost::new(Dwc::new(params)?))
    }

    pub fn new_dwc2(params: Dwc2NewParams) -> Result<USBHost> {
        Ok(USBHost::new(Dwc2::new(params)?))
    }

    pub fn new_ehci(params: EhciNewParams) -> Result<USBHost> {
        Ok(USBHost::new(Ehci::new(params)?))
    }

    pub(crate) fn new(backend: impl CoreOp) -> Self {
        let b = Core::new(backend);
        Self {
            backend: Box::new(b),
            initialized: false,
        }
    }
}

pub struct DeviceAddressInfo {
    pub root_port_id: u8,
    pub parent_hub: Option<Id<Hub>>,
    pub port_speed: Speed,
    pub port_id: u8,
    pub infos: BTreeMap<Id<Hub>, HubInfo>,
}
