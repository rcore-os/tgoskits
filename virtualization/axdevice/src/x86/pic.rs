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

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let (port, width) = decode_access(access)?;
        self.inner
            .handle_read(port, width)
            .map(|value| value as u64)
            .map_err(|_| DeviceError::Internal)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (port, width) = decode_access(access)?;
        self.inner
            .handle_write(port, width, value as usize)
            .map_err(|_| DeviceError::Internal)
    }
}

fn decode_access(access: &DeviceAccess) -> DeviceResult<(X86Port, x86_vlapic::X86AccessWidth)> {
    if access.bus() != BusKind::Port {
        return Err(DeviceError::OutOfRange {
            addr: access.address(),
        });
    }
    let port =
        X86Port::new(
            u16::try_from(access.address()).map_err(|_| DeviceError::OutOfRange {
                addr: access.address(),
            })?,
        );
    Ok((port, x86_access_width(access.width())))
}
