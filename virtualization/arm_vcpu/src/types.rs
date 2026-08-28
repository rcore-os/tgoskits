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

use core::fmt::{Debug, Formatter, LowerHex, UpperHex};

/// Result type returned by the OS-neutral AArch64 vCPU core.
pub type ArmVcpuResult<T = ()> = Result<T, ArmVcpuError>;

/// Errors produced by the OS-neutral AArch64 vCPU core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmVcpuError {
    /// A caller supplied an invalid argument or unsupported hardware encoding.
    InvalidInput,
    /// The requested operation is not supported by this CPU or this vCPU core.
    Unsupported,
    /// Hardware or software state is inconsistent with the requested transition.
    BadState,
}

/// Guest physical address.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct ArmGuestPhysAddr(usize);

impl ArmGuestPhysAddr {
    /// Creates a guest physical address from a raw `usize`.
    pub const fn from_usize(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw address value.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

impl From<usize> for ArmGuestPhysAddr {
    fn from(value: usize) -> Self {
        Self::from_usize(value)
    }
}

impl From<ArmGuestPhysAddr> for usize {
    fn from(value: ArmGuestPhysAddr) -> Self {
        value.as_usize()
    }
}

impl Debug for ArmGuestPhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "GPA({:#x})", self.0)
    }
}

impl LowerHex for ArmGuestPhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for ArmGuestPhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#X}", self.0)
    }
}

/// AArch64 system-register address encoding used by trapped MRS/MSR exits.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct ArmSysRegAddr(usize);

impl ArmSysRegAddr {
    /// Creates a system-register address from the ISS-derived encoding.
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw register address encoding.
    pub const fn addr(self) -> usize {
        self.0
    }
}

impl From<usize> for ArmSysRegAddr {
    fn from(value: usize) -> Self {
        Self::new(value)
    }
}

impl From<ArmSysRegAddr> for usize {
    fn from(value: ArmSysRegAddr) -> Self {
        value.addr()
    }
}

impl Debug for ArmSysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "ArmSysRegAddr({:#x})", self.0)
    }
}

impl LowerHex for ArmSysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for ArmSysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#X}", self.0)
    }
}

/// Width of a trapped guest memory access.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ArmAccessWidth {
    /// 8-bit access.
    Byte,
    /// 16-bit access.
    Word,
    /// 32-bit access.
    Dword,
    /// 64-bit access.
    Qword,
}

impl ArmAccessWidth {
    /// Returns this access width in bytes.
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
        }
    }
}

impl TryFrom<usize> for ArmAccessWidth {
    type Error = ArmVcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(ArmVcpuError::InvalidInput),
        }
    }
}

impl From<ArmAccessWidth> for usize {
    fn from(value: ArmAccessWidth) -> Self {
        value.size()
    }
}

/// Access type reported by a current-EL host page fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmHostPageFaultAccess {
    /// The faulting operation read memory.
    Read,
    /// The faulting operation wrote memory.
    Write,
    /// The faulting operation fetched an instruction.
    Execute,
}

/// Host operations required by AArch64 virtualization code.
///
/// The vCPU core calls these static methods at architecture boundaries where
/// the embedding OS or VMM owns the policy: virtual interrupt injection,
/// physical interrupt reporting, and current-EL exception dispatch.
pub trait ArmHostOps {
    /// Injects a virtual interrupt through host interrupt-controller state.
    fn inject_virtual_interrupt(vector: u32) -> ArmVcpuResult;

    /// Completes the priority drop for an IAR value captured by the
    /// assembly-only lower-EL IRQ exit path.
    ///
    /// The implementation must return the stable token used for later
    /// deactivate, or `None` for a special or spurious acknowledgement.
    fn finish_pending_host_irq(raw_ack: u32) -> Option<usize>;

    /// Dispatches a host IRQ taken while running at the current exception level.
    fn handle_current_host_irq();

    /// Handles a host page fault taken while running at the current exception level.
    ///
    /// The implementation may replace `saved_pc` when an exception-table
    /// entry recovers the fault. Returning `true` resumes from the resulting
    /// saved PC; returning `false` retains the vCPU core's detailed panic.
    fn handle_current_host_page_fault(
        _saved_pc: &mut usize,
        _fault_addr: usize,
        _access: ArmHostPageFaultAccess,
        _parent_irqs_enabled: bool,
    ) -> bool {
        false
    }
}

pub(crate) const fn decode_current_el_host_page_fault(
    exception_class: u64,
    instruction_specific_syndrome: u64,
) -> Option<ArmHostPageFaultAccess> {
    const INSTRUCTION_ABORT_CURRENT_EL: u64 = 0b100001;
    const DATA_ABORT_CURRENT_EL: u64 = 0b100101;
    const FAULT_STATUS_MASK: u64 = 0b11_1111;
    const FAULT_STATUS_KIND_MASK: u64 = 0b11_1100;
    const TRANSLATION_FAULT: u64 = 0b00_0100;
    const PERMISSION_FAULT: u64 = 0b00_1100;
    const WRITE_NOT_READ: u64 = 1 << 6;
    const CACHE_MAINTENANCE: u64 = 1 << 8;

    let fault_status = instruction_specific_syndrome & FAULT_STATUS_MASK;
    if !matches!(
        fault_status & FAULT_STATUS_KIND_MASK,
        TRANSLATION_FAULT | PERMISSION_FAULT
    ) {
        return None;
    }

    match exception_class {
        INSTRUCTION_ABORT_CURRENT_EL => Some(ArmHostPageFaultAccess::Execute),
        DATA_ABORT_CURRENT_EL => {
            let is_write = instruction_specific_syndrome & WRITE_NOT_READ != 0;
            let is_cache_maintenance = instruction_specific_syndrome & CACHE_MAINTENANCE != 0;
            if is_write && !is_cache_maintenance {
                Some(ArmHostPageFaultAccess::Write)
            } else {
                Some(ArmHostPageFaultAccess::Read)
            }
        }
        _ => None,
    }
}

/// Stage-2 page table configuration selected by the embedding VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmNestedPagingConfig {
    /// Root physical address of the stage-2 page table.
    pub root_paddr: usize,
    /// Number of stage-2 page-table levels.
    pub levels: usize,
    /// Guest physical address width in bits.
    pub gpa_bits: usize,
    /// Hardware-specific mode value. For AArch64 this carries host PA bits when non-zero.
    pub mode: usize,
}

impl ArmNestedPagingConfig {
    /// Creates a nested paging configuration.
    pub const fn new(root_paddr: usize, levels: usize, gpa_bits: usize, mode: usize) -> Self {
        Self {
            root_paddr,
            levels,
            gpa_bits,
            mode,
        }
    }
}

/// Common GICv3 CPU-interface register trapped by the vCPU core.
///
/// Keeping the architectural register identity typed prevents raw system
/// register encodings from escaping into the embedding VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmGicCpuInterfaceRegister {
    /// `ICC_CTLR_EL1`, the common CPU-interface control register.
    Control,
    /// `ICC_PMR_EL1`, the virtual priority-mask register.
    PriorityMask,
    /// `ICC_RPR_EL1`, the virtual running-priority register.
    RunningPriority,
}

/// VM-exit reason returned by the AArch64 vCPU core.
#[non_exhaustive]
#[derive(Debug)]
pub enum ArmVmExit {
    /// A guest instruction triggered a hypercall.
    Hypercall {
        /// Hypercall number.
        nr: u64,
        /// Hypercall arguments.
        args: [u64; 6],
    },
    /// The guest performed an MMIO read.
    MmioRead {
        /// Guest physical address being read.
        addr: ArmGuestPhysAddr,
        /// Access width.
        width: ArmAccessWidth,
        /// Destination guest register.
        reg: usize,
        /// Destination register width.
        reg_width: ArmAccessWidth,
        /// Whether the value should be sign-extended.
        signed_ext: bool,
    },
    /// The guest performed an MMIO write.
    MmioWrite {
        /// Guest physical address being written.
        addr: ArmGuestPhysAddr,
        /// Access width.
        width: ArmAccessWidth,
        /// Value written by the guest.
        data: u64,
    },
    /// The guest performed a system-register read.
    SysRegRead {
        /// System-register address.
        addr: ArmSysRegAddr,
        /// Destination guest register.
        reg: usize,
    },
    /// The guest performed a system-register write.
    SysRegWrite {
        /// System-register address.
        addr: ArmSysRegAddr,
        /// Value written by the guest.
        value: u64,
    },
    /// The guest read a trapped GICv3 common CPU-interface register.
    GicCpuInterfaceRead {
        /// Register selected by the trapped MRS instruction.
        register: ArmGicCpuInterfaceRegister,
        /// Destination guest general-purpose register.
        destination: usize,
    },
    /// The guest wrote a trapped GICv3 common CPU-interface register.
    GicCpuInterfaceWrite {
        /// Register selected by the trapped MSR instruction.
        register: ArmGicCpuInterfaceRegister,
        /// Value written by the guest.
        value: u64,
    },
    /// A physical host interrupt should be handled by the embedding VMM.
    ExternalInterrupt {
        /// Opaque acknowledgement token, or `None` for a spurious interrupt.
        ///
        /// The token is returned unchanged so a split-EOI host controller can
        /// retain source information until the guest deactivates the interrupt.
        token: Option<usize>,
    },
    /// A guest WFI or WFE instruction was trapped.
    WaitForInterrupt,
    /// A guest PSCI CPU_OFF call was trapped.
    CpuDown {
        /// Guest-provided target state.
        state: u64,
    },
    /// A guest PSCI CPU_ON call was trapped.
    CpuUp {
        /// Target CPU affinity.
        target_cpu: u64,
        /// Guest entry point for the target CPU.
        entry_point: ArmGuestPhysAddr,
        /// Guest argument for the target CPU.
        arg: u64,
    },
    /// The guest requested system power-off.
    SystemDown,
    /// The guest wrote a GIC SGI system register.
    SendIPI {
        /// Complete `ICC_SGI1R_EL1` value, including affinity and range selector.
        value: u64,
    },
    /// The guest wrote `ICC_DIR_EL1` while deactivation trapping was enabled.
    DeactivateInterrupt {
        /// Guest-visible INTID carried by `ICC_DIR_EL1`.
        intid: u32,
    },
    /// The vCPU handled the event internally.
    Nothing,
}

#[cfg(test)]
mod tests {
    use core::marker::PhantomData;

    use super::*;

    struct BorrowedHost<'a>(PhantomData<&'a mut ()>);

    impl ArmHostOps for BorrowedHost<'_> {
        fn inject_virtual_interrupt(_vector: u32) -> ArmVcpuResult {
            Ok(())
        }

        fn finish_pending_host_irq(_raw_ack: u32) -> Option<usize> {
            None
        }

        fn handle_current_host_irq() {}
    }

    const INSTRUCTION_ABORT_CURRENT_EL: u64 = 0b100001;
    const DATA_ABORT_CURRENT_EL: u64 = 0b100101;
    const INSTRUCTION_ABORT_LOWER_EL: u64 = 0b100000;
    const DATA_ABORT_LOWER_EL: u64 = 0b100100;
    const TRANSLATION_FAULT_LEVEL_0: u64 = 0b000100;
    const TRANSLATION_FAULT_LEVEL_3: u64 = 0b000111;
    const PERMISSION_FAULT_LEVEL_0: u64 = 0b001100;
    const PERMISSION_FAULT_LEVEL_3: u64 = 0b001111;
    const ACCESS_FLAG_FAULT: u64 = 0b001000;
    const WRITE_NOT_READ: u64 = 1 << 6;
    const CACHE_MAINTENANCE: u64 = 1 << 8;

    #[test]
    fn arm_host_ops_accepts_a_non_static_implementor() {
        fn require_borrowed_host<'a>(_borrow: &'a mut ()) {
            fn require_host<H: ArmHostOps>() {}
            require_host::<BorrowedHost<'a>>();
            BorrowedHost::inject_virtual_interrupt(0).unwrap();
            assert_eq!(BorrowedHost::finish_pending_host_irq(0), None);
            BorrowedHost::handle_current_host_irq();
            let mut saved_pc = 0;
            assert!(!BorrowedHost::handle_current_host_page_fault(
                &mut saved_pc,
                0,
                ArmHostPageFaultAccess::Read,
                false,
            ));
        }

        let mut borrowed = ();
        require_borrowed_host(&mut borrowed);
    }

    #[test]
    fn host_page_fault_decoder_accepts_translation_and_permission_faults() {
        for fault_status in [
            TRANSLATION_FAULT_LEVEL_0,
            TRANSLATION_FAULT_LEVEL_3,
            PERMISSION_FAULT_LEVEL_0,
            PERMISSION_FAULT_LEVEL_3,
        ] {
            assert_eq!(
                decode_current_el_host_page_fault(INSTRUCTION_ABORT_CURRENT_EL, fault_status),
                Some(ArmHostPageFaultAccess::Execute)
            );
            assert_eq!(
                decode_current_el_host_page_fault(DATA_ABORT_CURRENT_EL, fault_status),
                Some(ArmHostPageFaultAccess::Read)
            );
        }
    }

    #[test]
    fn host_page_fault_decoder_distinguishes_read_write_and_execute() {
        assert_eq!(
            decode_current_el_host_page_fault(
                DATA_ABORT_CURRENT_EL,
                TRANSLATION_FAULT_LEVEL_0 | WRITE_NOT_READ
            ),
            Some(ArmHostPageFaultAccess::Write)
        );
        assert_eq!(
            decode_current_el_host_page_fault(
                DATA_ABORT_CURRENT_EL,
                TRANSLATION_FAULT_LEVEL_0 | WRITE_NOT_READ | CACHE_MAINTENANCE
            ),
            Some(ArmHostPageFaultAccess::Read)
        );
        assert_eq!(
            decode_current_el_host_page_fault(
                INSTRUCTION_ABORT_CURRENT_EL,
                PERMISSION_FAULT_LEVEL_3
            ),
            Some(ArmHostPageFaultAccess::Execute)
        );
    }

    #[test]
    fn host_page_fault_decoder_rejects_other_exception_classes_and_fault_statuses() {
        for exception_class in [INSTRUCTION_ABORT_LOWER_EL, DATA_ABORT_LOWER_EL, 0] {
            assert_eq!(
                decode_current_el_host_page_fault(exception_class, TRANSLATION_FAULT_LEVEL_0),
                None
            );
        }
        for fault_status in [ACCESS_FLAG_FAULT, 0, 0b010000] {
            assert_eq!(
                decode_current_el_host_page_fault(DATA_ABORT_CURRENT_EL, fault_status),
                None
            );
        }
    }
}
