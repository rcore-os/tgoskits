//! Native x86 host I/O port passthrough devices.

use axdevice_base::{AccessWidth, BaseDeviceOps, EmuDeviceType, Port, PortRange};
use axvm_types::{AxVmError as BackendError, AxVmResult as BackendResult};

use crate::{AxVmResult, ax_err};

/// A host x86 I/O port range passed directly through to a guest.
pub(crate) struct HostPortPassthrough {
    base: Port,
    length: u16,
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
        })
    }

    fn end(&self) -> Port {
        Port::new(self.base.number() + self.length - 1)
    }
}

impl BaseDeviceOps<PortRange> for HostPortPassthrough {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }

    fn address_range(&self) -> PortRange {
        PortRange::new(self.base, self.end())
    }

    fn handle_read(&self, port: Port, width: AccessWidth) -> BackendResult<usize> {
        match width {
            AccessWidth::Byte => Ok(unsafe { inb(port.number()) } as usize),
            AccessWidth::Word => Ok(unsafe { inw(port.number()) } as usize),
            AccessWidth::Dword => Ok(unsafe { inl(port.number()) } as usize),
            AccessWidth::Qword => Err(BackendError::Unsupported),
        }
    }

    fn handle_write(&self, port: Port, width: AccessWidth, value: usize) -> BackendResult {
        match width {
            AccessWidth::Byte => unsafe { outb(port.number(), value as u8) },
            AccessWidth::Word => unsafe { outw(port.number(), value as u16) },
            AccessWidth::Dword => unsafe { outl(port.number(), value as u32) },
            AccessWidth::Qword => {
                return Err(BackendError::Unsupported);
            }
        }
        Ok(())
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack));
    }
    value
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack));
    }
    value
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

unsafe fn outw(port: u16, value: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
    }
}

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_port_range_is_inclusive() {
        let dev = HostPortPassthrough::new(0x6000, 0x80).unwrap();

        assert_eq!(
            dev.address_range(),
            PortRange::new(Port::new(0x6000), Port::new(0x607f))
        );
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
            dev.handle_read(Port::new(0x6000), AccessWidth::Qword)
                .is_err()
        );
        assert!(
            dev.handle_write(Port::new(0x6000), AccessWidth::Qword, 0)
                .is_err()
        );
    }
}
