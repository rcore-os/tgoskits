//! System-register device exposing one timer state per current vCPU.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::any::Any;

use aarch64_sysreg::SystemRegType;
use axdevice::{AccessWidth, Device, IrqLine};
use axdevice_base::{BusAccess, BusKind, BusResponse, DeviceError, Resource};

use super::state::{VirtualPhysicalTimer, physical_counter};

const CNTFRQ: u32 = SystemRegType::CNTFRQ_EL0 as u32;
const CNTPCT: u32 = SystemRegType::CNTPCT_EL0 as u32;
const CNTP_TVAL: u32 = SystemRegType::CNTP_TVAL_EL0 as u32;
const CNTP_CTL: u32 = SystemRegType::CNTP_CTL_EL0 as u32;
const CNTP_CVAL: u32 = SystemRegType::CNTP_CVAL_EL0 as u32;

pub(super) struct VirtualTimerBank {
    timers: Vec<Arc<VirtualPhysicalTimer>>,
    frequency: u64,
    resources: Box<[Resource]>,
}

impl VirtualTimerBank {
    pub(super) fn new(lines: Vec<IrqLine>, frequency: u64) -> Self {
        Self {
            timers: lines
                .into_iter()
                .map(|line| Arc::new(VirtualPhysicalTimer::new(line, frequency)))
                .collect(),
            frequency,
            resources: [CNTFRQ, CNTPCT, CNTP_TVAL, CNTP_CTL, CNTP_CVAL]
                .map(|addr| Resource::SysReg { addr, count: 1 })
                .into(),
        }
    }

    fn timer_for_current_vcpu(&self) -> Result<&Arc<VirtualPhysicalTimer>, DeviceError> {
        let vcpu = crate::current_vcpu_id().ok_or_else(|| DeviceError::Backend {
            operation: "access AArch64 physical timer",
            detail: "no vCPU is current on this host CPU".into(),
        })?;
        self.timers
            .get(vcpu)
            .ok_or_else(|| DeviceError::InvalidInput {
                operation: "access AArch64 physical timer",
                detail: alloc::format!("current vCPU {vcpu} has no timer state"),
            })
    }

    fn read(&self, addr: u32) -> Result<u64, DeviceError> {
        match addr {
            CNTFRQ => Ok(self.frequency),
            CNTPCT => Ok(physical_counter()),
            CNTP_TVAL => Ok(self.timer_for_current_vcpu()?.read_tval()),
            CNTP_CTL => Ok(self.timer_for_current_vcpu()?.read_control()),
            CNTP_CVAL => Ok(self.timer_for_current_vcpu()?.read_compare()),
            _ => Err(DeviceError::OutOfRange {
                addr: u64::from(addr),
            }),
        }
    }

    fn write(&self, addr: u32, value: u64) -> Result<(), DeviceError> {
        let timer = self.timer_for_current_vcpu()?;
        let result = match addr {
            CNTP_TVAL => timer.write_tval(value as u32),
            CNTP_CTL => timer.write_control(value as u32),
            CNTP_CVAL => timer.write_compare(value),
            CNTFRQ | CNTPCT => return Err(DeviceError::ReadOnly),
            _ => {
                return Err(DeviceError::OutOfRange {
                    addr: u64::from(addr),
                });
            }
        };
        result.map_err(|error| DeviceError::Backend {
            operation: "update AArch64 physical timer interrupt",
            detail: alloc::format!("{error}"),
        })
    }
}

impl Device for VirtualTimerBank {
    fn name(&self) -> &str {
        "aarch64-physical-timers"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::SysReg {
            return Err(DeviceError::InvalidInput {
                operation: "access AArch64 physical timer",
                detail: alloc::format!("expected system-register access, got {:?}", access.kind),
            });
        }
        if access.width != AccessWidth::Qword {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Qword,
                actual: access.width,
            });
        }
        let addr = u32::try_from(access.addr)
            .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
        if access.is_read {
            self.read(addr).map(|value| BusResponse::Read { value })
        } else {
            self.write(addr, access.data).map(|()| BusResponse::Write)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
