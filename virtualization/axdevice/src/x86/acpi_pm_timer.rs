//! Q35-compatible ACPI power-management timer used by x86 guest firmware.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU16, Ordering};

use axdevice_base::*;

const PM_TIMER_FREQUENCY_HZ: u64 = 3_579_545;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const PM_TIMER_MASK: u64 = 0x00ff_ffff;

/// Returns monotonic host time in nanoseconds for one PM timer read.
pub type X86MonotonicNanos = fn() -> u64;

/// The minimal Q35 ACPI power-management block required by guest firmware.
pub struct X86AcpiPmTimerDevice {
    monotonic_nanos: X86MonotonicNanos,
    status: AtomicU16,
    enable: AtomicU16,
    control: AtomicU16,
    stop: StopGrant,
    // Retaining the line keeps the planned SCI endpoint and its lease alive.
    _sci: IrqLine,
    resources: Box<[Resource]>,
}

impl X86AcpiPmTimerDevice {
    /// Q35 ICH9 power-management I/O base.
    pub const PORT_BASE: u16 = 0x600;
    /// Size of the ICH9 power-management I/O window.
    pub const PORT_SIZE: u16 = 0x80;
    /// Offset of the PM1 status/enable event block.
    pub const EVENT_PORT_OFFSET: u16 = 0;
    /// Number of I/O bytes occupied by the PM1 event block.
    pub const EVENT_REGISTER_SIZE: u16 = 4;
    /// Offset of the PM1 control register.
    pub const CONTROL_PORT_OFFSET: u16 = 4;
    /// Number of I/O bytes occupied by the PM1 control register.
    pub const CONTROL_REGISTER_SIZE: u16 = 2;
    /// Offset of the free-running timer register within the PM I/O window.
    pub const TIMER_PORT_OFFSET: u16 = 8;
    /// Number of I/O bytes occupied by the timer register.
    pub const TIMER_REGISTER_SIZE: u16 = 4;

    const CONTROL_OFFSET: u64 = Self::CONTROL_PORT_OFFSET as u64;
    const TIMER_OFFSET: u64 = Self::TIMER_PORT_OFFSET as u64;

    /// Creates a PM timer and inert fixed-event block backed by the supplied clock.
    pub fn new(
        monotonic_nanos: X86MonotonicNanos,
        sci: IrqLine,
        stop: StopGrant,
    ) -> DeviceResult<Self> {
        let sci_line =
            u32::try_from(sci.input().value()).map_err(|_| DeviceError::InvalidInput {
                operation: "create x86 ACPI PM device",
                detail: "SCI controller input does not fit the device resource format".into(),
            })?;
        Ok(Self {
            monotonic_nanos,
            status: AtomicU16::new(0),
            enable: AtomicU16::new(0),
            control: AtomicU16::new(0),
            stop,
            _sci: sci,
            resources: alloc::vec![
                Resource::PortRange {
                    base: Self::PORT_BASE,
                    size: Self::PORT_SIZE,
                },
                Resource::IrqLine {
                    line: sci_line,
                    trigger: InterruptTriggerMode::LevelTriggered,
                },
            ]
            .into_boxed_slice(),
        })
    }

    fn counter(&self) -> u32 {
        pm_timer_counter((self.monotonic_nanos)())
    }

    fn access_size(access: &BusAccess) -> Result<u64, DeviceError> {
        match access.width {
            AccessWidth::Byte => Ok(1),
            AccessWidth::Word => Ok(2),
            AccessWidth::Dword => Ok(4),
            AccessWidth::Qword => Err(DeviceError::Unsupported {
                operation: "read x86 ACPI PM timer",
                detail: "the ICH9 power-management block supports accesses up to 32 bits".into(),
            }),
        }
    }

    fn offset(&self, access: &BusAccess, width: u64) -> Result<u64, DeviceError> {
        let offset = access
            .addr
            .checked_sub(u64::from(Self::PORT_BASE))
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        if offset + width > u64::from(Self::PORT_SIZE) {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        Ok(offset)
    }

    fn read(&self, access: &BusAccess) -> Result<u64, DeviceError> {
        let width = Self::access_size(access)?;
        let offset = self.offset(access, width)?;
        let timer = self.counter().to_le_bytes();
        let status = self.status.load(Ordering::Acquire).to_le_bytes();
        let enable = self.enable.load(Ordering::Acquire).to_le_bytes();
        let control = self.control.load(Ordering::Acquire).to_le_bytes();
        let mut value = 0u64;
        for index in 0..width {
            let register = offset + index;
            let byte = match register {
                0..=1 => status[register as usize],
                2..=3 => enable[(register - 2) as usize],
                Self::CONTROL_OFFSET..=5 => control[(register - Self::CONTROL_OFFSET) as usize],
                Self::TIMER_OFFSET..=11 => timer[(register - Self::TIMER_OFFSET) as usize],
                _ => 0,
            };
            value |= u64::from(byte) << (index * 8);
        }
        Ok(value)
    }

    fn write(&self, access: &BusAccess, context: &mut dyn DeviceAccess) -> Result<(), DeviceError> {
        let width = Self::access_size(access)?;
        let offset = self.offset(access, width)?;
        let mut status_clear = 0u16;
        let mut enable_mask = 0u16;
        let mut enable_value = 0u16;
        let mut control_mask = 0u16;
        let mut control_value = 0u16;
        for index in 0..width {
            let register = offset + index;
            let byte = ((access.data >> (index * 8)) & 0xff) as u16;
            match register {
                0..=1 => status_clear |= byte << (register * 8),
                2..=3 => {
                    let shift = (register - 2) * 8;
                    enable_mask |= 0xffu16 << shift;
                    enable_value |= byte << shift;
                }
                Self::CONTROL_OFFSET..=5 => {
                    let shift = (register - Self::CONTROL_OFFSET) * 8;
                    control_mask |= 0xffu16 << shift;
                    control_value |= byte << shift;
                }
                _ => {}
            }
        }
        if status_clear != 0 {
            self.status.fetch_and(!status_clear, Ordering::AcqRel);
        }
        update_register(&self.enable, enable_mask, enable_value);
        let control = update_register(&self.control, control_mask, control_value);
        if control_mask & (1 << 13) != 0 && control & 0x3c00 == 0x2000 {
            context.request_vm_stop(&self.stop, "ACPI soft-off")?;
        }
        Ok(())
    }
}

fn update_register(register: &AtomicU16, mask: u16, value: u16) -> u16 {
    if mask == 0 {
        return register.load(Ordering::Acquire);
    }
    let mut current = register.load(Ordering::Acquire);
    loop {
        let next = (current & !mask) | value;
        match register.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

impl Device for X86AcpiPmTimerDevice {
    fn name(&self) -> &str {
        "x86-acpi-pm-timer"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Port {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        if access.is_read {
            self.read(access).map(|value| BusResponse::Read { value })
        } else {
            self.write(access, context).map(|()| BusResponse::Write)
        }
    }
}

fn pm_timer_counter(now_nanos: u64) -> u32 {
    let seconds = now_nanos / NANOSECONDS_PER_SECOND;
    let nanos = now_nanos % NANOSECONDS_PER_SECOND;
    let ticks = seconds
        .wrapping_mul(PM_TIMER_FREQUENCY_HZ)
        .wrapping_add(nanos * PM_TIMER_FREQUENCY_HZ / NANOSECONDS_PER_SECOND);
    (ticks & PM_TIMER_MASK) as u32
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicU64, Ordering};

    use axdevice_base::{
        ControllerInputId, DeviceId, InterruptControllerId, IrqResult, WiredIrqInput, WiredIrqSink,
    };

    use super::*;

    static NOW_NANOS: AtomicU64 = AtomicU64::new(0);

    struct NoMemory;

    impl DeviceAccess for NoMemory {
        fn device_id(&self) -> DeviceId {
            DeviceId::new(0)
        }
    }

    struct StopAccess {
        grant: StopGrant,
        requested: bool,
    }

    impl DeviceAccess for StopAccess {
        fn device_id(&self) -> DeviceId {
            DeviceId::new(0)
        }

        fn request_vm_stop(&mut self, grant: &StopGrant, _reason: &str) -> DeviceResult {
            assert!(grant.same_token(&self.grant));
            self.requested = true;
            Ok(())
        }
    }

    fn now_nanos() -> u64 {
        NOW_NANOS.load(Ordering::Relaxed)
    }

    struct NoopIrqSink;

    impl WiredIrqSink for NoopIrqSink {
        fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
            Ok(())
        }

        fn pulse(&self, _input: ControllerInputId) -> IrqResult {
            Ok(())
        }
    }

    fn sci_line() -> IrqLine {
        WiredIrqInput::new(
            InterruptControllerId::new(0),
            ControllerInputId::new(9),
            InterruptTriggerMode::LevelTriggered,
            Arc::new(NoopIrqSink),
        )
        .connect()
        .unwrap()
    }

    #[test]
    fn pm_timer_advances_at_the_acpi_frequency() {
        NOW_NANOS.store(0, Ordering::Relaxed);
        let timer = X86AcpiPmTimerDevice::new(now_nanos, sci_line(), StopGrant::new()).unwrap();
        let mut memory = NoMemory;
        let access = BusAccess {
            kind: BusKind::Port,
            is_read: true,
            addr: u64::from(X86AcpiPmTimerDevice::PORT_BASE) + X86AcpiPmTimerDevice::TIMER_OFFSET,
            width: AccessWidth::Dword,
            data: 0,
        };

        let BusResponse::Read { value: first } = timer.access(&access, &mut memory).unwrap() else {
            panic!("PM timer read returned a write response");
        };
        NOW_NANOS.store(NANOSECONDS_PER_SECOND, Ordering::Relaxed);
        let BusResponse::Read { value: second } = timer.access(&access, &mut memory).unwrap()
        else {
            panic!("PM timer read returned a write response");
        };

        assert_eq!(first, 0);
        assert_eq!(second, PM_TIMER_FREQUENCY_HZ);
    }

    #[test]
    fn pm1_control_soft_off_uses_the_device_stop_capability() {
        let grant = StopGrant::new();
        let timer = X86AcpiPmTimerDevice::new(now_nanos, sci_line(), grant.clone()).unwrap();
        let mut access_context = StopAccess {
            grant,
            requested: false,
        };
        timer
            .access(
                &BusAccess {
                    kind: BusKind::Port,
                    is_read: false,
                    addr: u64::from(X86AcpiPmTimerDevice::PORT_BASE)
                        + X86AcpiPmTimerDevice::CONTROL_OFFSET,
                    width: AccessWidth::Word,
                    data: 1 << 13,
                },
                &mut access_context,
            )
            .unwrap();
        assert!(access_context.requested);
    }
}
