//! Stateless PCI Express Enhanced Configuration Access Mechanism device.

use alloc::boxed::Box;

use axdevice_base::{
    AccessWidth, BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult, Resource,
};

const MIB: u64 = 1 << 20;
const MAX_SIZE: u64 = 256 * MIB;
const FUNCTION_SIZE: u64 = 1 << 12;

/// A VM-owned PCI Express ECAM aperture with no endpoint functions.
#[derive(Debug)]
pub struct PciEcamDevice {
    base: u64,
    size: u64,
    resources: Box<[Resource]>,
}

impl PciEcamDevice {
    /// Creates a stateless PCI ECAM device for an aperture beginning at `base`.
    ///
    /// The aperture starts at bus zero, and its size determines the number of
    /// buses exposed to the guest. Every BDF is initially absent.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceError::InvalidInput`] when `base` is not 1 MiB aligned,
    /// when `size` is zero, not 1 MiB granular, or larger than 256 MiB, or when
    /// the resulting aperture end overflows the address space.
    pub fn new(base: u64, size: u64) -> Result<Self, DeviceError> {
        validate_aperture(base, size)?;
        Ok(Self {
            base,
            size,
            resources: alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice(),
        })
    }

    fn decode_access(&self, access: &DeviceAccess) -> Result<DecodedEcamAddress, DeviceError> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }

        let width = access_width(access.width())?;
        let offset = access
            .address()
            .checked_sub(self.base)
            .ok_or(DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        let access_end = offset.checked_add(width).ok_or(DeviceError::OutOfRange {
            addr: access.address(),
        })?;
        if access_end > self.size {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }

        let register = offset & (FUNCTION_SIZE - 1);
        if register + width > FUNCTION_SIZE {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        if !offset.is_multiple_of(width) {
            return Err(DeviceError::InvalidInput {
                operation: "access PCI ECAM",
                detail: "word and dword accesses must be naturally aligned".into(),
            });
        }

        Ok(DecodedEcamAddress {
            bus: (offset >> 20) as u8,
            device: ((offset >> 15) & 0x1f) as u8,
            function: ((offset >> 12) & 0x7) as u8,
            register: register as u16,
        })
    }
}

impl Device for PciEcamDevice {
    fn name(&self) -> &str {
        "pci-ecam"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let decoded = self.decode_access(access)?;
        let _ = decoded.components();
        Ok(all_ones(access_width(access.width())?))
    }

    fn write(
        &self,
        access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let decoded = self.decode_access(access)?;
        let _ = decoded.components();
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct DecodedEcamAddress {
    bus: u8,
    device: u8,
    function: u8,
    register: u16,
}

impl DecodedEcamAddress {
    fn components(self) -> (u8, u8, u8, u16) {
        (self.bus, self.device, self.function, self.register)
    }
}

fn validate_aperture(base: u64, size: u64) -> Result<(), DeviceError> {
    if !base.is_multiple_of(MIB) {
        return Err(DeviceError::InvalidInput {
            operation: "construct PCI ECAM device",
            detail: "the aperture base must be 1 MiB aligned".into(),
        });
    }
    if size == 0 || !size.is_multiple_of(MIB) || size > MAX_SIZE {
        return Err(DeviceError::InvalidInput {
            operation: "construct PCI ECAM device",
            detail: "the aperture size must be nonzero, 1 MiB granular, and at most 256 MiB".into(),
        });
    }
    base.checked_add(size).ok_or(DeviceError::InvalidInput {
        operation: "construct PCI ECAM device",
        detail: "the aperture end overflows the address space".into(),
    })?;
    Ok(())
}

fn access_width(width: AccessWidth) -> Result<u64, DeviceError> {
    match width {
        AccessWidth::Byte => Ok(1),
        AccessWidth::Word => Ok(2),
        AccessWidth::Dword => Ok(4),
        AccessWidth::Qword => Err(DeviceError::Unsupported {
            operation: "access PCI ECAM",
            detail: "PCI configuration space supports accesses up to 32 bits".into(),
        }),
    }
}

fn all_ones(width: u64) -> u64 {
    u64::MAX >> ((8 - width) * 8)
}

#[cfg(test)]
mod tests {
    use axdevice_base::{
        AccessWidth, BusKind, Device, DeviceAccess, DeviceError, DeviceId, DeviceVcpuId,
        NoopDeviceContext,
    };

    use super::PciEcamDevice;

    const BASE: u64 = 0x2000_0000;
    const SIZE: u64 = 0x0800_0000;

    #[test]
    fn rejects_invalid_constructor_arguments() {
        assert!(PciEcamDevice::new(BASE, 0).is_err());
        assert!(PciEcamDevice::new(BASE + 0x1000, SIZE).is_err());
        assert!(PciEcamDevice::new(BASE, SIZE + 1).is_err());
        assert!(PciEcamDevice::new(BASE, 0x1010_0000).is_err());
        assert!(PciEcamDevice::new(0xffff_ffff_fff0_0000, 0x0010_0000).is_err());
    }

    #[test]
    fn absent_bdf_reads_return_width_matched_all_ones() {
        let device = PciEcamDevice::new(BASE, SIZE).unwrap();

        assert_eq!(read(&device, BASE, AccessWidth::Byte), 0xff);
        assert_eq!(read(&device, BASE, AccessWidth::Word), 0xffff);
        assert_eq!(read(&device, BASE, AccessWidth::Dword), 0xffff_ffff);
    }

    #[test]
    fn decoder_is_relative_to_aperture_base() {
        let device = PciEcamDevice::new(BASE, SIZE).unwrap();
        let offset = (0x12 << 20) | (0x0b << 15) | (0x5 << 12) | 0x3c;
        let decoded = device.decode_access(&mmio(BASE + offset, AccessWidth::Dword));

        let decoded = decoded.unwrap();
        assert_eq!(decoded.bus, 0x12);
        assert_eq!(decoded.device, 0x0b);
        assert_eq!(decoded.function, 0x5);
        assert_eq!(decoded.register, 0x3c);
    }

    #[test]
    fn writes_to_absent_bdf_are_ignored() {
        let device = PciEcamDevice::new(BASE, SIZE).unwrap();
        let access = mmio(BASE + 0x12_3454, AccessWidth::Dword);
        let mut context = NoopDeviceContext::new(DeviceId::new(0));

        device.write(&access, 0x1234_5678, &mut context).unwrap();
        assert_eq!(device.read(&access, &mut context).unwrap(), 0xffff_ffff);
    }

    #[test]
    fn rejects_unsupported_width_and_wrong_alignment() {
        let device = PciEcamDevice::new(BASE, SIZE).unwrap();

        assert!(matches!(
            read_result(&device, BASE, AccessWidth::Qword),
            Err(DeviceError::Unsupported { .. })
        ));
        assert!(matches!(
            read_result(&device, BASE + 1, AccessWidth::Word),
            Err(DeviceError::InvalidInput { .. })
        ));
        assert!(matches!(
            read_result(&device, BASE + 2, AccessWidth::Dword),
            Err(DeviceError::InvalidInput { .. })
        ));
    }

    #[test]
    fn rejects_aperture_end_and_cross_function_accesses() {
        let device = PciEcamDevice::new(BASE, SIZE).unwrap();

        assert!(matches!(
            read_result(&device, BASE + SIZE, AccessWidth::Byte),
            Err(DeviceError::OutOfRange { .. })
        ));
        assert!(matches!(
            read_result(&device, BASE + SIZE - 1, AccessWidth::Word),
            Err(DeviceError::OutOfRange { .. })
        ));
        assert_eq!(read(&device, BASE + 0xfff, AccessWidth::Byte), 0xff);
        assert!(matches!(
            read_result(&device, BASE + 0xfff, AccessWidth::Word),
            Err(DeviceError::OutOfRange { .. })
        ));
        assert!(matches!(
            read_result(&device, BASE + 0xffe, AccessWidth::Dword),
            Err(DeviceError::OutOfRange { .. })
        ));
    }

    fn mmio(address: u64, width: AccessWidth) -> DeviceAccess {
        DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Mmio, address, width)
    }

    fn read(device: &PciEcamDevice, address: u64, width: AccessWidth) -> u64 {
        read_result(device, address, width).unwrap()
    }

    fn read_result(
        device: &PciEcamDevice,
        address: u64,
        width: AccessWidth,
    ) -> Result<u64, DeviceError> {
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        device.read(&mmio(address, width), &mut context)
    }
}
