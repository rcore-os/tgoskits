//! Native x86 host I/O port passthrough devices.

use std::{boxed::Box, sync::Arc};

use axdevice::*;
use axdevice_base::*;
use axvm_types::HostPortAssignment;

use crate::{AxVmResult, ax_err};

/// A host x86 I/O port range passed directly through to a guest.
pub(crate) struct HostPortPassthrough {
    base: Port,
    length: u16,
    resources: Box<[Resource]>,
}

impl HostPortPassthrough {
    /// Creates a passthrough device for an inclusive host I/O port range.
    pub(crate) fn new(base: u16, length: u16) -> AxVmResult<Self> {
        if length == 0 {
            return ax_err!(InvalidInput, "host port passthrough range is empty");
        }
        if base.checked_add(length - 1).is_none() {
            return ax_err!(InvalidInput, "host port passthrough range overflows");
        }
        Ok(Self {
            base: Port::new(base),
            length,
            resources: std::vec![Resource::PortRange { base, size: length }].into_boxed_slice(),
        })
    }

    fn end(&self) -> Port {
        Port::new(self.base.number() + self.length - 1)
    }

    fn contains(&self, port: Port) -> bool {
        (self.base.number()..=self.end().number()).contains(&port.number())
    }

    fn read_port(&self, port: Port, width: AccessWidth) -> DeviceResult<usize> {
        if !self.contains(port) {
            return Err(DeviceError::OutOfRange {
                addr: port.number() as u64,
            });
        }
        match width {
            AccessWidth::Byte => Ok(unsafe { inb(port.number()) } as usize),
            AccessWidth::Word => Ok(unsafe { inw(port.number()) } as usize),
            AccessWidth::Dword => Ok(unsafe { inl(port.number()) } as usize),
            AccessWidth::Qword => Err(DeviceError::Unsupported {
                operation: "read host I/O port",
                detail: "x86 port I/O does not support 64-bit accesses".into(),
            }),
        }
    }

    fn write_port(&self, port: Port, width: AccessWidth, value: usize) -> DeviceResult {
        if !self.contains(port) {
            return Err(DeviceError::OutOfRange {
                addr: port.number() as u64,
            });
        }
        match width {
            AccessWidth::Byte => unsafe { outb(port.number(), value as u8) },
            AccessWidth::Word => unsafe { outw(port.number(), value as u16) },
            AccessWidth::Dword => unsafe { outl(port.number(), value as u32) },
            AccessWidth::Qword => {
                return Err(DeviceError::Unsupported {
                    operation: "write host I/O port",
                    detail: "x86 port I/O does not support 64-bit accesses".into(),
                });
            }
        }
        Ok(())
    }
}

/// Builds the atomic device contribution for one configured host port range.
pub(crate) struct HostPortPassthroughFactory {
    config: HostPortAssignment,
}

impl HostPortPassthroughFactory {
    /// Creates a factory for one validated-at-build-time port range.
    pub(crate) const fn new(config: HostPortAssignment) -> Self {
        Self { config }
    }

    /// Creates the port device contribution.
    pub(crate) fn build(&self) -> AxVmResult<DeviceBundle> {
        let passthrough = Arc::new(HostPortPassthrough::new(
            self.config.base,
            self.config.length,
        )?);
        let device: Arc<dyn Device> = passthrough;
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            device,
        )))
    }
}

/// Factory registry entry for planner-generated x86 host-port passthrough
/// device configs.
pub(crate) struct HostPortPassthroughDeviceModel {
    base: u16,
    length: u16,
}

impl HostPortPassthroughDeviceModel {
    pub(crate) const fn new(base: u16, length: u16) -> Self {
        Self { base, length }
    }
}

impl DeviceModel for HostPortPassthroughDeviceModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        HostPortPassthrough::new(self.base, self.length).map_err(|error| {
            DeviceManagerError::InvalidConfig {
                operation: "declare host port passthrough",
                detail: std::format!("{error}"),
            }
        })?;
        DeviceRequirements::new().with_pio(
            ResourceSlot::new("registers")?,
            self.length,
            1,
            ResourceRequest::Fixed(self.base),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.pio(&ResourceSlot::new("registers")?)?;
        if base != self.base || length != self.length {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build host port passthrough",
                detail: "planned port range differs from the internal device config".into(),
            });
        }
        HostPortPassthroughFactory::new(HostPortAssignment { base, length })
            .build()
            .map_err(|error| DeviceManagerError::InvalidConfig {
                operation: "build host port passthrough",
                detail: std::format!("{error}"),
            })
    }
}

impl Device for HostPortPassthrough {
    fn name(&self) -> &str {
        "x86-host-port-passthrough"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Port || access.addr > u16::MAX as u64 {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }

        let port = Port::new(access.addr as u16);
        if access.is_read {
            self.read_port(port, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
        } else {
            self.write_port(port, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
        }
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        std::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        std::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack));
    }
    value
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        std::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack));
    }
    value
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        std::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

unsafe fn outw(port: u16, value: u16) {
    unsafe {
        std::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
    }
}

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        std::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

#[cfg(test)]
mod tests {
    use axdevice::DeviceRuntime;

    use super::*;

    #[test]
    fn passthrough_port_range_is_inclusive() {
        let dev = HostPortPassthrough::new(0x6000, 0x80).unwrap();

        assert_eq!(
            dev.resources(),
            &[Resource::PortRange {
                base: 0x6000,
                size: 0x80
            }]
        );
        assert_eq!(dev.end(), Port::new(0x607f));
    }

    #[test]
    fn passthrough_port_range_rejects_empty_and_overflowing_ranges() {
        assert!(HostPortPassthrough::new(0x6000, 0).is_err());
        assert!(HostPortPassthrough::new(0xfff0, 0x20).is_err());
    }

    #[test]
    fn passthrough_port_rejects_qword_without_touching_hardware() {
        let dev = HostPortPassthrough::new(0x6000, 0x80).unwrap();

        assert!(
            dev.read_port(Port::new(0x6000), AccessWidth::Qword)
                .is_err()
        );
        assert!(
            dev.write_port(Port::new(0x6000), AccessWidth::Qword, 0)
                .is_err()
        );
    }

    #[test]
    fn passthrough_port_bundle_registers_through_device_runtime() {
        let bundle = HostPortPassthroughFactory::new(HostPortAssignment {
            base: 0x6000,
            length: 0x80,
        })
        .build()
        .unwrap();
        let mut devices = DeviceRuntime::default();

        devices.register_bundle(bundle).unwrap();

        assert_eq!(devices.device_count(), 1);
    }
}
