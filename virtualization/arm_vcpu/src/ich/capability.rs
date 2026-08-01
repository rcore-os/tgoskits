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

use crate::{ArmVcpuError, ArmVcpuResult, ArmVirtualIntId};

const LIST_REGISTERS_MASK: u64 = 0x1f;
const RESERVED_LOW_MASK: u64 = 0x1fff << 5;
const TDS_BIT: u64 = 1 << 19;
const ID_BITS_SHIFT: u32 = 23;
const THREE_BIT_MASK: u64 = 0b111;
const PREEMPTION_BITS_SHIFT: u32 = 26;
const PRIORITY_BITS_SHIFT: u32 = 29;
const RESERVED_HIGH_MASK: u64 = (u32::MAX as u64) << 32;

/// Validated capabilities of one Arm GIC virtualization CPU interface.
///
/// Instances can only be obtained from a checked `ICH_VTR_EL2` decode or from
/// capability discovery helpers. Accessors report actual counts rather than
/// the architecture's minus-one encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IchCapabilityProfile {
    list_register_count: u8,
    virtual_intid_bits: u8,
    priority_bits: u8,
    preemption_bits: u8,
    active_priority_register_count: u8,
    supports_tdir: bool,
}

impl IchCapabilityProfile {
    /// Decodes and validates a raw `ICH_VTR_EL2` value.
    ///
    /// # Errors
    ///
    /// Returns [`ArmVcpuError::InvalidIchCapability`] for reserved encodings,
    /// nonzero RES0 bits, or a priority shape that cannot be represented by
    /// the architectural AP0R/AP1R register banks.
    pub fn from_raw_vtr(raw_vtr: u64) -> ArmVcpuResult<Self> {
        Self::decode(raw_vtr)
            .map_err(|reason| ArmVcpuError::InvalidIchCapability { raw_vtr, reason })
    }

    /// Number of implemented ICH list registers.
    pub const fn list_register_count(self) -> usize {
        self.list_register_count as usize
    }

    /// Number of virtual interrupt identifier bits.
    pub const fn virtual_intid_bits(self) -> usize {
        self.virtual_intid_bits as usize
    }

    /// Number of implemented virtual priority bits.
    pub const fn priority_bits(self) -> usize {
        self.priority_bits as usize
    }

    /// Number of implemented virtual preemption bits.
    pub const fn preemption_bits(self) -> usize {
        self.preemption_bits as usize
    }

    /// Number of implemented registers in each AP0R and AP1R bank.
    pub const fn active_priority_register_count(self) -> usize {
        self.active_priority_register_count as usize
    }

    /// Whether `ICH_HCR_EL2.TDIR` is implemented.
    pub const fn supports_tdir(self) -> bool {
        self.supports_tdir
    }

    fn decode(raw_vtr: u64) -> Result<Self, IchCapabilityError> {
        let reserved_bits = raw_vtr & (RESERVED_LOW_MASK | RESERVED_HIGH_MASK);
        if reserved_bits != 0 {
            return Err(IchCapabilityError::ReservedBits { reserved_bits });
        }

        let encoded_list_registers = (raw_vtr & LIST_REGISTERS_MASK) as u8;
        if encoded_list_registers > 15 {
            return Err(IchCapabilityError::ListRegisterEncoding {
                encoded: encoded_list_registers,
            });
        }

        let encoded_id_bits = field(raw_vtr, ID_BITS_SHIFT);
        let virtual_intid_bits = match encoded_id_bits {
            0 => 16,
            1 => 24,
            encoded => return Err(IchCapabilityError::VirtualIntidEncoding { encoded }),
        };
        if virtual_intid_bits < required_intid_bits(ArmVirtualIntId::MAX) {
            return Err(IchCapabilityError::InsufficientVirtualIntidBits {
                actual: virtual_intid_bits,
                required: required_intid_bits(ArmVirtualIntId::MAX),
            });
        }

        let encoded_priority_bits = field(raw_vtr, PRIORITY_BITS_SHIFT);
        if !(4..=6).contains(&encoded_priority_bits) {
            return Err(IchCapabilityError::PriorityEncoding {
                encoded: encoded_priority_bits,
            });
        }
        let priority_bits = encoded_priority_bits + 1;

        let encoded_preemption_bits = field(raw_vtr, PREEMPTION_BITS_SHIFT);
        if encoded_preemption_bits > 6 {
            return Err(IchCapabilityError::PreemptionEncoding {
                encoded: encoded_preemption_bits,
            });
        }
        let preemption_bits = encoded_preemption_bits + 1;
        if preemption_bits < 5 || preemption_bits > priority_bits {
            return Err(IchCapabilityError::InvalidPriorityShape {
                priority_bits,
                preemption_bits,
            });
        }

        let active_priority_register_count = 1u8 << (priority_bits - 5);
        if active_priority_register_count > 4 {
            return Err(IchCapabilityError::ActivePriorityRegisterCount {
                count: active_priority_register_count,
            });
        }

        Ok(Self {
            list_register_count: encoded_list_registers + 1,
            virtual_intid_bits,
            priority_bits,
            preemption_bits,
            active_priority_register_count,
            supports_tdir: raw_vtr & TDS_BIT != 0,
        })
    }
}

/// Architectural reason why an `ICH_VTR_EL2` value was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IchCapabilityError {
    /// LISTREGS used a value above the architectural maximum of 15.
    #[error("reserved LISTREGS encoding {encoded:#x}")]
    ListRegisterEncoding { encoded: u8 },
    /// IDBITS used an encoding other than 16-bit or 24-bit INTIDs.
    #[error("reserved IDBITS encoding {encoded:#x}")]
    VirtualIntidEncoding { encoded: u8 },
    /// The reported INTID width cannot represent the supported INTID range.
    #[error("{actual} ID bits cannot represent the required {required} bits")]
    InsufficientVirtualIntidBits { actual: u8, required: u8 },
    /// PRIbits used an encoding outside the architectural 5-7 bit range.
    #[error("reserved PRIbits encoding {encoded:#x}")]
    PriorityEncoding { encoded: u8 },
    /// PREbits used the reserved encoding 7.
    #[error("reserved PREbits encoding {encoded:#x}")]
    PreemptionEncoding { encoded: u8 },
    /// Priority and preemption fields form an invalid architectural shape.
    #[error(
        "invalid priority shape: {priority_bits} priority bits, {preemption_bits} preemption bits"
    )]
    InvalidPriorityShape {
        priority_bits: u8,
        preemption_bits: u8,
    },
    /// The priority shape would require more AP registers than the wrapper has.
    #[error("priority shape requires {count} active-priority registers per bank")]
    ActivePriorityRegisterCount { count: u8 },
    /// A RES0 bit was nonzero.
    #[error("reserved bits are nonzero: {reserved_bits:#x}")]
    ReservedBits { reserved_bits: u64 },
}

const fn field(raw: u64, shift: u32) -> u8 {
    ((raw >> shift) & THREE_BIT_MASK) as u8
}

const fn required_intid_bits(max_intid: u32) -> u8 {
    (u32::BITS - max_intid.leading_zeros()) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_one_and_sixteen_list_registers() {
        assert_eq!(valid_profile(0, 4, 4, 0).list_register_count(), 1);
        assert_eq!(valid_profile(15, 4, 4, 0).list_register_count(), 16);
    }

    #[test]
    fn decodes_both_architectural_intid_widths() {
        assert_eq!(valid_profile(3, 4, 4, 0).virtual_intid_bits(), 16);
        assert_eq!(valid_profile(3, 4, 4, 1).virtual_intid_bits(), 24);
    }

    #[test]
    fn converts_priority_and_preemption_encodings_to_actual_bits() {
        let profile = valid_profile(3, 6, 5, 0);
        assert_eq!(profile.priority_bits(), 7);
        assert_eq!(profile.preemption_bits(), 6);
        assert_eq!(profile.active_priority_register_count(), 4);
    }

    #[test]
    fn decodes_tdir_support() {
        assert!(!valid_profile(3, 4, 4, 0).supports_tdir());
        assert!(
            IchCapabilityProfile::from_raw_vtr(raw_vtr(3, 4, 4, 0) | TDS_BIT)
                .unwrap()
                .supports_tdir()
        );
    }

    #[test]
    fn rejects_reserved_field_encodings_and_res0_bits() {
        for raw in [
            raw_vtr(16, 4, 4, 0),
            raw_vtr(3, 4, 4, 2),
            raw_vtr(3, 7, 4, 0),
            raw_vtr(3, 4, 7, 0),
            raw_vtr(3, 4, 4, 0) | (1 << 5),
            raw_vtr(3, 4, 4, 0) | (1 << 32),
        ] {
            assert!(matches!(
                IchCapabilityProfile::from_raw_vtr(raw),
                Err(ArmVcpuError::InvalidIchCapability { raw_vtr, .. }) if raw_vtr == raw
            ));
        }
    }

    #[test]
    fn rejects_preemption_shape_wider_than_priority_or_below_minimum() {
        for raw in [raw_vtr(3, 4, 5, 0), raw_vtr(3, 4, 3, 0)] {
            assert!(matches!(
                IchCapabilityProfile::from_raw_vtr(raw),
                Err(ArmVcpuError::InvalidIchCapability {
                    reason: IchCapabilityError::InvalidPriorityShape { .. },
                    ..
                })
            ));
        }
    }

    fn valid_profile(
        list_registers: u8,
        priority_bits: u8,
        preemption_bits: u8,
        id_bits: u8,
    ) -> IchCapabilityProfile {
        IchCapabilityProfile::from_raw_vtr(raw_vtr(
            list_registers,
            priority_bits,
            preemption_bits,
            id_bits,
        ))
        .unwrap()
    }

    const fn raw_vtr(
        list_registers: u8,
        priority_bits: u8,
        preemption_bits: u8,
        id_bits: u8,
    ) -> u64 {
        list_registers as u64
            | (id_bits as u64) << ID_BITS_SHIFT
            | (preemption_bits as u64) << PREEMPTION_BITS_SHIFT
            | (priority_bits as u64) << PRIORITY_BITS_SHIFT
    }
}
