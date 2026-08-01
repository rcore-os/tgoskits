// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{ArmVcpuError, ArmVcpuResult, IchCapabilityProfile, IchRegisterOperation};

const MAX_LIST_REGISTERS: usize = 16;
const MAX_ACTIVE_PRIORITY_REGISTERS: usize = 4;
const HCR_ENABLE: u64 = 1 << 0;
const HCR_UNDERFLOW_INTERRUPT_ENABLE: u64 = 1 << 1;
const HCR_TRAP_DEACTIVATE: u64 = 1 << 14;
const HCR_EOI_COUNT_SHIFT: u32 = 27;
const HCR_EOI_COUNT_MASK: u64 = 0x1f << HCR_EOI_COUNT_SHIFT;
const OWNED_HCR_POLICY_MASK: u64 =
    HCR_ENABLE | HCR_UNDERFLOW_INTERRUPT_ENABLE | HCR_TRAP_DEACTIVATE;

/// Saved ICH execution state belonging to one vCPU.
#[derive(Debug, Default)]
pub(crate) struct IchVcpuContext {
    hcr_policy: IchHcrPolicy,
    vmcr: u64,
    ap0r: [u64; MAX_ACTIVE_PRIORITY_REGISTERS],
    ap1r: [u64; MAX_ACTIVE_PRIORITY_REGISTERS],
    list_registers: [u64; MAX_LIST_REGISTERS],
    capacity: Option<IchContextCapacity>,
    initialized: bool,
    bound_cpu: Option<usize>,
}

impl IchVcpuContext {
    pub(crate) fn initialize(&mut self, profile: IchCapabilityProfile) {
        if self.capacity.is_none() {
            self.capacity = Some(IchContextCapacity::from_profile(profile));
            self.initialized = true;
        }
    }

    pub(crate) fn restore<R: IchRegisterAccess>(
        &self,
        registers: &mut R,
        cpu_profile: IchCapabilityProfile,
    ) -> ArmVcpuResult {
        let capacity = self.capacity.ok_or(ArmVcpuError::BadState)?;
        self.hcr_policy.validate(cpu_profile)?;

        let result = self.restore_disabled(registers, capacity, cpu_profile);
        if let Err(error) = result {
            cleanup_local_interface(registers, cpu_profile);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn save<R: IchRegisterAccess>(
        &mut self,
        registers: &mut R,
        cpu_profile: IchCapabilityProfile,
    ) -> ArmVcpuResult {
        let capacity = self.capacity.ok_or(ArmVcpuError::BadState)?;
        let raw_hcr = match registers.read_hcr() {
            Ok(raw_hcr) => raw_hcr,
            Err(error) => {
                cleanup_local_interface(registers, cpu_profile);
                return Err(error);
            }
        };
        let (hcr_policy, validation_error) = IchHcrPolicy::from_observed(raw_hcr, cpu_profile);

        let mut snapshot = IchSnapshot::default();
        let save_result = snapshot.read_disabled(registers, capacity);
        cleanup_local_interface(registers, cpu_profile);

        if save_result.is_ok() {
            self.hcr_policy = hcr_policy;
            self.vmcr = snapshot.vmcr;
            self.ap0r = snapshot.ap0r;
            self.ap1r = snapshot.ap1r;
            self.list_registers = snapshot.list_registers;
        }

        if let Some(error) = validation_error {
            return Err(error);
        }
        save_result
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    fn restore_disabled<R: IchRegisterAccess>(
        &self,
        registers: &mut R,
        capacity: IchContextCapacity,
        cpu_profile: IchCapabilityProfile,
    ) -> ArmVcpuResult {
        registers.write_hcr(0)?;
        registers.write_vmcr(self.vmcr)?;
        for slot in 0..capacity.active_priority_register_count {
            registers.write_ap0r(slot, self.ap0r[slot])?;
            registers.write_ap1r(slot, self.ap1r[slot])?;
        }
        for slot in 0..capacity.list_register_count {
            registers.write_list_register(slot, self.list_registers[slot])?;
        }
        for slot in capacity.list_register_count..cpu_profile.list_register_count() {
            registers.write_list_register(slot, 0)?;
        }
        registers.write_hcr(self.hcr_policy.raw())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct IchHcrPolicy(u64);

impl IchHcrPolicy {
    fn from_observed(
        raw_hcr: u64,
        cpu_profile: IchCapabilityProfile,
    ) -> (Self, Option<ArmVcpuError>) {
        let policy = Self(raw_hcr & OWNED_HCR_POLICY_MASK);
        let eoi_count = ((raw_hcr & HCR_EOI_COUNT_MASK) >> HCR_EOI_COUNT_SHIFT) as u8;
        let validation_error = if eoi_count != 0 {
            Some(ArmVcpuError::UnexpectedIchEoiCount { count: eoi_count })
        } else {
            policy.validate(cpu_profile).err()
        };
        (policy, validation_error)
    }

    fn validate(self, cpu_profile: IchCapabilityProfile) -> ArmVcpuResult {
        if self.0 & HCR_TRAP_DEACTIVATE != 0 && !cpu_profile.supports_tdir() {
            Err(ArmVcpuError::UnsupportedIchHcrPolicy {
                policy: self.0,
                capability: cpu_profile,
            })
        } else {
            Ok(())
        }
    }

    const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IchContextCapacity {
    list_register_count: usize,
    active_priority_register_count: usize,
}

impl IchContextCapacity {
    const fn from_profile(profile: IchCapabilityProfile) -> Self {
        Self {
            list_register_count: profile.list_register_count(),
            active_priority_register_count: profile.active_priority_register_count(),
        }
    }
}

#[derive(Debug, Default)]
struct IchSnapshot {
    vmcr: u64,
    ap0r: [u64; MAX_ACTIVE_PRIORITY_REGISTERS],
    ap1r: [u64; MAX_ACTIVE_PRIORITY_REGISTERS],
    list_registers: [u64; MAX_LIST_REGISTERS],
}

impl IchSnapshot {
    fn read_disabled<R: IchRegisterAccess>(
        &mut self,
        registers: &mut R,
        capacity: IchContextCapacity,
    ) -> ArmVcpuResult {
        registers.write_hcr(0)?;
        for slot in 0..capacity.list_register_count {
            self.list_registers[slot] = registers.read_list_register(slot)?;
        }
        for slot in 0..capacity.active_priority_register_count {
            self.ap0r[slot] = registers.read_ap0r(slot)?;
            self.ap1r[slot] = registers.read_ap1r(slot)?;
        }
        self.vmcr = registers.read_vmcr()?;
        Ok(())
    }
}

pub(crate) trait IchRegisterAccess {
    fn read_hcr(&mut self) -> ArmVcpuResult<u64>;
    fn write_hcr(&mut self, value: u64) -> ArmVcpuResult;
    fn read_vmcr(&mut self) -> ArmVcpuResult<u64>;
    fn write_vmcr(&mut self, value: u64) -> ArmVcpuResult;
    fn read_ap0r(&mut self, slot: usize) -> ArmVcpuResult<u64>;
    fn write_ap0r(&mut self, slot: usize, value: u64) -> ArmVcpuResult;
    fn read_ap1r(&mut self, slot: usize) -> ArmVcpuResult<u64>;
    fn write_ap1r(&mut self, slot: usize, value: u64) -> ArmVcpuResult;
    fn read_list_register(&mut self, slot: usize) -> ArmVcpuResult<u64>;
    fn write_list_register(&mut self, slot: usize, value: u64) -> ArmVcpuResult;
}

fn cleanup_local_interface<R: IchRegisterAccess>(
    registers: &mut R,
    cpu_profile: IchCapabilityProfile,
) {
    let _ = registers.write_hcr(0);
    for slot in 0..cpu_profile.list_register_count() {
        let _ = registers.write_list_register(slot, 0);
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) struct HardwareIchRegisters;

#[cfg(target_arch = "aarch64")]
impl IchRegisterAccess for HardwareIchRegisters {
    fn read_hcr(&mut self) -> ArmVcpuResult<u64> {
        use arm_gic_driver::v3::{ICH_HCR_EL2, Readable};
        Ok(ICH_HCR_EL2.get())
    }

    fn write_hcr(&mut self, value: u64) -> ArmVcpuResult {
        use arm_gic_driver::v3::{ICH_HCR_EL2, Writeable};
        ICH_HCR_EL2.set(value);
        Ok(())
    }

    fn read_vmcr(&mut self) -> ArmVcpuResult<u64> {
        use arm_gic_driver::v3::{ICH_VMCR_EL2, Readable};
        Ok(ICH_VMCR_EL2.get())
    }

    fn write_vmcr(&mut self, value: u64) -> ArmVcpuResult {
        use arm_gic_driver::v3::{ICH_VMCR_EL2, Writeable};
        ICH_VMCR_EL2.set(value);
        Ok(())
    }

    fn read_ap0r(&mut self, slot: usize) -> ArmVcpuResult<u64> {
        read_ap0r(slot)
    }

    fn write_ap0r(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
        write_ap0r(slot, value)
    }

    fn read_ap1r(&mut self, slot: usize) -> ArmVcpuResult<u64> {
        read_ap1r(slot)
    }

    fn write_ap1r(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
        write_ap1r(slot, value)
    }

    fn read_list_register(&mut self, slot: usize) -> ArmVcpuResult<u64> {
        use arm_gic_driver::v3::ich_lr_el2_get;
        Ok(ich_lr_el2_get(slot).get())
    }

    fn write_list_register(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
        arm_gic_driver::v3::ich_lr_el2_set_raw(slot, value);
        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
fn read_ap0r(slot: usize) -> ArmVcpuResult<u64> {
    use arm_gic_driver::v3::{
        ICH_AP0R0_EL2, ICH_AP0R1_EL2, ICH_AP0R2_EL2, ICH_AP0R3_EL2, Readable,
    };
    match slot {
        0 => Ok(ICH_AP0R0_EL2.get()),
        1 => Ok(ICH_AP0R1_EL2.get()),
        2 => Ok(ICH_AP0R2_EL2.get()),
        3 => Ok(ICH_AP0R3_EL2.get()),
        _ => Err(register_error(IchRegisterOperation::ReadAp0r(slot))),
    }
}

#[cfg(target_arch = "aarch64")]
fn write_ap0r(slot: usize, value: u64) -> ArmVcpuResult {
    use arm_gic_driver::v3::{
        ICH_AP0R0_EL2, ICH_AP0R1_EL2, ICH_AP0R2_EL2, ICH_AP0R3_EL2, Writeable,
    };
    match slot {
        0 => ICH_AP0R0_EL2.set(value),
        1 => ICH_AP0R1_EL2.set(value),
        2 => ICH_AP0R2_EL2.set(value),
        3 => ICH_AP0R3_EL2.set(value),
        _ => return Err(register_error(IchRegisterOperation::WriteAp0r(slot))),
    }
    Ok(())
}

#[cfg(target_arch = "aarch64")]
fn read_ap1r(slot: usize) -> ArmVcpuResult<u64> {
    use arm_gic_driver::v3::{
        ICH_AP1R0_EL2, ICH_AP1R1_EL2, ICH_AP1R2_EL2, ICH_AP1R3_EL2, Readable,
    };
    match slot {
        0 => Ok(ICH_AP1R0_EL2.get()),
        1 => Ok(ICH_AP1R1_EL2.get()),
        2 => Ok(ICH_AP1R2_EL2.get()),
        3 => Ok(ICH_AP1R3_EL2.get()),
        _ => Err(register_error(IchRegisterOperation::ReadAp1r(slot))),
    }
}

#[cfg(target_arch = "aarch64")]
fn write_ap1r(slot: usize, value: u64) -> ArmVcpuResult {
    use arm_gic_driver::v3::{
        ICH_AP1R0_EL2, ICH_AP1R1_EL2, ICH_AP1R2_EL2, ICH_AP1R3_EL2, Writeable,
    };
    match slot {
        0 => ICH_AP1R0_EL2.set(value),
        1 => ICH_AP1R1_EL2.set(value),
        2 => ICH_AP1R2_EL2.set(value),
        3 => ICH_AP1R3_EL2.set(value),
        _ => return Err(register_error(IchRegisterOperation::WriteAp1r(slot))),
    }
    Ok(())
}

fn register_error(operation: IchRegisterOperation) -> ArmVcpuError {
    ArmVcpuError::IchRegisterAccess { operation }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    const RAW_PROFILE: u64 = 3 | (4 << 26) | (4 << 29);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Event {
        ReadHcr,
        WriteHcr(u64),
        ReadVmcr,
        WriteVmcr(u64),
        ReadAp0r(usize),
        WriteAp0r(usize, u64),
        ReadAp1r(usize),
        WriteAp1r(usize, u64),
        ReadLr(usize),
        WriteLr(usize, u64),
    }

    struct FakeRegisters {
        hcr: u64,
        vmcr: u64,
        ap0r: [u64; 4],
        ap1r: [u64; 4],
        lrs: [u64; 16],
        events: Vec<Event>,
        fail_at: Option<usize>,
        operation_count: usize,
    }

    impl FakeRegisters {
        fn populated() -> Self {
            Self {
                hcr: HCR_ENABLE,
                vmcr: 0x55,
                ap0r: [0x10, 0x11, 0x12, 0x13],
                ap1r: [0x20, 0x21, 0x22, 0x23],
                lrs: core::array::from_fn(|slot| 0x100 + slot as u64),
                events: Vec::new(),
                fail_at: None,
                operation_count: 0,
            }
        }

        fn record(&mut self, event: Event, operation: IchRegisterOperation) -> ArmVcpuResult {
            self.events.push(event);
            let current = self.operation_count;
            self.operation_count += 1;
            if self.fail_at == Some(current) {
                Err(register_error(operation))
            } else {
                Ok(())
            }
        }
    }

    impl IchRegisterAccess for FakeRegisters {
        fn read_hcr(&mut self) -> ArmVcpuResult<u64> {
            self.record(Event::ReadHcr, IchRegisterOperation::ReadHcr)?;
            Ok(self.hcr)
        }

        fn write_hcr(&mut self, value: u64) -> ArmVcpuResult {
            self.record(Event::WriteHcr(value), IchRegisterOperation::WriteHcr)?;
            self.hcr = value;
            Ok(())
        }

        fn read_vmcr(&mut self) -> ArmVcpuResult<u64> {
            self.record(Event::ReadVmcr, IchRegisterOperation::ReadVmcr)?;
            Ok(self.vmcr)
        }

        fn write_vmcr(&mut self, value: u64) -> ArmVcpuResult {
            self.record(Event::WriteVmcr(value), IchRegisterOperation::WriteVmcr)?;
            self.vmcr = value;
            Ok(())
        }

        fn read_ap0r(&mut self, slot: usize) -> ArmVcpuResult<u64> {
            self.record(Event::ReadAp0r(slot), IchRegisterOperation::ReadAp0r(slot))?;
            Ok(self.ap0r[slot])
        }

        fn write_ap0r(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
            self.record(
                Event::WriteAp0r(slot, value),
                IchRegisterOperation::WriteAp0r(slot),
            )?;
            self.ap0r[slot] = value;
            Ok(())
        }

        fn read_ap1r(&mut self, slot: usize) -> ArmVcpuResult<u64> {
            self.record(Event::ReadAp1r(slot), IchRegisterOperation::ReadAp1r(slot))?;
            Ok(self.ap1r[slot])
        }

        fn write_ap1r(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
            self.record(
                Event::WriteAp1r(slot, value),
                IchRegisterOperation::WriteAp1r(slot),
            )?;
            self.ap1r[slot] = value;
            Ok(())
        }

        fn read_list_register(&mut self, slot: usize) -> ArmVcpuResult<u64> {
            self.record(
                Event::ReadLr(slot),
                IchRegisterOperation::ReadListRegister(slot),
            )?;
            Ok(self.lrs[slot])
        }

        fn write_list_register(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
            self.record(
                Event::WriteLr(slot, value),
                IchRegisterOperation::WriteListRegister(slot),
            )?;
            self.lrs[slot] = value;
            Ok(())
        }
    }

    #[test]
    fn restore_enables_hcr_only_after_vmcr_aprs_and_lrs() {
        let profile = profile();
        let mut context = IchVcpuContext::default();
        context.initialize(profile);
        context.hcr_policy = IchHcrPolicy(HCR_ENABLE);
        context.vmcr = 0x55;
        context.ap0r[0] = 0x10;
        context.ap1r[0] = 0x20;
        context.list_registers[..4].copy_from_slice(&[1, 2, 3, 4]);
        let mut registers = FakeRegisters::populated();
        registers.events.clear();

        context.restore(&mut registers, profile).unwrap();

        assert_eq!(registers.events.first(), Some(&Event::WriteHcr(0)));
        assert_eq!(registers.events.get(1), Some(&Event::WriteVmcr(0x55)));
        assert_eq!(registers.events.last(), Some(&Event::WriteHcr(HCR_ENABLE)));
    }

    #[test]
    fn save_disables_before_reading_state_and_clears_lrs() {
        let profile = profile();
        let mut context = IchVcpuContext::default();
        context.initialize(profile);
        let mut registers = FakeRegisters::populated();

        context.save(&mut registers, profile).unwrap();

        assert_eq!(registers.events[0], Event::ReadHcr);
        assert_eq!(registers.events[1], Event::WriteHcr(0));
        assert!(registers.lrs[..4].iter().all(|value| *value == 0));
        assert_eq!(registers.hcr, 0);
        assert_eq!(context.hcr_policy, IchHcrPolicy(HCR_ENABLE));
        assert_eq!(context.vmcr, 0x55);
    }

    #[test]
    fn save_reports_eoi_count_after_disabling_and_clearing() {
        let profile = profile();
        let mut context = IchVcpuContext::default();
        context.initialize(profile);
        let mut registers = FakeRegisters::populated();
        registers.hcr |= 3 << HCR_EOI_COUNT_SHIFT;

        assert_eq!(
            context.save(&mut registers, profile),
            Err(ArmVcpuError::UnexpectedIchEoiCount { count: 3 })
        );
        assert_eq!(registers.hcr, 0);
        assert!(registers.lrs[..4].iter().all(|value| *value == 0));
    }

    #[test]
    fn restore_failure_still_disables_and_clears_the_local_interface() {
        let profile = profile();
        let mut context = IchVcpuContext::default();
        context.initialize(profile);
        context.hcr_policy = IchHcrPolicy(HCR_ENABLE);
        let mut registers = FakeRegisters::populated();
        registers.fail_at = Some(2);

        assert!(context.restore(&mut registers, profile).is_err());
        assert_eq!(registers.hcr, 0);
        assert!(registers.lrs[..4].iter().all(|value| *value == 0));
    }

    #[test]
    fn save_failure_still_disables_and_clears_the_local_interface() {
        let profile = profile();
        let mut context = IchVcpuContext::default();
        context.initialize(profile);
        let mut registers = FakeRegisters::populated();
        registers.fail_at = Some(3);

        assert!(context.save(&mut registers, profile).is_err());
        assert_eq!(registers.hcr, 0);
        assert!(registers.lrs[..4].iter().all(|value| *value == 0));
    }

    #[test]
    fn reset_clears_execution_state_capacity_and_owner() {
        let mut context = IchVcpuContext::default();
        context.initialize(profile());
        context.hcr_policy = IchHcrPolicy(HCR_ENABLE);
        context.vmcr = 1;
        context.list_registers[0] = 2;
        context.bound_cpu = Some(3);

        context.reset();

        assert_eq!(context.hcr_policy, IchHcrPolicy::default());
        assert_eq!(context.vmcr, 0);
        assert!(context.list_registers.iter().all(|value| *value == 0));
        assert_eq!(context.capacity, None);
        assert!(!context.initialized);
        assert_eq!(context.bound_cpu, None);
    }

    fn profile() -> IchCapabilityProfile {
        IchCapabilityProfile::from_raw_vtr(RAW_PROFILE).unwrap()
    }
}
