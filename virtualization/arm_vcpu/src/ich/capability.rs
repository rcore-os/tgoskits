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

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{ArmVcpuError, ArmVcpuResult, ArmVirtualIntId};

const LIST_REGISTERS_MASK: u64 = 0x1f;
const RESERVED_LOW_MASK: u64 = 0x1fff << 5;
const TDS_BIT: u64 = 1 << 19;
const ID_BITS_SHIFT: u32 = 23;
const THREE_BIT_MASK: u64 = 0b111;
const PREEMPTION_BITS_SHIFT: u32 = 26;
const PRIORITY_BITS_SHIFT: u32 = 29;
const RESERVED_HIGH_MASK: u64 = (u32::MAX as u64) << 32;
const CPU_CAPABILITY_CAPACITY: usize = usize::BITS as usize;

static ICH_CAPABILITIES: IchCapabilityRegistry<CPU_CAPABILITY_CAPACITY> =
    IchCapabilityRegistry::new();

/// Returns the immutable ICH capability profile published by one logical CPU.
///
/// # Errors
///
/// Returns [`ArmVcpuError::IchCapabilityCpuOutOfRange`] when `cpu_id` is not
/// representable by the runtime CPU mask, or
/// [`ArmVcpuError::IchCapabilityNotPublished`] before that CPU successfully
/// enables virtualization.
pub fn ich_capability(cpu_id: usize) -> ArmVcpuResult<IchCapabilityProfile> {
    ICH_CAPABILITIES.get(cpu_id)
}

/// Computes the lossless common ICH capability for a set of logical CPUs.
///
/// LR count and INTID width are reduced to the common minimum. Priority,
/// preemption, and AP-register shapes must match exactly. TDIR is reported only
/// when every CPU supports it.
///
/// # Errors
///
/// Returns an error for an empty set, an unpublished CPU, or incompatible
/// priority/AP-register shapes.
pub fn common_ich_capability(cpu_ids: &[usize]) -> ArmVcpuResult<IchCapabilityProfile> {
    ICH_CAPABILITIES.common(cpu_ids)
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn publish_ich_capability(
    cpu_id: usize,
    profile: IchCapabilityProfile,
) -> ArmVcpuResult {
    ICH_CAPABILITIES.publish(cpu_id, profile)
}

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

    const fn packed(self) -> u64 {
        self.list_register_count as u64
            | (self.virtual_intid_bits as u64) << 5
            | (self.priority_bits as u64) << 10
            | (self.preemption_bits as u64) << 13
            | (self.active_priority_register_count as u64) << 16
            | (self.supports_tdir as u64) << 19
    }

    const fn from_packed(packed: u64) -> Self {
        Self {
            list_register_count: (packed & 0x1f) as u8,
            virtual_intid_bits: ((packed >> 5) & 0x1f) as u8,
            priority_bits: ((packed >> 10) & 0x7) as u8,
            preemption_bits: ((packed >> 13) & 0x7) as u8,
            active_priority_register_count: ((packed >> 16) & 0x7) as u8,
            supports_tdir: packed & (1 << 19) != 0,
        }
    }

    const fn has_same_register_shape(self, other: Self) -> bool {
        self.priority_bits == other.priority_bits
            && self.preemption_bits == other.preemption_bits
            && self.active_priority_register_count == other.active_priority_register_count
    }

    const fn common_with(self, other: Self) -> Self {
        Self {
            list_register_count: if self.list_register_count < other.list_register_count {
                self.list_register_count
            } else {
                other.list_register_count
            },
            virtual_intid_bits: if self.virtual_intid_bits < other.virtual_intid_bits {
                self.virtual_intid_bits
            } else {
                other.virtual_intid_bits
            },
            priority_bits: self.priority_bits,
            preemption_bits: self.preemption_bits,
            active_priority_register_count: self.active_priority_register_count,
            supports_tdir: self.supports_tdir && other.supports_tdir,
        }
    }
}

struct IchCapabilityRegistry<const CAPACITY: usize> {
    profiles: [AtomicU64; CAPACITY],
}

impl<const CAPACITY: usize> IchCapabilityRegistry<CAPACITY> {
    const fn new() -> Self {
        Self {
            profiles: [const { AtomicU64::new(0) }; CAPACITY],
        }
    }

    fn publish(&self, cpu_id: usize, profile: IchCapabilityProfile) -> ArmVcpuResult {
        let slot = self.slot(cpu_id)?;
        let attempted = profile.packed();
        match slot.compare_exchange(0, attempted, Ordering::Release, Ordering::Acquire) {
            Ok(_) => Ok(()),
            Err(published) if published == attempted => Ok(()),
            Err(published) => Err(ArmVcpuError::IchCapabilityConflict {
                cpu_id,
                published: IchCapabilityProfile::from_packed(published),
                attempted: profile,
            }),
        }
    }

    fn get(&self, cpu_id: usize) -> ArmVcpuResult<IchCapabilityProfile> {
        let packed = self.slot(cpu_id)?.load(Ordering::Acquire);
        if packed == 0 {
            Err(ArmVcpuError::IchCapabilityNotPublished { cpu_id })
        } else {
            Ok(IchCapabilityProfile::from_packed(packed))
        }
    }

    fn common(&self, cpu_ids: &[usize]) -> ArmVcpuResult<IchCapabilityProfile> {
        let (&first_cpu_id, remaining) = cpu_ids.split_first().ok_or(ArmVcpuError::InvalidInput)?;
        let first = self.get(first_cpu_id)?;
        let mut common = first;

        for &cpu_id in remaining {
            let other = self.get(cpu_id)?;
            if !first.has_same_register_shape(other) {
                return Err(ArmVcpuError::IncompatibleIchCapabilities {
                    first_cpu_id,
                    first,
                    cpu_id,
                    other,
                });
            }
            common = common.common_with(other);
        }
        Ok(common)
    }

    fn slot(&self, cpu_id: usize) -> ArmVcpuResult<&AtomicU64> {
        self.profiles
            .get(cpu_id)
            .ok_or(ArmVcpuError::IchCapabilityCpuOutOfRange {
                cpu_id,
                capacity: CAPACITY,
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

    #[test]
    fn registry_publishes_once_and_accepts_idempotent_publication() {
        let registry = IchCapabilityRegistry::<4>::new();
        let profile = valid_profile(3, 4, 4, 0);

        registry.publish(2, profile).unwrap();
        registry.publish(2, profile).unwrap();

        assert_eq!(registry.get(2).unwrap(), profile);
    }

    #[test]
    fn registry_rejects_conflicting_publication() {
        let registry = IchCapabilityRegistry::<4>::new();
        let published = valid_profile(3, 4, 4, 0);
        let attempted = valid_profile(7, 4, 4, 0);
        registry.publish(1, published).unwrap();

        assert_eq!(
            registry.publish(1, attempted),
            Err(ArmVcpuError::IchCapabilityConflict {
                cpu_id: 1,
                published,
                attempted,
            })
        );
    }

    #[test]
    fn registry_reports_unpublished_and_out_of_range_cpus() {
        let registry = IchCapabilityRegistry::<2>::new();
        assert_eq!(
            registry.get(0),
            Err(ArmVcpuError::IchCapabilityNotPublished { cpu_id: 0 })
        );
        assert_eq!(
            registry.get(2),
            Err(ArmVcpuError::IchCapabilityCpuOutOfRange {
                cpu_id: 2,
                capacity: 2,
            })
        );
    }

    #[test]
    fn common_profile_reduces_lr_intid_and_tdir_capabilities() {
        let registry = IchCapabilityRegistry::<3>::new();
        let first = IchCapabilityProfile::from_raw_vtr(raw_vtr(15, 5, 4, 1) | TDS_BIT).unwrap();
        let second = valid_profile(7, 5, 4, 0);
        registry.publish(0, first).unwrap();
        registry.publish(2, second).unwrap();

        let common = registry.common(&[0, 2]).unwrap();
        assert_eq!(common.list_register_count(), 8);
        assert_eq!(common.virtual_intid_bits(), 16);
        assert_eq!(common.priority_bits(), 6);
        assert_eq!(common.active_priority_register_count(), 2);
        assert!(!common.supports_tdir());
    }

    #[test]
    fn common_profile_rejects_lossy_priority_shape() {
        let registry = IchCapabilityRegistry::<2>::new();
        let first = valid_profile(3, 4, 4, 0);
        let other = valid_profile(3, 5, 4, 0);
        registry.publish(0, first).unwrap();
        registry.publish(1, other).unwrap();

        assert!(matches!(
            registry.common(&[0, 1]),
            Err(ArmVcpuError::IncompatibleIchCapabilities {
                first_cpu_id: 0,
                cpu_id: 1,
                ..
            })
        ));
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
