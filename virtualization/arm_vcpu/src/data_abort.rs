//! Typed AArch64 data-abort information passed to the embedding VMM.

use crate::{ArmAccessWidth, ArmGuestPhysAddr, ArmGuestVirtAddr, ArmVcpuError, ArmVcpuResult};

const ESR_IL_BIT: u32 = 1 << 25;
const ISS_ISV_BIT: u32 = 1 << 24;
const ISS_SAS_SHIFT: u32 = 22;
const ISS_SSE_BIT: u32 = 1 << 21;
const ISS_SRT_SHIFT: u32 = 16;
const ISS_SF_BIT: u32 = 1 << 15;
const ISS_FNV_BIT: u32 = 1 << 10;
const ISS_CM_BIT: u32 = 1 << 8;
const ISS_S1PTW_BIT: u32 = 1 << 7;
const ISS_WNR_BIT: u32 = 1 << 6;
const ISS_FSC_MASK: u32 = 0x3f;

/// A validated AArch64 general-purpose register identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmGpr(u8);

impl ArmGpr {
    /// Returns the architectural register index (`0..=31`).
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<usize> for ArmGpr {
    type Error = ArmVcpuError;

    fn try_from(index: usize) -> Result<Self, Self::Error> {
        u8::try_from(index)
            .ok()
            .filter(|index| *index <= 31)
            .map(Self)
            .ok_or(ArmVcpuError::InvalidInput)
    }
}

/// Extension applied to a value loaded by a trapped guest instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmLoadExtension {
    /// Fill the upper bits with zeroes.
    Zero,
    /// Replicate the most-significant bit of the accessed value.
    Sign,
}

/// A single-register memory access decoded from a valid data-abort syndrome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmDataAccess {
    /// A guest load whose result must be written to `register`.
    Read {
        /// Memory transaction width.
        width: ArmAccessWidth,
        /// Destination register.
        register: ArmGpr,
        /// Architectural width of the destination register.
        register_width: ArmAccessWidth,
        /// Extension applied before writing the destination register.
        extension: ArmLoadExtension,
    },
    /// A guest store whose value has already been narrowed to `width`.
    Write {
        /// Memory transaction width.
        width: ArmAccessWidth,
        /// Source register.
        register: ArmGpr,
        /// Value presented to the memory transaction.
        value: u64,
    },
}

impl ArmDataAccess {
    /// Returns the memory transaction width.
    pub const fn width(self) -> ArmAccessWidth {
        match self {
            Self::Read { width, .. } | Self::Write { width, .. } => width,
        }
    }
}

/// Stage at which an AArch64 data abort was reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmDataFault {
    /// Address-size fault at the reported translation-table level.
    AddressSize { level: u8 },
    /// Translation fault at the reported translation-table level.
    Translation { level: u8 },
    /// Access-flag fault at the reported translation-table level.
    AccessFlag { level: u8 },
    /// Permission fault at the reported translation-table level.
    Permission { level: u8 },
    /// Synchronous external abort not associated with a table walk.
    SynchronousExternal,
    /// Synchronous external abort during a translation-table walk.
    SynchronousExternalOnWalk { level: u8 },
    /// Synchronous parity or ECC error not associated with a table walk.
    SynchronousParityOrEcc,
    /// Synchronous parity or ECC error during a translation-table walk.
    SynchronousParityOrEccOnWalk { level: u8 },
    /// An architecturally valid fault status not modeled by this crate.
    Other { status: u8 },
}

/// Raw architectural syndrome plus checked field accessors for a data abort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmDataAbortSyndrome(u32);

impl ArmDataAbortSyndrome {
    /// Creates a syndrome from the complete `ESR_EL2` value captured on exit.
    pub(crate) const fn from_esr(esr: u32) -> Self {
        Self(esr)
    }

    /// Returns the complete captured `ESR_EL2` value.
    pub const fn raw_esr(self) -> u32 {
        self.0
    }

    /// Returns whether SAS, SRT, SF and SSE contain valid instruction syndrome.
    pub const fn has_valid_instruction_syndrome(self) -> bool {
        self.iss() & ISS_ISV_BIT != 0
    }

    /// Returns whether the abort arose during a stage-1 translation-table walk.
    pub const fn is_stage1_page_table_walk(self) -> bool {
        self.iss() & ISS_S1PTW_BIT != 0
    }

    /// Returns whether the trapped operation was cache maintenance.
    pub const fn is_cache_maintenance(self) -> bool {
        self.iss() & ISS_CM_BIT != 0
    }

    /// Returns whether the faulting memory operation required write access.
    pub const fn is_write(self) -> bool {
        self.iss() & ISS_WNR_BIT != 0
    }

    /// Returns whether the captured `FAR_EL2` value is architecturally valid.
    pub const fn has_valid_fault_address(self) -> bool {
        self.iss() & ISS_FNV_BIT == 0
    }

    /// Classifies the architectural Fault Status Code.
    pub const fn fault(self) -> ArmDataFault {
        let status = self.fault_status();
        match status {
            0..=3 => ArmDataFault::AddressSize { level: status },
            4..=7 => ArmDataFault::Translation { level: status - 4 },
            8..=11 => ArmDataFault::AccessFlag { level: status - 8 },
            12..=15 => ArmDataFault::Permission { level: status - 12 },
            0x10 => ArmDataFault::SynchronousExternal,
            0x14..=0x17 => ArmDataFault::SynchronousExternalOnWalk {
                level: status - 0x14,
            },
            0x18 => ArmDataFault::SynchronousParityOrEcc,
            0x1c..=0x1f => ArmDataFault::SynchronousParityOrEccOnWalk {
                level: status - 0x1c,
            },
            status => ArmDataFault::Other { status },
        }
    }

    pub(crate) const fn hpfar_is_valid(self) -> bool {
        matches!(
            self.fault(),
            ArmDataFault::AddressSize { .. }
                | ArmDataFault::Translation { .. }
                | ArmDataFault::AccessFlag { .. }
        ) || self.is_stage1_page_table_walk()
            && matches!(self.fault(), ArmDataFault::Permission { .. })
    }

    pub(crate) const fn fault_safe_to_translate(self) -> bool {
        if matches!(
            self.fault(),
            ArmDataFault::SynchronousExternalOnWalk { .. }
                | ArmDataFault::SynchronousParityOrEccOnWalk { .. }
        ) {
            return false;
        }
        !matches!(self.fault(), ArmDataFault::SynchronousExternal) || self.has_valid_fault_address()
    }

    pub(crate) const fn has_valid_ipa_offset(self) -> bool {
        self.has_valid_fault_address() && !self.is_stage1_page_table_walk()
    }

    pub(crate) const fn instruction_size(self) -> usize {
        if self.0 & ESR_IL_BIT == 0 { 2 } else { 4 }
    }

    pub(crate) const fn access_width(self) -> Option<ArmAccessWidth> {
        if !self.has_valid_instruction_syndrome() {
            return None;
        }
        Some(match (self.iss() >> ISS_SAS_SHIFT) & 0b11 {
            0 => ArmAccessWidth::Byte,
            1 => ArmAccessWidth::Word,
            2 => ArmAccessWidth::Dword,
            _ => ArmAccessWidth::Qword,
        })
    }

    pub(crate) const fn access_register(self) -> usize {
        ((self.iss() >> ISS_SRT_SHIFT) & 0b1_1111) as usize
    }

    pub(crate) const fn access_register_width(self) -> ArmAccessWidth {
        if self.iss() & ISS_SF_BIT == 0 {
            ArmAccessWidth::Dword
        } else {
            ArmAccessWidth::Qword
        }
    }

    pub(crate) const fn load_extension(self) -> ArmLoadExtension {
        if self.iss() & ISS_SSE_BIT == 0 {
            ArmLoadExtension::Zero
        } else {
            ArmLoadExtension::Sign
        }
    }

    const fn iss(self) -> u32 {
        self.0 & 0x01ff_ffff
    }

    const fn fault_status(self) -> u8 {
        (self.iss() & ISS_FSC_MASK) as u8
    }
}

/// A fault IPA whose 4 KiB page is known even when the byte offset is not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmFaultIpa {
    page_base: ArmGuestPhysAddr,
    page_offset: Option<u16>,
}

impl ArmFaultIpa {
    pub(crate) const fn from_hpfar(hpfar: usize, far: u64, offset_valid: bool) -> Self {
        Self {
            page_base: ArmGuestPhysAddr::from_usize(hpfar << 8),
            page_offset: if offset_valid {
                Some((far as u16) & 0x0fff)
            } else {
                None
            },
        }
    }

    #[cfg(test)]
    pub(crate) const fn page(address: ArmGuestPhysAddr) -> Self {
        Self {
            page_base: ArmGuestPhysAddr::from_usize(address.as_usize() & !0x0fff),
            page_offset: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn exact(address: ArmGuestPhysAddr) -> Self {
        Self {
            page_base: ArmGuestPhysAddr::from_usize(address.as_usize() & !0x0fff),
            page_offset: Some((address.as_usize() as u16) & 0x0fff),
        }
    }

    /// Returns the 4 KiB IPA page reported for the fault.
    pub const fn page_base(self) -> ArmGuestPhysAddr {
        self.page_base
    }

    /// Returns the exact IPA only when the architectural page offset is valid.
    pub const fn exact_address(self) -> Option<ArmGuestPhysAddr> {
        match self.page_offset {
            Some(offset) => Some(ArmGuestPhysAddr::from_usize(
                self.page_base.as_usize() | offset as usize,
            )),
            None => None,
        }
    }
}

/// A data abort captured by the vCPU core before an embedding VMM assigns
/// address ownership or chooses an emulation policy.
#[derive(Debug, Eq, PartialEq)]
pub struct ArmDataAbort {
    fault_ipa: Option<ArmFaultIpa>,
    fault_virtual_address: Option<ArmGuestVirtAddr>,
    instruction_address: u64,
    syndrome: ArmDataAbortSyndrome,
    access: Option<ArmDataAccess>,
}

impl ArmDataAbort {
    pub(crate) const fn new(
        fault_ipa: Option<ArmFaultIpa>,
        fault_virtual_address: Option<ArmGuestVirtAddr>,
        instruction_address: u64,
        syndrome: ArmDataAbortSyndrome,
        access: Option<ArmDataAccess>,
    ) -> Self {
        Self {
            fault_ipa,
            fault_virtual_address,
            instruction_address,
            syndrome,
            access,
        }
    }

    /// Returns the guest physical fault page and, when valid, its byte offset.
    pub const fn fault_ipa(&self) -> Option<ArmFaultIpa> {
        self.fault_ipa
    }

    /// Returns the guest virtual fault address, when FAR is valid.
    pub const fn fault_virtual_address(&self) -> Option<ArmGuestVirtAddr> {
        self.fault_virtual_address
    }

    /// Returns the address of the instruction that raised the abort.
    pub const fn instruction_address(&self) -> u64 {
        self.instruction_address
    }

    /// Returns the captured architectural syndrome.
    pub const fn syndrome(&self) -> ArmDataAbortSyndrome {
        self.syndrome
    }

    /// Returns a decoded single-register access only when all required syndrome
    /// fields are architecturally valid.
    pub const fn access(&self) -> Option<ArmDataAccess> {
        self.access
    }

    pub(crate) const fn instruction_size(&self) -> usize {
        self.syndrome.instruction_size()
    }
}

/// Successful result supplied after the embedding VMM emulates an access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmDataAccessResult {
    /// Value returned by a read transaction.
    Read(u64),
    /// Acknowledgement of a completed write transaction.
    Write,
}

pub(crate) fn decode_data_access(
    syndrome: ArmDataAbortSyndrome,
    register_value: impl FnOnce(ArmGpr) -> u64,
) -> ArmVcpuResult<Option<ArmDataAccess>> {
    if !syndrome.has_valid_instruction_syndrome()
        || syndrome.is_stage1_page_table_walk()
        || syndrome.is_cache_maintenance()
    {
        return Ok(None);
    }

    let width = syndrome.access_width().ok_or(ArmVcpuError::InvalidInput)?;
    let register = ArmGpr::try_from(syndrome.access_register())?;
    if syndrome.is_write() {
        return Ok(Some(ArmDataAccess::Write {
            width,
            register,
            value: register_value(register) & access_mask(width),
        }));
    }
    Ok(Some(ArmDataAccess::Read {
        width,
        register,
        register_width: syndrome.access_register_width(),
        extension: syndrome.load_extension(),
    }))
}

pub(crate) const fn access_mask(width: ArmAccessWidth) -> u64 {
    match width {
        ArmAccessWidth::Byte => u8::MAX as u64,
        ArmAccessWidth::Word => u16::MAX as u64,
        ArmAccessWidth::Dword => u32::MAX as u64,
        ArmAccessWidth::Qword => u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_valid_single_register_load() {
        let esr = (0x24 << 26)
            | ESR_IL_BIT
            | ISS_ISV_BIT
            | (2 << ISS_SAS_SHIFT)
            | (3 << ISS_SRT_SHIFT)
            | ISS_SF_BIT
            | 0x7;
        let syndrome = ArmDataAbortSyndrome::from_esr(esr);

        assert_eq!(
            decode_data_access(syndrome, |_| 0).unwrap(),
            Some(ArmDataAccess::Read {
                width: ArmAccessWidth::Dword,
                register: ArmGpr(3),
                register_width: ArmAccessWidth::Qword,
                extension: ArmLoadExtension::Zero,
            })
        );
        assert_eq!(syndrome.fault(), ArmDataFault::Translation { level: 3 });
    }

    #[test]
    fn rejects_data_abort_access_when_isv_is_clear() {
        let syndrome = ArmDataAbortSyndrome::from_esr(0x9200_0007);

        assert_eq!(decode_data_access(syndrome, |_| 0).unwrap(), None);
        assert_eq!(syndrome.access_width(), None);
    }

    #[test]
    fn narrows_store_value_to_transaction_width() {
        let esr = (0x24 << 26)
            | ESR_IL_BIT
            | ISS_ISV_BIT
            | (1 << ISS_SAS_SHIFT)
            | (4 << ISS_SRT_SHIFT)
            | ISS_WNR_BIT
            | 0x7;
        let syndrome = ArmDataAbortSyndrome::from_esr(esr);

        assert_eq!(
            decode_data_access(syndrome, |_| 0x1234_5678).unwrap(),
            Some(ArmDataAccess::Write {
                width: ArmAccessWidth::Word,
                register: ArmGpr(4),
                value: 0x5678,
            })
        );
    }

    #[test]
    fn fault_ipa_does_not_invent_an_invalid_page_offset() {
        let page = ArmFaultIpa::from_hpfar(0x1030, 0xffff_0000_dead_0a0, false);
        let exact = ArmFaultIpa::from_hpfar(0x1030, 0xffff_0000_dead_0a0, true);

        assert_eq!(page.page_base(), ArmGuestPhysAddr::from_usize(0x103000));
        assert_eq!(page.exact_address(), None);
        assert_eq!(
            exact.exact_address(),
            Some(ArmGuestPhysAddr::from_usize(0x1030a0))
        );
    }

    #[test]
    fn syndrome_selects_only_architecturally_valid_ipa_sources() {
        let translation_with_invalid_far =
            ArmDataAbortSyndrome::from_esr((0x24 << 26) | ESR_IL_BIT | ISS_FNV_BIT | 0x7);
        assert!(translation_with_invalid_far.hpfar_is_valid());
        assert!(!translation_with_invalid_far.has_valid_ipa_offset());

        let permission = ArmDataAbortSyndrome::from_esr((0x24 << 26) | ESR_IL_BIT | 0xf);
        assert!(!permission.hpfar_is_valid());
        assert!(permission.fault_safe_to_translate());

        let permission_during_walk =
            ArmDataAbortSyndrome::from_esr((0x24 << 26) | ESR_IL_BIT | ISS_S1PTW_BIT | 0xf);
        assert!(permission_during_walk.hpfar_is_valid());
        assert!(!permission_during_walk.has_valid_ipa_offset());

        let external_with_invalid_far =
            ArmDataAbortSyndrome::from_esr((0x24 << 26) | ESR_IL_BIT | ISS_FNV_BIT | 0x10);
        assert!(!external_with_invalid_far.hpfar_is_valid());
        assert!(!external_with_invalid_far.fault_safe_to_translate());

        let external_during_walk = ArmDataAbortSyndrome::from_esr((0x24 << 26) | ESR_IL_BIT | 0x16);
        assert!(!external_during_walk.fault_safe_to_translate());
    }
}
