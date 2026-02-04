// Copyright 2025 The Axvisor Team
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

use aarch64_cpu_ext::registers::*;

mod api;
pub mod cache;

pub fn inject_interrupt(irq: usize) {
    debug!("Injecting virtual interrupt: {irq}");

    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("Failed to get GIC driver")
        .lock()
        .unwrap();
    if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
        use arm_gic_driver::{
            IntId,
            v2::{VirtualInterruptConfig, VirtualInterruptState},
        };

        let gich = gic.hypervisor_interface().expect("Failed to get GICH");
        gich.enable();
        gich.set_virtual_interrupt(
            0,
            VirtualInterruptConfig::software(
                unsafe { IntId::raw(irq as _) },
                None,
                0,
                VirtualInterruptState::Pending,
                false,
                true,
            ),
        );
        return;
    }

    if let Some(_gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
        inject_interrupt_gic_v3(irq as _);
        return;
    }

    panic!("no gic driver found")
}

pub fn inject_interrupt_gic_v3(vector: usize) {
    use arm_gic_driver::v3::*;

    debug!("Injecting virtual interrupt: vector={vector}");
    let elsr = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS);
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;

    let mut free_lr = -1_isize;

    // First, check if this interrupt is already pending/active
    for i in 0..lr_num {
        // find a free list register
        if (1 << i) & elsr > 0 {
            if free_lr == -1 {
                free_lr = i as isize;
            }
            continue;
        }
        let lr_val = ich_lr_el2_get(i);

        if lr_val.read(ICH_LR_EL2::VINTID) == vector as u64
            && lr_val.matches_any(&[ICH_LR_EL2::STATE::Pending, ICH_LR_EL2::STATE::Active])
        {
            debug!("Virtual interrupt {vector} already pending/active in LR{i}, skipping");
            // If the interrupt is already pending or active, we can skip injecting it again.
            // This is important to avoid duplicate injections.
            continue;
        }
    }

    debug!("use free lr {free_lr} to inject irq {vector}");

    if free_lr == -1 {
        warn!("No free list register to inject IRQ {vector}, checking ICH_HCR_EL2");

        // Try to find and reuse an inactive LR
        for i in 0..lr_num {
            let lr_val = ich_lr_el2_get(i);
            if lr_val.matches_any(&[ICH_LR_EL2::STATE::Invalid]) {
                debug!("Reusing inactive LR{i} for IRQ {vector}");
                free_lr = i as isize;

                break;
            }
        }

        if free_lr == -1 {
            panic!("No free list register to inject IRQ {}", vector);
        }
    }

    ich_lr_el2_write(
        free_lr as _,
        ICH_LR_EL2::VINTID.val(vector as u64) + ICH_LR_EL2::STATE::Pending + ICH_LR_EL2::GROUP::SET,
    );

    // Ensure the virtual interrupt interface is enabled
    let en = ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN);
    if !en {
        // Check EN bit
        warn!("Virtual interrupt interface not enabled, enabling now");
        ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
    }

    debug!("Virtual interrupt {vector} injected successfully in LR{free_lr}");
}

pub fn hardware_check() {
    let pa_bits = match ID_AA64MMFR0_EL1.read_as_enum(ID_AA64MMFR0_EL1::PARange) {
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_32) => 32,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_36) => 36,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_40) => 40,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_42) => 42,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_44) => 44,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_48) => 48,
        Some(ID_AA64MMFR0_EL1::PARange::Value::Bits_52) => 52,
        _ => 32,
    };

    let level = match pa_bits {
        44.. => 4,
        _ => 3,
    };

    #[cfg(feature = "ept-level-4")]
    {
        if level < 4 {
            panic!(
                "4-level EPT feature is enabled, but the hardware only supports {}-level page tables. Please disable the 4-level EPT feature or use hardware that supports 4-level page tables.",
                level
            );
        }
    }
    #[cfg(not(feature = "ept-level-4"))]
    {
        if level > 3 {
            panic!(
                "The hardware supports {}-level page tables, but the 4-level EPT feature is not enabled. Please enable the 4-level EPT feature to utilize the hardware's full capabilities.",
                level
            );
        }
    }
}
