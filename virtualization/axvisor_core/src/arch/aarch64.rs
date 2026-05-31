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
                "4-level EPT feature is enabled, but the hardware only supports {}-level page \
                 tables. Please disable the 4-level EPT feature or use hardware that supports \
                 4-level page tables.",
                level
            );
        }
    }
    #[cfg(not(feature = "ept-level-4"))]
    {
        if level > 3 {
            panic!(
                "The hardware supports {}-level page tables, but the 4-level EPT feature is not \
                 enabled. Please enable the 4-level EPT feature to utilize the hardware's full \
                 capabilities.",
                level
            );
        }
    }
}
