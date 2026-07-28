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

use core::fmt::{Display, Formatter};

use crate::{ArmVcpuError, ArmVcpuResult};

/// A traditional virtual interrupt identifier encodable by the supported GIC interface.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ArmVirtualIntId(u32);

impl ArmVirtualIntId {
    /// Largest non-special traditional interrupt identifier.
    pub const MAX: u32 = 1019;

    /// Creates a checked virtual interrupt identifier.
    pub const fn new(value: u32) -> ArmVcpuResult<Self> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(ArmVcpuError::InvalidVirtualInterruptId {
                value: value as usize,
            })
        }
    }

    /// Returns the architectural interrupt identifier.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl TryFrom<usize> for ArmVirtualIntId {
    type Error = ArmVcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let value_u32 =
            u32::try_from(value).map_err(|_| ArmVcpuError::InvalidVirtualInterruptId { value })?;
        Self::new(value_u32).map_err(|_| ArmVcpuError::InvalidVirtualInterruptId { value })
    }
}

impl From<ArmVirtualIntId> for u32 {
    fn from(intid: ArmVirtualIntId) -> Self {
        intid.as_u32()
    }
}

impl Display for ArmVirtualIntId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

/// State carried by a valid ICH list register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IchLrState {
    /// The interrupt is pending delivery.
    Pending,
    /// The interrupt has been acknowledged by the guest.
    Active,
    /// The interrupt is active and another instance is pending.
    ActivePending,
}

/// Software-owned ICH list register contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IchLrEntry {
    /// The slot is empty. Other raw fields have no identity semantics.
    Invalid,
    /// A software-backed virtual interrupt.
    Software {
        /// Virtual interrupt identifier.
        intid: ArmVirtualIntId,
        /// Pending/active state.
        state: IchLrState,
        /// Virtual interrupt priority.
        priority: u8,
        /// Whether the interrupt belongs to virtual Group 1.
        group1: bool,
        /// Whether guest EOI should cause maintenance notification.
        eoi: bool,
    },
}

/// Result of scanning ICH LRs for the compatibility direct-injection path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IchDirectInjection {
    /// The interrupt is already represented by a valid software LR.
    AlreadyPresent,
    /// The interrupt may be written to this empty LR slot.
    Vacant(usize),
}

const LR_VINTID_MASK: u64 = u32::MAX as u64;
const LR_EOI_BIT: u64 = 1 << 41;
const LR_PRIORITY_SHIFT: u32 = 48;
const LR_GROUP1_BIT: u64 = 1 << 60;
const LR_HW_BIT: u64 = 1 << 61;
const LR_STATE_SHIFT: u32 = 62;
const LR_STATE_MASK: u64 = 0b11 << LR_STATE_SHIFT;

impl IchLrEntry {
    /// Decodes raw ICH LR contents without performing any system-register access.
    pub fn decode(slot: usize, raw: u64) -> ArmVcpuResult<Self> {
        let raw_state = (raw & LR_STATE_MASK) >> LR_STATE_SHIFT;
        if raw_state == 0 {
            return Ok(Self::Invalid);
        }
        if raw & LR_HW_BIT != 0 {
            return Err(ArmVcpuError::UnsupportedListRegister { slot });
        }

        let raw_intid = (raw & LR_VINTID_MASK) as u32;
        let intid = ArmVirtualIntId::new(raw_intid)
            .map_err(|_| ArmVcpuError::MalformedListRegister { slot })?;
        let state = match raw_state {
            1 => IchLrState::Pending,
            2 => IchLrState::Active,
            3 => IchLrState::ActivePending,
            _ => unreachable!(),
        };

        Ok(Self::Software {
            intid,
            state,
            priority: (raw >> LR_PRIORITY_SHIFT) as u8,
            group1: raw & LR_GROUP1_BIT != 0,
            eoi: raw & LR_EOI_BIT != 0,
        })
    }

    /// Encodes this entry into raw ICH LR contents.
    pub const fn encode(self) -> u64 {
        match self {
            Self::Invalid => 0,
            Self::Software {
                intid,
                state,
                priority,
                group1,
                eoi,
            } => {
                let state = match state {
                    IchLrState::Pending => 1,
                    IchLrState::Active => 2,
                    IchLrState::ActivePending => 3,
                };
                intid.as_u32() as u64
                    | (priority as u64) << LR_PRIORITY_SHIFT
                    | (group1 as u64) << 60
                    | (eoi as u64) << 41
                    | state << LR_STATE_SHIFT
            }
        }
    }
}

/// Selects an LR for the compatibility direct-injection path.
///
/// This helper deliberately folds duplicate INTIDs. Stateful SPI delivery must
/// use its controller state machine instead because an edge arriving while an
/// interrupt is active has different semantics.
pub fn plan_direct_injection(
    intid: ArmVirtualIntId,
    empty_status: u16,
    raw_lrs: &[u64],
) -> ArmVcpuResult<IchDirectInjection> {
    if raw_lrs.is_empty() || raw_lrs.len() > 16 {
        return Err(ArmVcpuError::InvalidListRegisterCount {
            count: raw_lrs.len(),
        });
    }

    let mut free_lr = None;
    for (slot, raw) in raw_lrs.iter().copied().enumerate() {
        let state = (raw & LR_STATE_MASK) >> LR_STATE_SHIFT;
        if state == 0 {
            if empty_status & (1 << slot) != 0 {
                free_lr.get_or_insert(slot);
            }
            continue;
        }
        if raw & LR_HW_BIT != 0 {
            continue;
        }
        if matches!(
            IchLrEntry::decode(slot, raw)?,
            IchLrEntry::Software {
                intid: resident,
                state: IchLrState::Pending
                    | IchLrState::Active
                    | IchLrState::ActivePending,
                ..
            } if resident == intid
        ) {
            return Ok(IchDirectInjection::AlreadyPresent);
        }
    }

    free_lr
        .map(IchDirectInjection::Vacant)
        .ok_or(ArmVcpuError::NoFreeListRegister { intid })
}
