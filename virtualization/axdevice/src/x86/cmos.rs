//! Minimal MC146818-compatible CMOS state for x86 guest firmware.

use alloc::boxed::Box;

use ax_sync::SpinLock as Mutex;
use axdevice_base::*;

const INDEX_PORT: u16 = 0x70;
const DATA_PORT: u16 = 0x71;
const CMOS_BYTES: usize = 128;
const RTC_REGISTER_C: usize = 0x0c;
const RTC_REGISTER_D: usize = 0x0d;
const RTC_VALID: u8 = 0x80;

/// CMOS device exposing the guest memory size and a valid static RTC value.
pub struct X86CmosDevice {
    state: Mutex<CmosState>,
    resources: Box<[Resource]>,
}

struct CmosState {
    index: u8,
    bytes: [u8; CMOS_BYTES],
}

impl X86CmosDevice {
    /// Creates CMOS contents for a guest whose contiguous low RAM ends at `low_memory_size`.
    pub fn new(low_memory_size: u64) -> Self {
        Self {
            state: Mutex::new(CmosState::new(low_memory_size)),
            resources: alloc::vec![Resource::PortRange {
                base: INDEX_PORT,
                size: 2,
            }]
            .into_boxed_slice(),
        }
    }
}

impl CmosState {
    fn new(low_memory_size: u64) -> Self {
        let mut state = Self {
            index: 0,
            bytes: [0; CMOS_BYTES],
        };

        // Keep the RTC valid and deterministic. Firmware only needs a coherent
        // calendar until the guest installs its own time source.
        state.bytes[0x06] = 0x06;
        state.bytes[0x07] = 0x01;
        state.bytes[0x08] = 0x01;
        state.bytes[0x09] = 0x00;
        state.bytes[0x0a] = 0x26;
        state.bytes[0x0b] = 0x02;
        state.bytes[RTC_REGISTER_D] = RTC_VALID;
        state.bytes[0x32] = 0x20;

        state.write_word(0x15, 640);
        let below_16m_kib = low_memory_size
            .saturating_sub(1024 * 1024)
            .min(15 * 1024 * 1024)
            / 1024;
        state.write_word(0x17, below_16m_kib as u16);
        state.write_word(0x30, below_16m_kib as u16);

        let above_16m = low_memory_size.saturating_sub(16 * 1024 * 1024) / (64 * 1024);
        state.write_word(0x34, above_16m.min(u64::from(u16::MAX)) as u16);
        let above_4g = low_memory_size.saturating_sub(4 * 1024 * 1024 * 1024) / (64 * 1024);
        let above_4g = above_4g.min(0x00ff_ffff) as u32;
        state.bytes[0x5b] = above_4g as u8;
        state.bytes[0x5c] = (above_4g >> 8) as u8;
        state.bytes[0x5d] = (above_4g >> 16) as u8;
        state
    }

    fn write_word(&mut self, register: usize, value: u16) {
        self.bytes[register..register + 2].copy_from_slice(&value.to_le_bytes());
    }
}

impl Device for X86CmosDevice {
    fn name(&self) -> &str {
        "x86-cmos"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        validate_access(access)?;
        let mut state = self.state.lock_irqsave();
        match access.address() {
            addr if addr == u64::from(INDEX_PORT) => Ok(u64::from(state.index)),
            addr if addr == u64::from(DATA_PORT) => {
                let index = usize::from(state.index);
                let value = state.bytes[index];
                if index == RTC_REGISTER_C {
                    state.bytes[index] = 0;
                }
                Ok(u64::from(value))
            }
            addr => Err(DeviceError::OutOfRange { addr }),
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        validate_access(access)?;
        let mut state = self.state.lock_irqsave();
        match access.address() {
            addr if addr == u64::from(INDEX_PORT) => {
                state.index = value as u8 & 0x7f;
                Ok(())
            }
            addr if addr == u64::from(DATA_PORT) => {
                let index = usize::from(state.index);
                if !matches!(index, RTC_REGISTER_C | RTC_REGISTER_D) {
                    state.bytes[index] = value as u8;
                }
                Ok(())
            }
            addr => Err(DeviceError::OutOfRange { addr }),
        }
    }
}

fn validate_access(access: &DeviceAccess) -> DeviceResult {
    if access.bus() != BusKind::Port || access.width() != AccessWidth::Byte {
        return Err(DeviceError::Unsupported {
            operation: "access x86 CMOS",
            detail: "CMOS supports byte port accesses only".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axdevice_base::{DeviceContext, DeviceId, DeviceVcpuId};

    use super::*;

    struct NoMemory;

    impl DeviceContext for NoMemory {
        fn device_id(&self) -> DeviceId {
            DeviceId::new(0)
        }
    }

    fn read_register(device: &X86CmosDevice, register: u8) -> u8 {
        let mut memory = NoMemory;
        device
            .write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Port,
                    u64::from(INDEX_PORT),
                    AccessWidth::Byte,
                ),
                u64::from(register),
                &mut memory,
            )
            .unwrap();
        device
            .read(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Port,
                    u64::from(DATA_PORT),
                    AccessWidth::Byte,
                ),
                &mut memory,
            )
            .unwrap() as u8
    }

    fn write_register(device: &X86CmosDevice, register: u8, value: u8) {
        let mut memory = NoMemory;
        device
            .write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Port,
                    u64::from(INDEX_PORT),
                    AccessWidth::Byte,
                ),
                u64::from(register),
                &mut memory,
            )
            .unwrap();
        device
            .write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Port,
                    u64::from(DATA_PORT),
                    AccessWidth::Byte,
                ),
                u64::from(value),
                &mut memory,
            )
            .unwrap();
    }

    #[test]
    fn cmos_reports_the_guest_low_memory_limit() {
        let device = X86CmosDevice::new(512 * 1024 * 1024);

        assert_eq!(
            device.resources(),
            &[Resource::PortRange {
                base: 0x70,
                size: 2
            }]
        );
        assert_eq!(read_register(&device, 0x34), 0x00);
        assert_eq!(read_register(&device, 0x35), 0x1f);
        assert_eq!(read_register(&device, 0x0d), 0x80);
    }

    #[test]
    fn rtc_valid_bit_survives_register_d_writes() {
        let device = X86CmosDevice::new(512 * 1024 * 1024);

        write_register(&device, 0x0d, 0);

        assert_eq!(read_register(&device, 0x0d), 0x80);
    }
}
