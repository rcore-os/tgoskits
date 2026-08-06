//! Unified-device adapter for the guest-owned dual 8259 PIC.

use alloc::{boxed::Box, string::String};

use axdevice_base::*;
use x86_vlapic::{EmulatedPic, X86Port};

use super::{X86PicDeviceOps, port_resource, x86_access_width};

/// Device-runtime adapter for the OS-neutral legacy PIC core.
pub struct X86PicDevice {
    inner: EmulatedPic,
    name: String,
    resources: Box<[Resource]>,
}

impl X86PicDevice {
    /// Creates a guest-owned master/slave PIC pair.
    pub fn new() -> Self {
        Self {
            inner: EmulatedPic::new(),
            name: String::from("x86-pic"),
            resources: EmulatedPic::port_ranges()
                .map(port_resource)
                .to_vec()
                .into_boxed_slice(),
        }
    }
}

impl Default for X86PicDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl X86PicDeviceOps for X86PicDevice {
    fn pulse_irq(&self, irq: u8) -> Option<u8> {
        self.inner.pulse_irq(irq)
    }
}

impl Device for X86PicDevice {
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
        if access.kind != BusKind::Port {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let port = X86Port::new(
            u16::try_from(access.addr)
                .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?,
        );
        let width = x86_access_width(access.width);
        if access.is_read {
            self.inner
                .handle_read(port, width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(port, width, access.data as usize)
                .map(|()| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }
}
