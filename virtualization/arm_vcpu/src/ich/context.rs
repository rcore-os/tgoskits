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
const UNOWNED_HCR_MASK: u64 = !(OWNED_HCR_POLICY_MASK | HCR_EOI_COUNT_MASK);

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
    pub(crate) fn bind<R: IchRegisterAccess>(
        &mut self,
        cpu_id: usize,
        registers: &mut R,
        cpu_profile: IchCapabilityProfile,
    ) -> ArmVcpuResult {
        if let Some(bound_cpu) = self.bound_cpu {
            return Err(ArmVcpuError::IchVcpuAlreadyBound { cpu_id: bound_cpu });
        }

        self.initialize(cpu_profile);
        self.ensure_compatible(cpu_id, cpu_profile)?;
        self.restore(registers, cpu_profile)?;
        self.bound_cpu = Some(cpu_id);
        Ok(())
    }

    pub(crate) fn unbind<R: IchRegisterAccess>(
        &mut self,
        cpu_id: usize,
        registers: &mut R,
        cpu_profile: IchCapabilityProfile,
    ) -> ArmVcpuResult {
        match self.bound_cpu {
            None => return Err(ArmVcpuError::IchVcpuNotBound),
            Some(expected_cpu) if expected_cpu != cpu_id => {
                return Err(ArmVcpuError::IchVcpuCpuMismatch {
                    expected_cpu,
                    actual_cpu: cpu_id,
                });
            }
            Some(_) => {}
        }

        let result = self.save(registers, cpu_profile);
        self.bound_cpu = None;
        result
    }

    fn initialize(&mut self, profile: IchCapabilityProfile) {
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
        if let Err(error) = self.hcr_policy.validate(cpu_profile) {
            cleanup_local_interface(registers, cpu_profile);
            return Err(error);
        }

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

    pub(crate) fn with_bound_ich<R: IchRegisterAccess, T>(
        &mut self,
        cpu_id: usize,
        registers: &mut R,
        cpu_profile: IchCapabilityProfile,
        operation: impl for<'bound> FnOnce(&mut BoundIch<'bound, R>) -> ArmVcpuResult<T>,
    ) -> ArmVcpuResult<T> {
        match self.bound_cpu {
            None => return Err(ArmVcpuError::IchVcpuNotBound),
            Some(expected_cpu) if expected_cpu != cpu_id => {
                return Err(ArmVcpuError::IchVcpuCpuMismatch {
                    expected_cpu,
                    actual_cpu: cpu_id,
                });
            }
            Some(_) => {}
        }
        self.ensure_compatible(cpu_id, cpu_profile)?;

        operation(&mut BoundIch {
            context: self,
            registers,
            cpu_profile,
        })
    }

    fn ensure_compatible(&self, cpu_id: usize, cpu_profile: IchCapabilityProfile) -> ArmVcpuResult {
        let capacity = self.capacity.ok_or(ArmVcpuError::BadState)?;
        if capacity.is_supported_by(cpu_profile) {
            Ok(())
        } else {
            Err(ArmVcpuError::IncompatibleIchVcpuCapability {
                cpu_id,
                required_list_registers: capacity.list_register_count,
                required_priority_bits: capacity.priority_bits,
                required_preemption_bits: capacity.preemption_bits,
                available: cpu_profile,
            })
        }
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

/// Non-escaping access to the ICH registers owned by one bound vCPU.
pub(crate) struct BoundIch<'bound, R> {
    context: &'bound mut IchVcpuContext,
    registers: &'bound mut R,
    cpu_profile: IchCapabilityProfile,
}

impl<R: IchRegisterAccess> BoundIch<'_, R> {
    pub(crate) const fn capability(&self) -> IchCapabilityProfile {
        self.cpu_profile
    }

    pub(crate) fn read_lr(&mut self, slot: usize) -> ArmVcpuResult<u64> {
        self.ensure_lr_slot(slot)?;
        self.registers.read_list_register(slot)
    }

    pub(crate) fn write_lr(&mut self, slot: usize, value: u64) -> ArmVcpuResult {
        self.ensure_lr_slot(slot)?;
        self.registers.write_list_register(slot, value)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the PR3.3 refill path invalidates folded list registers"
        )
    )]
    pub(crate) fn invalidate_lr(&mut self, slot: usize) -> ArmVcpuResult {
        self.write_lr(slot, 0)
    }

    pub(crate) fn empty_lr_mask(&mut self) -> ArmVcpuResult<u16> {
        let implemented = self.capacity()?.list_register_count;
        let implemented_mask = if implemented == u16::BITS as usize {
            u16::MAX
        } else {
            (1u16 << implemented) - 1
        };
        Ok(self.registers.read_empty_list_register_status()? & implemented_mask)
    }

    pub(crate) fn update_hcr(&mut self, update: IchHcrUpdate) -> ArmVcpuResult {
        let policy = self.context.hcr_policy.updated(update);
        policy.validate(self.cpu_profile)?;
        self.registers.write_hcr(policy.raw())?;
        self.context.hcr_policy = policy;
        Ok(())
    }

    fn capacity(&self) -> ArmVcpuResult<IchContextCapacity> {
        self.context.capacity.ok_or(ArmVcpuError::BadState)
    }

    fn ensure_lr_slot(&self, slot: usize) -> ArmVcpuResult {
        if slot < self.capacity()?.list_register_count {
            Ok(())
        } else {
            Err(ArmVcpuError::UnsupportedListRegister { slot })
        }
    }
}

/// Typed changes allowed through the bound ICH access boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IchHcrUpdate {
    EnableInterface,
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
        let unowned_bits = raw_hcr & UNOWNED_HCR_MASK;
        let validation_error = if eoi_count != 0 {
            Some(ArmVcpuError::UnexpectedIchEoiCount { count: eoi_count })
        } else if unowned_bits != 0 {
            Some(ArmVcpuError::UnexpectedIchHcrBits { bits: unowned_bits })
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

    const fn updated(self, update: IchHcrUpdate) -> Self {
        match update {
            IchHcrUpdate::EnableInterface => Self(self.0 | HCR_ENABLE),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IchContextCapacity {
    list_register_count: usize,
    active_priority_register_count: usize,
    priority_bits: usize,
    preemption_bits: usize,
}

impl IchContextCapacity {
    const fn from_profile(profile: IchCapabilityProfile) -> Self {
        Self {
            list_register_count: profile.list_register_count(),
            active_priority_register_count: profile.active_priority_register_count(),
            priority_bits: profile.priority_bits(),
            preemption_bits: profile.preemption_bits(),
        }
    }

    const fn is_supported_by(self, profile: IchCapabilityProfile) -> bool {
        profile.list_register_count() >= self.list_register_count
            && profile.active_priority_register_count() == self.active_priority_register_count
            && profile.priority_bits() == self.priority_bits
            && profile.preemption_bits() == self.preemption_bits
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
    fn read_empty_list_register_status(&mut self) -> ArmVcpuResult<u16>;
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

    fn read_empty_list_register_status(&mut self) -> ArmVcpuResult<u16> {
        use arm_gic_driver::v3::{ICH_ELRSR_EL2, Readable};
        Ok(ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS) as u16)
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
#[path = "context_tests.rs"]
mod tests;
