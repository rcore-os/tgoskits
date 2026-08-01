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
    ReadElrsr,
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
    empty_lr_status: u16,
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
            empty_lr_status: 0,
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

    fn read_empty_list_register_status(&mut self) -> ArmVcpuResult<u16> {
        self.record(
            Event::ReadElrsr,
            IchRegisterOperation::ReadEmptyListRegisterStatus,
        )?;
        Ok(self.empty_lr_status)
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
fn save_rejects_unowned_hcr_configuration_after_cleanup() {
    let profile = profile();
    let mut context = IchVcpuContext::default();
    context.initialize(profile);
    let mut registers = FakeRegisters::populated();
    registers.hcr |= 1 << 2;

    assert_eq!(
        context.save(&mut registers, profile),
        Err(ArmVcpuError::UnexpectedIchHcrBits { bits: 1 << 2 })
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
fn unsupported_restore_policy_still_disables_and_clears_the_local_interface() {
    let profile = profile();
    let mut context = IchVcpuContext::default();
    context.initialize(profile);
    context.hcr_policy = IchHcrPolicy(HCR_ENABLE | HCR_TRAP_DEACTIVATE);
    let mut registers = FakeRegisters::populated();

    assert!(matches!(
        context.restore(&mut registers, profile),
        Err(ArmVcpuError::UnsupportedIchHcrPolicy { .. })
    ));
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

#[test]
fn bind_rejects_double_binding_and_unbind_rejects_wrong_cpu() {
    let profile = profile();
    let mut context = IchVcpuContext::default();
    let mut registers = FakeRegisters::populated();
    registers.hcr = 0;

    context.bind(1, &mut registers, profile).unwrap();
    assert_eq!(
        context.bind(1, &mut registers, profile),
        Err(ArmVcpuError::IchVcpuAlreadyBound { cpu_id: 1 })
    );
    assert_eq!(
        context.unbind(2, &mut registers, profile),
        Err(ArmVcpuError::IchVcpuCpuMismatch {
            expected_cpu: 1,
            actual_cpu: 2,
        })
    );
    assert_eq!(context.bound_cpu, Some(1));
}

#[test]
fn migration_rejects_a_cpu_with_fewer_lrs_than_first_bind() {
    let initial = profile();
    let smaller = IchCapabilityProfile::from_raw_vtr(1 | (4 << 26) | (4 << 29)).unwrap();
    let mut context = IchVcpuContext::default();
    let mut registers = FakeRegisters::populated();
    registers.hcr = 0;

    context.bind(0, &mut registers, initial).unwrap();
    context.unbind(0, &mut registers, initial).unwrap();

    assert!(matches!(
        context.bind(1, &mut registers, smaller),
        Err(ArmVcpuError::IncompatibleIchVcpuCapability {
            cpu_id: 1,
            required_list_registers: 4,
            ..
        })
    ));
    assert_eq!(context.bound_cpu, None);
}

#[test]
fn alternating_vcpus_preserve_isolated_ich_state() {
    let profile = profile();
    let mut first = IchVcpuContext::default();
    let mut second = IchVcpuContext::default();
    let mut registers = FakeRegisters::populated();
    registers.hcr = 0;
    registers.vmcr = 0;
    registers.lrs.fill(0);

    first.bind(0, &mut registers, profile).unwrap();
    registers.hcr = HCR_ENABLE;
    registers.vmcr = 0xa1;
    registers.lrs[0] = 0xa2;
    first.unbind(0, &mut registers, profile).unwrap();

    second.bind(0, &mut registers, profile).unwrap();
    assert_eq!(registers.vmcr, 0);
    assert_eq!(registers.lrs[0], 0);
    registers.vmcr = 0xb1;
    registers.lrs[0] = 0xb2;
    second.unbind(0, &mut registers, profile).unwrap();

    first.bind(0, &mut registers, profile).unwrap();
    assert_eq!(registers.hcr, HCR_ENABLE);
    assert_eq!(registers.vmcr, 0xa1);
    assert_eq!(registers.lrs[0], 0xa2);
}

#[test]
fn bound_ich_validates_owner_and_limits_register_access_to_vcpu_capacity() {
    let profile = profile();
    let mut context = IchVcpuContext::default();
    let mut registers = FakeRegisters::populated();
    registers.hcr = 0;
    context.bind(2, &mut registers, profile).unwrap();
    registers.lrs[0] = 0x1234;
    registers.empty_lr_status = u16::MAX;

    let result = context.with_bound_ich(2, &mut registers, profile, |bound| {
        assert_eq!(bound.capability(), profile);
        assert_eq!(bound.read_lr(0)?, 0x1234);
        bound.write_lr(1, 0x5678)?;
        bound.invalidate_lr(0)?;
        assert_eq!(bound.empty_lr_mask()?, 0b1111);
        bound.update_hcr(IchHcrUpdate::EnableInterface)?;
        assert_eq!(
            bound.read_lr(profile.list_register_count()),
            Err(ArmVcpuError::UnsupportedListRegister {
                slot: profile.list_register_count(),
            })
        );
        Ok(())
    });

    assert_eq!(result, Ok(()));
    assert_eq!(registers.lrs[0], 0);
    assert_eq!(registers.lrs[1], 0x5678);
    assert_eq!(registers.hcr, HCR_ENABLE);
    assert_eq!(
        context.with_bound_ich(1, &mut registers, profile, |_| Ok(())),
        Err(ArmVcpuError::IchVcpuCpuMismatch {
            expected_cpu: 2,
            actual_cpu: 1,
        })
    );
}

fn profile() -> IchCapabilityProfile {
    IchCapabilityProfile::from_raw_vtr(RAW_PROFILE).unwrap()
}
