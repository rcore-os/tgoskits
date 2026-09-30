//! x86 compatibility window for an absent TPM TIS device.

use axdevice::*;
use axdevice_base::*;

const TPM_TIS_BASE: u64 = 0xfed4_0000;
const TPM_TIS_SIZE: u64 = 0x1000;

/// Emulates the open-bus behavior of an absent QEMU TPM TIS window.
pub(super) struct X86UnassignedTpmMmioModel;

impl DeviceModel for X86UnassignedTpmMmioModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(
            ResourceSlot::new("registers")?,
            TPM_TIS_SIZE,
            1,
            ResourceRequest::Fixed(TPM_TIS_BASE),
        )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let range = context.mmio(&ResourceSlot::new("registers")?)?;
        if range != (TPM_TIS_BASE, TPM_TIS_SIZE) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 absent TPM TIS window",
                detail: "planned TPM TIS probe range differs from the Q35 compatibility range"
                    .into(),
            });
        }
        let device: std::sync::Arc<dyn Device> = std::sync::Arc::new(X86UnassignedTpmMmioDevice);
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            device,
        )))
    }
}

struct X86UnassignedTpmMmioDevice;

impl Device for X86UnassignedTpmMmioDevice {
    fn name(&self) -> &str {
        "x86-absent-tpm-tis-mmio"
    }

    fn resources(&self) -> &[Resource] {
        static RESOURCES: [Resource; 1] = [Resource::MmioRange {
            base: TPM_TIS_BASE,
            size: TPM_TIS_SIZE,
        }];
        &RESOURCES
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        validate_access(access)?;
        Ok(match access.width() {
            AccessWidth::Byte => u8::MAX as u64,
            AccessWidth::Word => u16::MAX as u64,
            AccessWidth::Dword => u32::MAX as u64,
            AccessWidth::Qword => u64::MAX,
        })
    }

    fn write(
        &self,
        access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        validate_access(access)
    }
}

fn validate_access(access: &DeviceAccess) -> DeviceResult {
    if access.bus() != BusKind::Mmio {
        return Err(DeviceError::OutOfRange {
            addr: access.address(),
        });
    }
    let end = access
        .address()
        .checked_add(access.width().size() as u64)
        .ok_or(DeviceError::OutOfRange {
            addr: access.address(),
        })?;
    if access.address() < TPM_TIS_BASE || end > TPM_TIS_BASE + TPM_TIS_SIZE {
        return Err(DeviceError::OutOfRange {
            addr: access.address(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_tpm_reads_as_width_sized_open_bus() {
        let device = X86UnassignedTpmMmioDevice;
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        for (width, expected) in [
            (AccessWidth::Byte, u8::MAX as u64),
            (AccessWidth::Word, u16::MAX as u64),
            (AccessWidth::Dword, u32::MAX as u64),
            (AccessWidth::Qword, u64::MAX),
        ] {
            let access =
                DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Mmio, TPM_TIS_BASE, width);
            assert_eq!(device.read(&access, &mut context), Ok(expected));
        }
    }

    #[test]
    fn absent_tpm_ignores_writes_inside_the_probe_window() {
        let device = X86UnassignedTpmMmioDevice;
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        let access = DeviceAccess::new(
            DeviceVcpuId::new(0),
            BusKind::Mmio,
            TPM_TIS_BASE + 0x30,
            AccessWidth::Dword,
        );
        assert_eq!(device.write(&access, 0x1234_5678, &mut context), Ok(()));
    }
}
