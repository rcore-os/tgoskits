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

use arm_vcpu::{
    ArmVcpuError, ArmVirtualIntId, IchDirectInjection, IchLrEntry, IchLrState,
    plan_direct_injection,
};

#[test]
fn virtual_interrupt_ids_preserve_the_traditional_intid_range() {
    for value in [0, 31, 32, 255, 256, 1019] {
        let intid = ArmVirtualIntId::try_from(value).unwrap();
        assert_eq!(intid.as_u32(), value as u32);
    }
}

#[test]
fn virtual_interrupt_ids_reject_reserved_and_unrepresentable_values() {
    assert_eq!(
        ArmVirtualIntId::try_from(1020),
        Err(ArmVcpuError::InvalidVirtualInterruptId { value: 1020 })
    );
    assert_eq!(
        ArmVirtualIntId::try_from(usize::MAX),
        Err(ArmVcpuError::InvalidVirtualInterruptId { value: usize::MAX })
    );
}

#[test]
fn software_list_registers_use_the_architectural_bit_positions() {
    let entry = IchLrEntry::Software {
        intid: ArmVirtualIntId::new(256).unwrap(),
        state: IchLrState::ActivePending,
        priority: 0xa5,
        group1: true,
        eoi: true,
    };

    let raw = entry.encode();
    assert_eq!(raw & u32::MAX as u64, 256);
    assert_eq!((raw >> 41) & 1, 1);
    assert_eq!((raw >> 48) & 0xff, 0xa5);
    assert_eq!((raw >> 60) & 1, 1);
    assert_eq!((raw >> 61) & 1, 0);
    assert_eq!((raw >> 62) & 0b11, 0b11);
    assert_eq!(IchLrEntry::decode(3, raw), Ok(entry));
}

#[test]
fn software_list_registers_round_trip_every_valid_state() {
    for state in [
        IchLrState::Pending,
        IchLrState::Active,
        IchLrState::ActivePending,
    ] {
        let entry = IchLrEntry::Software {
            intid: ArmVirtualIntId::new(1019).unwrap(),
            state,
            priority: 0x80,
            group1: false,
            eoi: true,
        };

        assert_eq!(IchLrEntry::decode(15, entry.encode()), Ok(entry));
    }
}

#[test]
fn invalid_list_registers_ignore_residual_identity_fields() {
    let raw = u32::MAX as u64 | (1 << 61) | (1 << 41);
    assert_eq!(IchLrEntry::decode(0, raw), Ok(IchLrEntry::Invalid));
    assert_eq!(IchLrEntry::Invalid.encode(), 0);
}

#[test]
fn hardware_and_malformed_software_list_registers_are_rejected() {
    let pending = 1_u64 << 62;
    assert_eq!(
        IchLrEntry::decode(4, pending | (1 << 61)),
        Err(ArmVcpuError::UnsupportedListRegister { slot: 4 })
    );
    assert_eq!(
        IchLrEntry::decode(5, pending | 1020),
        Err(ArmVcpuError::MalformedListRegister { slot: 5 })
    );
}

#[test]
fn direct_injection_reports_a_full_lr_set_without_panicking() {
    let requested = ArmVirtualIntId::new(40).unwrap();
    let occupied = IchLrEntry::Software {
        intid: ArmVirtualIntId::new(41).unwrap(),
        state: IchLrState::Active,
        priority: 0,
        group1: true,
        eoi: false,
    }
    .encode();

    assert_eq!(
        plan_direct_injection(requested, 0, &[occupied; 16]),
        Err(ArmVcpuError::NoFreeListRegister { intid: requested })
    );
}

#[test]
fn direct_injection_requires_elrsr_and_invalid_state_to_agree() {
    let requested = ArmVirtualIntId::new(40).unwrap();
    let occupied = IchLrEntry::Software {
        intid: ArmVirtualIntId::new(41).unwrap(),
        state: IchLrState::Active,
        priority: 0,
        group1: true,
        eoi: false,
    }
    .encode();

    assert_eq!(
        plan_direct_injection(requested, 0b11, &[occupied, 0]),
        Ok(IchDirectInjection::Vacant(1))
    );
}

#[test]
fn direct_injection_folds_an_existing_software_intid() {
    let requested = ArmVirtualIntId::new(40).unwrap();
    let resident = IchLrEntry::Software {
        intid: requested,
        state: IchLrState::ActivePending,
        priority: 0,
        group1: true,
        eoi: false,
    }
    .encode();

    assert_eq!(
        plan_direct_injection(requested, 0b10, &[resident, 0]),
        Ok(IchDirectInjection::AlreadyPresent)
    );
}

#[test]
fn direct_injection_skips_hardware_list_registers() {
    let requested = ArmVirtualIntId::new(40).unwrap();
    let hardware_pending = (1_u64 << 62) | (1 << 61) | requested.as_u32() as u64;

    assert_eq!(
        plan_direct_injection(requested, 0b10, &[hardware_pending, 0]),
        Ok(IchDirectInjection::Vacant(1))
    );
}

#[test]
fn direct_injection_rejects_invalid_list_register_counts() {
    let requested = ArmVirtualIntId::new(40).unwrap();

    assert_eq!(
        plan_direct_injection(requested, 0, &[]),
        Err(ArmVcpuError::InvalidListRegisterCount { count: 0 })
    );
    assert_eq!(
        plan_direct_injection(requested, 0, &[0; 17]),
        Err(ArmVcpuError::InvalidListRegisterCount { count: 17 })
    );
}
