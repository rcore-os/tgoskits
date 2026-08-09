//! Portable write filtering for shared physical MMIO providers.

use std::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};

use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, Resource,
};
use rdif_clk::ClockMmioWriteProtection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilteredMmioWrite {
    Forward(u64),
    Suppress,
}

pub(crate) fn filter_mmio_write(
    protections: &[ClockMmioWriteProtection],
    offset: usize,
    width: AccessWidth,
    mut value: u64,
) -> FilteredMmioWrite {
    let mut filtered_protected_register = false;
    for protection in protections {
        match *protection {
            ClockMmioWriteProtection::Deny {
                offset: protected_offset,
                length,
            } => {
                if ranges_overlap(offset, width.size(), protected_offset, length) {
                    return FilteredMmioWrite::Suppress;
                }
            }
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: protected_offset,
                value_mask,
                write_enable_mask,
            } => {
                if !ranges_overlap(offset, width.size(), protected_offset, 4) {
                    continue;
                }
                if offset != protected_offset || width != AccessWidth::Dword {
                    return FilteredMmioWrite::Suppress;
                }
                value &= !u64::from(value_mask | write_enable_mask);
                filtered_protected_register = true;
            }
        }
    }

    if filtered_protected_register && value == 0 {
        FilteredMmioWrite::Suppress
    } else {
        FilteredMmioWrite::Forward(value)
    }
}

pub(crate) trait MmioRegisterAccess: Send + Sync {
    fn read(&self, offset: usize, width: AccessWidth) -> Result<u64, DeviceError>;

    fn write(&self, offset: usize, width: AccessWidth, value: u64) -> Result<(), DeviceError>;
}

pub(crate) struct SharedMmioDevice {
    name: String,
    base: u64,
    length: usize,
    protections: Box<[ClockMmioWriteProtection]>,
    backend: Arc<dyn MmioRegisterAccess>,
    resources: Box<[Resource]>,
}

impl SharedMmioDevice {
    pub(crate) fn new(
        name: String,
        base: usize,
        length: usize,
        protections: Vec<ClockMmioWriteProtection>,
        backend: Arc<dyn MmioRegisterAccess>,
    ) -> Self {
        Self {
            name,
            base: base as u64,
            length,
            protections: protections.into_boxed_slice(),
            backend,
            resources: vec![Resource::MmioRange {
                base: base as u64,
                size: length as u64,
            }]
            .into_boxed_slice(),
        }
    }

    fn handle_access(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let offset = self.checked_offset(access)?;
        if access.is_read {
            return self
                .backend
                .read(offset, access.width)
                .map(|value| BusResponse::Read { value });
        }

        match filter_mmio_write(&self.protections, offset, access.width, access.data) {
            FilteredMmioWrite::Forward(value) => {
                self.backend.write(offset, access.width, value)?;
            }
            FilteredMmioWrite::Suppress => {}
        }
        Ok(BusResponse::Write)
    }

    fn checked_offset(&self, access: &BusAccess) -> Result<usize, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let offset = access
            .addr
            .checked_sub(self.base)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        let width = access.width.size();
        let end = offset
            .checked_add(width)
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        if end > self.length {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        if !offset.is_multiple_of(width) {
            return Err(DeviceError::InvalidInput {
                operation: "access shared MMIO provider",
                detail: std::format!(
                    "unaligned {:?} access at provider offset {offset:#x}",
                    access.width
                ),
            });
        }
        Ok(offset)
    }
}

impl Device for SharedMmioDevice {
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
        self.handle_access(access)
    }
}

fn ranges_overlap(
    first_offset: usize,
    first_length: usize,
    second_offset: usize,
    second_length: usize,
) -> bool {
    let Some(first_end) = first_offset.checked_add(first_length) else {
        return true;
    };
    let Some(second_end) = second_offset.checked_add(second_length) else {
        return true;
    };
    first_offset < second_end && second_offset < first_end
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn strips_rk3568_uart2_gate_disable_write() {
        let protections = [ClockMmioWriteProtection::MaskedWrite32 {
            offset: 0x370,
            value_mask: 0xf,
            write_enable_mask: 0xf << 16,
        }];

        let filtered = filter_mmio_write(&protections, 0x370, AccessWidth::Dword, 0x0009_0009);

        assert_eq!(filtered, FilteredMmioWrite::Suppress);
    }

    #[test]
    fn forwards_unprotected_bits_in_the_same_rockchip_write() {
        let protections = [ClockMmioWriteProtection::MaskedWrite32 {
            offset: 0x370,
            value_mask: 0x9,
            write_enable_mask: 0x9 << 16,
        }];

        assert_eq!(
            filter_mmio_write(&protections, 0x370, AccessWidth::Dword, 0x0019_0019,),
            FilteredMmioWrite::Forward(0x0010_0010)
        );
    }

    #[test]
    fn suppresses_partial_or_denied_protected_writes() {
        let protections = [
            ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x370,
                value_mask: 0xf,
                write_enable_mask: 0xf << 16,
            },
            ClockMmioWriteProtection::Deny {
                offset: 0x1dc,
                length: 4,
            },
        ];

        assert_eq!(
            filter_mmio_write(&protections, 0x371, AccessWidth::Byte, 0xff),
            FilteredMmioWrite::Suppress
        );
        assert_eq!(
            filter_mmio_write(&protections, 0x1dc, AccessWidth::Dword, u32::MAX.into()),
            FilteredMmioWrite::Suppress
        );
    }

    #[test]
    fn shared_device_forwards_reads_and_filtered_writes() {
        let backend = Arc::new(MockBackend::new(0x1122_3344));
        let device = SharedMmioDevice::new(
            "shared-clock-provider".into(),
            0x1000,
            0x1000,
            vec![ClockMmioWriteProtection::MaskedWrite32 {
                offset: 0x370,
                value_mask: 0x9,
                write_enable_mask: 0x9 << 16,
            }],
            backend.clone(),
        );

        let read = device
            .handle_access(&BusAccess {
                kind: BusKind::Mmio,
                is_read: true,
                addr: 0x1370,
                width: AccessWidth::Dword,
                data: 0,
            })
            .unwrap();
        assert!(matches!(read, BusResponse::Read { value: 0x1122_3344 }));

        device
            .handle_access(&BusAccess {
                kind: BusKind::Mmio,
                is_read: false,
                addr: 0x1370,
                width: AccessWidth::Dword,
                data: 0x0019_0019,
            })
            .unwrap();
        assert_eq!(
            backend.writes.lock().unwrap().as_slice(),
            &[(0x370, AccessWidth::Dword, 0x0010_0010)]
        );
        assert_eq!(
            device.resources(),
            &[Resource::MmioRange {
                base: 0x1000,
                size: 0x1000,
            }]
        );
    }

    #[test]
    fn shared_device_forwards_zero_writes_outside_protected_registers() {
        let backend = Arc::new(MockBackend::new(0));
        let device = SharedMmioDevice::new(
            "shared-clock-provider".into(),
            0x1000,
            0x1000,
            vec![ClockMmioWriteProtection::Deny {
                offset: 0x370,
                length: 4,
            }],
            backend.clone(),
        );

        device
            .handle_access(&BusAccess {
                kind: BusKind::Mmio,
                is_read: false,
                addr: 0x1200,
                width: AccessWidth::Dword,
                data: 0,
            })
            .unwrap();

        assert_eq!(
            backend.writes.lock().unwrap().as_slice(),
            &[(0x200, AccessWidth::Dword, 0)]
        );
    }

    struct MockBackend {
        read_value: u64,
        writes: Mutex<Vec<(usize, AccessWidth, u64)>>,
    }

    impl MockBackend {
        fn new(read_value: u64) -> Self {
            Self {
                read_value,
                writes: Mutex::new(Vec::new()),
            }
        }
    }

    impl MmioRegisterAccess for MockBackend {
        fn read(&self, _offset: usize, _width: AccessWidth) -> Result<u64, DeviceError> {
            Ok(self.read_value)
        }

        fn write(&self, offset: usize, width: AccessWidth, value: u64) -> Result<(), DeviceError> {
            self.writes.lock().unwrap().push((offset, width, value));
            Ok(())
        }
    }
}
