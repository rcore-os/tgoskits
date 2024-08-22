use core::fmt::{Debug, Formatter, Result};

/// VM instruction error numbers. (SDM Vol. 3C, Section 30.4)
pub struct VmxInstructionError(u32);

impl VmxInstructionError {
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
            18 => "VM entry with executive-VMCS pointer not VMXON pointer (when attempting to deactivate the dual-monitor treatment of SMIs and SMM)",
            19 => "VMCALL with non-clear VMCS (when attempting to activate the dual-monitor treatment of SMIs and SMM)",
            20 => "VMCALL with invalid VM-exit control fields",
            22 => "VMCALL with incorrect MSEG revision identifier (when attempting to activate the dual-monitor treatment of SMIs and SMM)",
            23 => "VMXOFF under dual-monitor treatment of SMIs and SMM",
            24 => "VMCALL with invalid SMM-monitor features (when attempting to activate the dual-monitor treatment of SMIs and SMM)",
            25 => "VM entry with invalid VM-execution control fields in executive VMCS (when attempting to return from SMM)",
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
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, "VmxInstructionError({}, {:?})", self.0, self.as_str())
    }
}

numeric_enum_macro::numeric_enum! {
#[repr(u32)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[allow(non_camel_case_types)]
/// VMX basic exit reasons, as defined in the Intel Software Developer's Manual (SDM) Vol. 3D, Appendix C.
///
/// This enum represents the various reasons why a VM exit might occur during
/// the execution of a virtual machine in VMX (Virtual Machine Extensions) mode.
/// Each variant corresponds to a specific exit reason that can be identified
/// and handled by the hypervisor.
pub enum VmxExitReason {
    /// Exception or non-maskable interrupt (NMI) occurred.
    EXCEPTION_NMI = 0,
    /// An external interrupt was received.
    EXTERNAL_INTERRUPT = 1,
    /// A triple fault occurred.
    TRIPLE_FAULT = 2,
    /// INIT signal was received.
    INIT = 3,
    /// Startup IPI (SIPI) was received.
    SIPI = 4,
    /// System Management Interrupt (SMI) was received.
    SMI = 5,
    /// Other SMI was received.
    OTHER_SMI = 6,
    /// An interrupt window was open.
    INTERRUPT_WINDOW = 7,
    /// An NMI window was open.
    NMI_WINDOW = 8,
    /// A task switch occurred.
    TASK_SWITCH = 9,
    /// CPUID instruction was executed.
    CPUID = 10,
    /// GETSEC instruction was executed.
    GETSEC = 11,
    /// HLT instruction was executed.
    HLT = 12,
    /// INVD instruction was executed.
    INVD = 13,
    /// INVLPG instruction was executed.
    INVLPG = 14,
    /// RDPMC instruction was executed.
    RDPMC = 15,
    /// RDTSC instruction was executed.
    RDTSC = 16,
    /// RSM instruction was executed in SMM.
    RSM = 17,
    /// VMCALL instruction was executed.
    VMCALL = 18,
    /// VMCLEAR instruction was executed.
    VMCLEAR = 19,
    /// VMLAUNCH instruction was executed.
    VMLAUNCH = 20,
    /// VMPTRLD instruction was executed.
    VMPTRLD = 21,
    /// VMPTRST instruction was executed.
    VMPTRST = 22,
    /// VMREAD instruction was executed.
    VMREAD = 23,
    /// VMRESUME instruction was executed.
    VMRESUME = 24,
    /// VMWRITE instruction was executed.
    VMWRITE = 25,
    /// VMOFF instruction was executed.
    VMOFF = 26,
    /// VMON instruction was executed.
    VMON = 27,
    /// Control Register (CR) access.
    CR_ACCESS = 28,
    /// Debug Register (DR) access.
    DR_ACCESS = 29,
    /// I/O instruction was executed.
    IO_INSTRUCTION = 30,
    /// Model-Specific Register (MSR) read.
    MSR_READ = 31,
    /// Model-Specific Register (MSR) write.
    MSR_WRITE = 32,
    /// Guest state is invalid.
    INVALID_GUEST_STATE = 33,
    /// MSR load failed.
    MSR_LOAD_FAIL = 34,
    /// MWAIT instruction was executed.
    MWAIT_INSTRUCTION = 36,
    /// Monitor trap flag triggered.
    MONITOR_TRAP_FLAG = 37,
    /// MONITOR instruction was executed.
    MONITOR_INSTRUCTION = 39,
    /// PAUSE instruction was executed.
    PAUSE_INSTRUCTION = 40,
    /// Machine Check Exception (MCE) occurred during VM entry.
    MCE_DURING_VMENTRY = 41,
    /// Task Priority Register (TPR) below threshold.
    TPR_BELOW_THRESHOLD = 43,
    /// Access to Advanced Programmable Interrupt Controller (APIC).
    APIC_ACCESS = 44,
    /// Virtualized End Of Interrupt (EOI) was executed.
    VIRTUALIZED_EOI = 45,
    /// Access to Global Descriptor Table Register (GDTR) or Interrupt Descriptor Table Register (IDTR).
    GDTR_IDTR = 46,
    /// Access to Local Descriptor Table Register (LDTR) or Task Register (TR).
    LDTR_TR = 47,
    /// Extended Page Table (EPT) violation occurred.
    EPT_VIOLATION = 48,
    /// Extended Page Table (EPT) misconfiguration occurred.
    EPT_MISCONFIG = 49,
    /// INVEPT instruction was executed.
    INVEPT = 50,
    /// RDTSCP instruction was executed.
    RDTSCP = 51,
    /// Preemption timer expired.
    PREEMPTION_TIMER = 52,
    /// INVVPID instruction was executed.
    INVVPID = 53,
    /// WBINVD instruction was executed.
    WBINVD = 54,
    /// XSETBV instruction was executed.
    XSETBV = 55,
    /// APIC write occurred.
    APIC_WRITE = 56,
    /// RDRAND instruction was executed.
    RDRAND = 57,
    /// INVPCID instruction was executed.
    INVPCID = 58,
    /// VMFUNC was executed.
    VMFUNC = 59,
    /// ENCLS instruction was executed.
    ENCLS = 60,
    /// RDSEED instruction was executed.
    RDSEED = 61,
    /// Page modification log (PML) became full.
    PML_FULL = 62,
    /// XSAVES instruction was executed.
    XSAVES = 63,
    /// XRSTORS instruction was executed.
    XRSTORS = 64,
    /// PCONFIG instruction was executed.
    PCONFIG = 65,
    /// SPP event occurred.
    SPP_EVENT = 66,
    /// UMWAIT instruction was executed.
    UMWAIT = 67,
    /// TPAUSE instruction was executed.
    TPAUSE = 68,
    /// LOADIWKEY instruction was executed.
    LOADIWKEY = 69,
}
}

numeric_enum_macro::numeric_enum! {
#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
/// The interruption type (bits 10:8) in VM-Entry Interruption-Information Field
/// and VM-Exit Interruption-Information Field. (SDM Vol. 3C, Section 24.8.3, 24.9.2)
pub enum VmxInterruptionType {
    /// External interrupt
    External = 0,
    /// Reserved
    Reserved = 1,
    /// Non-maskable interrupt (NMI)
    NMI = 2,
    /// Hardware exception (e.g,. #PF)
    HardException = 3,
    /// Software interrupt (INT n)
    SoftIntr = 4,
    /// Privileged software exception (INT1)
    PrivSoftException = 5,
    /// Software exception (INT3 or INTO)
    SoftException = 6,
    /// Other event
    Other = 7,
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
            NONMASKABLE_INTERRUPT_VECTOR => Self::NMI,
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
