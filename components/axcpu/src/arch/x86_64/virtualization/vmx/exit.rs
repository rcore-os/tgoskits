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

use core::fmt::{Debug, Formatter};

/// VM instruction error numbers. (SDM Vol. 3C, Section 30.4)
pub struct VmxInstructionError(u32);

impl VmxInstructionError {
    /// Describes the architectural instruction-error number.
    pub fn as_str(&self) -> &str {
        match self.0 {
            0 => "OK",
            1 => "VMCALL executed in VMX root operation",
            2 => "VMCLEAR with invalid physical address",
            3 => "VMCLEAR with VMXON pointer",
            4 => "VMLAUNCH with non-clear VMCS",
            5 => "VMRESUME with non-launched VMCS",
            6 => "VMRESUME after VMXOFF (VMXOFF and VMXON between VMLAUNCH and VMRESUME)",
            7 => "VM entry with invalid control field(s)",
            8 => "VM entry with invalid host-state field(s)",
            9 => "VMPTRLD with invalid physical address",
            10 => "VMPTRLD with VMXON pointer",
            11 => "VMPTRLD with incorrect VMCS revision identifier",
            12 => "VMREAD/VMWRITE from/to unsupported VMCS component",
            13 => "VMWRITE to read-only VMCS component",
            15 => "VMXON executed in VMX root operation",
            16 => "VM entry with invalid executive-VMCS pointer",
            17 => "VM entry with non-launched executive VMCS",
            18 => {
                "VM entry with executive-VMCS pointer not VMXON pointer (when attempting to \
                 deactivate the dual-monitor treatment of SMIs and SMM)"
            }
            19 => {
                "VMCALL with non-clear VMCS (when attempting to activate the dual-monitor \
                 treatment of SMIs and SMM)"
            }
            20 => "VMCALL with invalid VM-exit control fields",
            22 => {
                "VMCALL with incorrect MSEG revision identifier (when attempting to activate the \
                 dual-monitor treatment of SMIs and SMM)"
            }
            23 => "VMXOFF under dual-monitor treatment of SMIs and SMM",
            24 => {
                "VMCALL with invalid SMM-monitor features (when attempting to activate the \
                 dual-monitor treatment of SMIs and SMM)"
            }
            25 => {
                "VM entry with invalid VM-execution control fields in executive VMCS (when \
                 attempting to return from SMM)"
            }
            26 => "VM entry with events blocked by MOV SS",
            28 => "Invalid operand to INVEPT/INVVPID",
            _ => "[INVALID]",
        }
    }
}

impl From<u32> for VmxInstructionError {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl Debug for VmxInstructionError {
    fn fmt(&self, f: &mut Formatter) -> core::fmt::Result {
        write!(f, "VmxInstructionError({}, {:?})", self.0, self.as_str())
    }
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
/// VMX basic exit reasons, as defined in the Intel Software Developer's Manual (SDM) Vol. 3D, Appendix C.
///
/// This enum represents the various reasons why a VM exit might occur during
/// the execution of a virtual machine in VMX (Virtual Machine Extensions) mode.
/// Each variant corresponds to a specific exit reason that can be identified
/// and handled by the hypervisor.
pub enum VmxExitReason {
    /// Exception or non-maskable interrupt (NMI) occurred.
    ExceptionNmi       = 0,
    /// An external interrupt was received.
    ExternalInterrupt  = 1,
    /// A triple fault occurred.
    TripleFault        = 2,
    /// INIT signal was received.
    Init               = 3,
    /// Startup IPI (SIPI) was received.
    Sipi               = 4,
    /// System Management Interrupt (SMI) was received.
    Smi                = 5,
    /// Other SMI was received.
    OtherSmi           = 6,
    /// An interrupt window was open.
    InterruptWindow    = 7,
    /// An NMI window was open.
    NmiWindow          = 8,
    /// A task switch occurred.
    TaskSwitch         = 9,
    /// CPUID instruction was executed.
    Cpuid              = 10,
    /// GETSEC instruction was executed.
    Getsec             = 11,
    /// HLT instruction was executed.
    Hlt                = 12,
    /// INVD instruction was executed.
    Invd               = 13,
    /// INVLPG instruction was executed.
    Invlpg             = 14,
    /// RDPMC instruction was executed.
    Rdpmc              = 15,
    /// RDTSC instruction was executed.
    Rdtsc              = 16,
    /// RSM instruction was executed in SMM.
    Rsm                = 17,
    /// VMCALL instruction was executed.
    Vmcall             = 18,
    /// VMCLEAR instruction was executed.
    Vmclear            = 19,
    /// VMLAUNCH instruction was executed.
    Vmlaunch           = 20,
    /// VMPTRLD instruction was executed.
    Vmptrld            = 21,
    /// VMPTRST instruction was executed.
    Vmptrst            = 22,
    /// VMREAD instruction was executed.
    Vmread             = 23,
    /// VMRESUME instruction was executed.
    Vmresume           = 24,
    /// VMWRITE instruction was executed.
    Vmwrite            = 25,
    /// VMOFF instruction was executed.
    Vmoff              = 26,
    /// VMON instruction was executed.
    Vmon               = 27,
    /// Control Register (CR) access.
    CrAccess           = 28,
    /// Debug Register (DR) access.
    DrAccess           = 29,
    /// I/O instruction was executed.
    IoInstruction      = 30,
    /// Model-Specific Register (MSR) read.
    MsrRead            = 31,
    /// Model-Specific Register (MSR) write.
    MsrWrite           = 32,
    /// Guest state is invalid.
    InvalidGuestState  = 33,
    /// MSR load failed.
    MsrLoadFail        = 34,
    /// MWAIT instruction was executed.
    MwaitInstruction   = 36,
    /// Monitor trap flag triggered.
    MonitorTrapFlag    = 37,
    /// MONITOR instruction was executed.
    MonitorInstruction = 39,
    /// PAUSE instruction was executed.
    PauseInstruction   = 40,
    /// Machine Check Exception (MCE) occurred during VM entry.
    MceDuringVmentry   = 41,
    /// Task Priority Register (TPR) below threshold.
    TprBelowThreshold  = 43,
    /// Access to Advanced Programmable Interrupt Controller (APIC).
    ApicAccess         = 44,
    /// Virtualized End Of Interrupt (EOI) was executed.
    VirtualizedEoi     = 45,
    /// Access to Global Descriptor Table Register (GDTR) or Interrupt Descriptor Table Register (IDTR).
    GdtrIdtr           = 46,
    /// Access to Local Descriptor Table Register (LDTR) or Task Register (TR).
    LdtrTr             = 47,
    /// Extended Page Table (EPT) violation occurred.
    EptViolation       = 48,
    /// Extended Page Table (EPT) misconfiguration occurred.
    EptMisconfig       = 49,
    /// INVEPT instruction was executed.
    Invept             = 50,
    /// RDTSCP instruction was executed.
    Rdtscp             = 51,
    /// Preemption timer expired.
    PreemptionTimer    = 52,
    /// INVVPID instruction was executed.
    Invvpid            = 53,
    /// WBINVD instruction was executed.
    Wbinvd             = 54,
    /// XSETBV instruction was executed.
    Xsetbv             = 55,
    /// APIC write occurred.
    ApicWrite          = 56,
    /// RDRAND instruction was executed.
    Rdrand             = 57,
    /// INVPCID instruction was executed.
    Invpcid            = 58,
    /// VMFUNC was executed.
    Vmfunc             = 59,
    /// ENCLS instruction was executed.
    Encls              = 60,
    /// RDSEED instruction was executed.
    Rdseed             = 61,
    /// Page modification log (PML) became full.
    PmlFull            = 62,
    /// XSAVES instruction was executed.
    Xsaves             = 63,
    /// XRSTORS instruction was executed.
    Xrstors            = 64,
    /// PCONFIG instruction was executed.
    Pconfig            = 65,
    /// SPP event occurred.
    SppEvent           = 66,
    /// UMWAIT instruction was executed.
    Umwait             = 67,
    /// TPAUSE instruction was executed.
    Tpause             = 68,
    /// LOADIWKEY instruction was executed.
    Loadiwkey          = 69,
}

impl TryFrom<u32> for VmxExitReason {
    type Error = u32;
    fn try_from(raw: u32) -> Result<Self, Self::Error> {
        match raw {
            0 => Ok(Self::ExceptionNmi),
            1 => Ok(Self::ExternalInterrupt),
            2 => Ok(Self::TripleFault),
            3 => Ok(Self::Init),
            4 => Ok(Self::Sipi),
            5 => Ok(Self::Smi),
            6 => Ok(Self::OtherSmi),
            7 => Ok(Self::InterruptWindow),
            8 => Ok(Self::NmiWindow),
            9 => Ok(Self::TaskSwitch),
            10 => Ok(Self::Cpuid),
            11 => Ok(Self::Getsec),
            12 => Ok(Self::Hlt),
            13 => Ok(Self::Invd),
            14 => Ok(Self::Invlpg),
            15 => Ok(Self::Rdpmc),
            16 => Ok(Self::Rdtsc),
            17 => Ok(Self::Rsm),
            18 => Ok(Self::Vmcall),
            19 => Ok(Self::Vmclear),
            20 => Ok(Self::Vmlaunch),
            21 => Ok(Self::Vmptrld),
            22 => Ok(Self::Vmptrst),
            23 => Ok(Self::Vmread),
            24 => Ok(Self::Vmresume),
            25 => Ok(Self::Vmwrite),
            26 => Ok(Self::Vmoff),
            27 => Ok(Self::Vmon),
            28 => Ok(Self::CrAccess),
            29 => Ok(Self::DrAccess),
            30 => Ok(Self::IoInstruction),
            31 => Ok(Self::MsrRead),
            32 => Ok(Self::MsrWrite),
            33 => Ok(Self::InvalidGuestState),
            34 => Ok(Self::MsrLoadFail),
            36 => Ok(Self::MwaitInstruction),
            37 => Ok(Self::MonitorTrapFlag),
            39 => Ok(Self::MonitorInstruction),
            40 => Ok(Self::PauseInstruction),
            41 => Ok(Self::MceDuringVmentry),
            43 => Ok(Self::TprBelowThreshold),
            44 => Ok(Self::ApicAccess),
            45 => Ok(Self::VirtualizedEoi),
            46 => Ok(Self::GdtrIdtr),
            47 => Ok(Self::LdtrTr),
            48 => Ok(Self::EptViolation),
            49 => Ok(Self::EptMisconfig),
            50 => Ok(Self::Invept),
            51 => Ok(Self::Rdtscp),
            52 => Ok(Self::PreemptionTimer),
            53 => Ok(Self::Invvpid),
            54 => Ok(Self::Wbinvd),
            55 => Ok(Self::Xsetbv),
            56 => Ok(Self::ApicWrite),
            57 => Ok(Self::Rdrand),
            58 => Ok(Self::Invpcid),
            59 => Ok(Self::Vmfunc),
            60 => Ok(Self::Encls),
            61 => Ok(Self::Rdseed),
            62 => Ok(Self::PmlFull),
            63 => Ok(Self::Xsaves),
            64 => Ok(Self::Xrstors),
            65 => Ok(Self::Pconfig),
            66 => Ok(Self::SppEvent),
            67 => Ok(Self::Umwait),
            68 => Ok(Self::Tpause),
            69 => Ok(Self::Loadiwkey),
            _ => Err(raw),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
/// The interruption type (bits 10:8) in VM-Entry Interruption-Information Field
/// and VM-Exit Interruption-Information Field. (SDM Vol. 3C, Section 24.8.3, 24.9.2)
pub enum VmxInterruptionType {
    /// External interrupt
    External          = 0,
    /// Reserved
    Reserved          = 1,
    /// Non-maskable interrupt (NMI)
    Nmi               = 2,
    /// Hardware exception (e.g,. #PF)
    HardException     = 3,
    /// Software interrupt (INT n)
    SoftIntr          = 4,
    /// Privileged software exception (INT1)
    PrivSoftException = 5,
    /// Software exception (INT3 or INTO)
    SoftException     = 6,
    /// Other event
    Other             = 7,
}

impl TryFrom<u8> for VmxInterruptionType {
    type Error = u8;
    fn try_from(raw: u8) -> Result<Self, Self::Error> {
        match raw {
            0 => Ok(Self::External),
            1 => Ok(Self::Reserved),
            2 => Ok(Self::Nmi),
            3 => Ok(Self::HardException),
            4 => Ok(Self::SoftIntr),
            5 => Ok(Self::PrivSoftException),
            6 => Ok(Self::SoftException),
            7 => Ok(Self::Other),
            _ => Err(raw),
        }
    }
}

impl VmxInterruptionType {
    /// Whether the exception/interrupt with `vector` has an error code.
    pub const fn vector_has_error_code(vector: u8) -> bool {
        use x86::irq::*;
        matches!(
            vector,
            DOUBLE_FAULT_VECTOR
                | INVALID_TSS_VECTOR
                | SEGMENT_NOT_PRESENT_VECTOR
                | STACK_SEGEMENT_FAULT_VECTOR
                | GENERAL_PROTECTION_FAULT_VECTOR
                | PAGE_FAULT_VECTOR
                | ALIGNMENT_CHECK_VECTOR
        )
    }

    /// Determine interruption type by the interrupt vector.
    pub const fn from_vector(vector: u8) -> Self {
        // SDM Vol. 3C, Section 24.8.3
        use x86::irq::*;
        match vector {
            DEBUG_VECTOR => Self::PrivSoftException,
            NONMASKABLE_INTERRUPT_VECTOR => Self::Nmi,
            BREAKPOINT_VECTOR | OVERFLOW_VECTOR => Self::SoftException,
            // SDM Vol. 3A, Section 6.15: All other vectors from 0 to 21 are exceptions.
            0..=VIRTUALIZATION_VECTOR => Self::HardException,
            32..=255 => Self::External,
            _ => Self::Other,
        }
    }

    /// For software interrupt, software exception, or privileged software
    /// exception,we need to set VM-Entry Instruction Length Field.
    pub const fn is_soft(&self) -> bool {
        matches!(
            *self,
            Self::SoftIntr | Self::SoftException | Self::PrivSoftException
        )
    }
}

/// VM-Exit Informations. (SDM Vol. 3C, Section 24.9.1)
#[derive(Debug)]
pub struct VmxExitInfo {
    /// VM-entry failure. (0 = true VM exit; 1 = VM-entry failure)
    pub entry_failure: bool,
    /// Basic exit reason.
    pub exit_reason: Result<VmxExitReason, u32>,
    /// For VM exits resulting from instruction execution, this field receives
    /// the length in bytes of the instruction whose execution led to the VM exit.
    pub exit_instruction_length: u32,
    /// Guest `RIP` where the VM exit occurs.
    pub guest_rip: usize,
}

/// VM-Entry/VM-Exit Interruption-Information Field. (SDM Vol. 3C, Section 24.8.3, 24.9.2)
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmxInterruptInfo {
    /// Vector of interrupt or exception.
    pub vector: u8,
    /// Determines details of how the injection is performed.
    pub int_type: VmxInterruptionType,
    /// For hardware exceptions that would have delivered an error code on the stack.
    pub err_code: Option<u32>,
    /// Whether the field is valid.
    pub valid: bool,
}

impl VmxInterruptInfo {
    /// Convert from the interrupt vector and the error code.
    pub fn from(vector: u8, err_code: Option<u32>) -> Self {
        Self {
            vector,
            int_type: VmxInterruptionType::from_vector(vector),
            err_code,
            valid: true,
        }
    }

    /// Raw bits for writing to VMCS.
    pub fn bits(&self) -> u32 {
        let mut bits = self.vector as u32;
        bits |= (self.int_type as u32) << 8;
        bits |= u32::from(self.err_code.is_some()) << 11;
        bits |= u32::from(self.valid) << 31;
        bits
    }
}

/// Exit Qualification for I/O Instructions. (SDM Vol. 3C, Section 27.2.1, Table 27-5)
#[derive(Debug)]
pub struct VmxIoExitInfo {
    /// Size of access.
    pub access_size: u8,
    /// Direction of the attempted access (0 = OUT, 1 = IN).
    pub is_in: bool,
    /// String instruction (0 = not string; 1 = string).
    pub is_string: bool,
    /// REP prefixed (0 = not REP; 1 = REP).
    pub is_repeat: bool,
    /// X86Port number. (as specified in DX or in an immediate operand)
    pub port: u16,
}

/// Exit Qualification for Control Register Accesses. (SDM Vol. 3C, Section 28.2.1, Table 28-5)
#[derive(Debug)]
pub struct CrAccessInfo {
    /// [3:0]
    /// Number of control register
    ///     (0 for CLTS and LMSW).
    /// Bit 3 is always 0 on processors that do not support Intel 64 architecture as they do not support CR8.
    pub cr_number: u8,
    /// [5:4]
    /// Access type:
    ///     0 = MOV to CR
    ///     1 = MOV from CR
    ///     2 = CLTS
    ///     3 = LMSW
    pub access_type: u8,
    /// [6]
    /// LMSW operand type:
    ///     0 = register
    ///     1 = memory
    /// For CLTS and MOV CR, cleared to 0
    pub lmsw_op_type: u8,
    /// [11:8]
    /// For MOV CR, the general-purpose register:
    ///     0=RAX 1=RCX 2=RDX 3=RBX 4=RSP 5=RBP 6=RSI 7=RDI
    ///     8–15 represent R8–R15, respectively (used only on processors that support Intel 64 architecture)
    /// For CLTS and LMSW, cleared to 0
    pub gpr: u8,
    /// [31:16]
    /// For LMSW, the LMSW source data
    /// For CLTS and MOV CR, cleared to 0
    pub lmsw_source_data: u16,
}

impl CrAccessInfo {
    /// Decodes a copied control-register-access exit qualification.
    pub const fn decode(qualification: usize) -> Self {
        Self {
            cr_number: (qualification & 15) as u8,
            access_type: ((qualification >> 4) & 3) as u8,
            lmsw_op_type: ((qualification >> 6) & 1) as u8,
            gpr: ((qualification >> 8) & 15) as u8,
            lmsw_source_data: (qualification >> 16) as u16,
        }
    }
}

/// Type of APIC-access, used in Exit Qualification for APIC Accesses. (SDM Vol. 3C, Section 28.2.2, Table 28-6)
#[derive(Debug)]
pub enum ApicAccessExitType {
    /// Linear access for data read.
    LinearDataRead      = 0,
    /// Linear access for data write.
    LinearDataWrite     = 1,
    /// Linear access for instruction fetch.
    LinearInstructionFetch = 2,
    /// Linear access for event delivery.
    LinearEventDelivery = 3,
    /// Linear access for monitoring.
    LinearMonitoring    = 4,
    /// Guest-physical access for event delivery.
    GuestPhysicalEventDelivery = 10,
    /// Guest-physical access for monitoring.
    GuestPhysicalMonitoring = 11,
    /// Guest-physical access for instruction fetch, data read, or data write.
    GuestPhysicalInstructionFetchReadWrite = 15,
}

impl TryFrom<u8> for ApicAccessExitType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::LinearDataRead),
            1 => Ok(Self::LinearDataWrite),
            2 => Ok(Self::LinearInstructionFetch),
            3 => Ok(Self::LinearEventDelivery),
            4 => Ok(Self::LinearMonitoring),
            10 => Ok(Self::GuestPhysicalEventDelivery),
            11 => Ok(Self::GuestPhysicalMonitoring),
            15 => Ok(Self::GuestPhysicalInstructionFetchReadWrite),
            _ => Err(()),
        }
    }
}

/// Exit Qualification for APIC Accesses. (SDM Vol. 3C, Section 28.2.2, Table 28-6)
#[derive(Debug)]
pub struct ApicAccessExitInfo {
    /// Offset within the APIC-access page. Not defined if `access_type` is 10, 11, or 15.
    pub offset: u16,
    /// Access type.
    pub access_type: ApicAccessExitType,
    /// Actually not used by us, see SDM for details.
    pub non_event_delivery_asynchronous: bool,
}

impl<M: crate::virtualization::ControlMemory> super::VmxControls<M> {
    /// Copies the architectural exit record out of the current VMCS.
    /// Unknown basic reasons retain their raw number for the host policy.
    pub fn exit_info(&self) -> Result<VmxExitInfo, crate::virtualization::VirtualizationError> {
        use super::{VmcsGuestNW, VmcsReadOnly32};
        let reason = self.read(VmcsReadOnly32::EXIT_REASON)?;
        Ok(VmxExitInfo {
            exit_reason: VmxExitReason::try_from(reason & 0xffff),
            entry_failure: reason & (1 << 31) != 0,
            exit_instruction_length: self.read(VmcsReadOnly32::VMEXIT_INSTRUCTION_LEN)?,
            guest_rip: self.read(VmcsGuestNW::RIP)?,
        })
    }

    /// Copies the acknowledged interruption record, ignoring invalid payloads.
    pub fn interrupt_exit_info(
        &self,
    ) -> Result<VmxInterruptInfo, crate::virtualization::VirtualizationError> {
        use super::VmcsReadOnly32;
        let info = self.read(VmcsReadOnly32::VMEXIT_INTERRUPTION_INFO)?;
        let error = if info & ((1 << 31) | (1 << 11)) == ((1 << 31) | (1 << 11)) {
            Some(self.read(VmcsReadOnly32::VMEXIT_INTERRUPTION_ERR_CODE)?)
        } else {
            None
        };
        Ok(VmxInterruptInfo::decode(info, error))
    }

    /// Copies the event whose delivery was interrupted by this exit.
    pub fn idt_vectoring_info(
        &self,
    ) -> Result<VmxInterruptInfo, crate::virtualization::VirtualizationError> {
        use super::VmcsReadOnly32;
        let info = self.read(VmcsReadOnly32::IDT_VECTORING_INFO)?;
        let error = if info & ((1 << 31) | (1 << 11)) == ((1 << 31) | (1 << 11)) {
            Some(self.read(VmcsReadOnly32::IDT_VECTORING_ERR_CODE)?)
        } else {
            None
        };
        Ok(VmxInterruptInfo::decode(info, error))
    }

    /// Programs one explicitly classified VM-entry event.
    /// Software events require their original instruction length; callers must
    /// preserve that length when reinjecting an interrupted delivery.
    pub fn inject_interrupt(
        &mut self,
        info: VmxInterruptInfo,
        instruction_len: u32,
    ) -> Result<(), crate::virtualization::VirtualizationError> {
        use super::VmcsControl32;
        if let Some(error) = info.err_code {
            self.write(VmcsControl32::VMENTRY_EXCEPTION_ERR_CODE, error)?;
        }
        if info.int_type.is_soft() {
            self.write(VmcsControl32::VMENTRY_INSTRUCTION_LEN, instruction_len)?;
        }
        self.write(VmcsControl32::VMENTRY_INTERRUPTION_INFO_FIELD, info.bits())
    }

    /// Decodes the qualification of an I/O-instruction exit.
    pub fn io_exit_info(
        &self,
    ) -> Result<VmxIoExitInfo, crate::virtualization::VirtualizationError> {
        let value = self.read(super::VmcsReadOnlyNW::EXIT_QUALIFICATION)?;
        Ok(VmxIoExitInfo {
            access_size: (value & 7) as u8 + 1,
            is_in: value & (1 << 3) != 0,
            is_string: value & (1 << 4) != 0,
            is_repeat: value & (1 << 5) != 0,
            port: (value >> 16) as u16,
        })
    }
}

impl VmxInterruptInfo {
    /// Decodes a saved VM-entry/exit interruption record.
    /// Undefined payload bits in an invalid record have no meaning.
    pub fn decode(info: u32, error_code: Option<u32>) -> Self {
        if info & (1 << 31) == 0 {
            return Self {
                vector: 0,
                int_type: VmxInterruptionType::External,
                err_code: None,
                valid: false,
            };
        }
        // The architectural three-bit field includes all eight encodings.
        let int_type = match (info >> 8) & 7 {
            0 => VmxInterruptionType::External,
            1 => VmxInterruptionType::Reserved,
            2 => VmxInterruptionType::Nmi,
            3 => VmxInterruptionType::HardException,
            4 => VmxInterruptionType::SoftIntr,
            5 => VmxInterruptionType::PrivSoftException,
            6 => VmxInterruptionType::SoftException,
            _ => VmxInterruptionType::Other,
        };
        Self {
            vector: info as u8,
            int_type,
            err_code: if info & (1 << 11) != 0 {
                error_code
            } else {
                None
            },
            valid: true,
        }
    }
}
