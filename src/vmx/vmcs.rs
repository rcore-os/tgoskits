#![allow(dead_code)]
#![deny(missing_docs)]
#![allow(non_camel_case_types)]
#![allow(clippy::upper_case_acronyms)]

use bit_field::BitField;
use x86::bits64::vmx;

use axaddrspace::{GuestPhysAddr, HostPhysAddr, NestedPageFaultInfo};
use axerrno::{AxResult, ax_err};
use page_table_entry::MappingFlags;

use super::as_axerr;
use super::definitions::{VmxExitReason, VmxInstructionError, VmxInterruptionType};
use crate::msr::Msr;

// HYGIENE: These macros are only used in this file, so we can use `as_axerr` directly.

macro_rules! vmcs_read {
    ($field_enum: ident, u64) => {
        impl $field_enum {
            pub fn read(self) -> AxResult<u64> {
                #[cfg(target_pointer_width = "64")]
                unsafe {
                    vmx::vmread(self as u32).map_err(as_axerr)
                }
                #[cfg(target_pointer_width = "32")]
                unsafe {
                    let field = self as u32;
                    Ok(vmx::vmread(field).map_err(as_axerr)?
                        + (vmx::vmread(field + 1).map_err(as_axerr)? << 32))
                }
            }
        }
    };
    ($field_enum: ident, $ux: ty) => {
        impl $field_enum {
            pub fn read(self) -> AxResult<$ux> {
                unsafe { vmx::vmread(self as u32).map(|v| v as $ux).map_err(as_axerr) }
            }
        }
    };
}

macro_rules! vmcs_write {
    ($field_enum: ident, u64) => {
        impl $field_enum {
            pub fn write(self, value: u64) -> AxResult {
                #[cfg(target_pointer_width = "64")]
                unsafe {
                    vmx::vmwrite(self as u32, value).map_err(as_axerr)
                }
                #[cfg(target_pointer_width = "32")]
                unsafe {
                    let field = self as u32;
                    vmx::vmwrite(field, value & 0xffff_ffff).map_err(as_axerr)?;
                    vmx::vmwrite(field + 1, value >> 32).map_err(as_axerr)?;
                    Ok(())
                }
            }
        }
    };
    ($field_enum: ident, $ux: ty) => {
        impl $field_enum {
            pub fn write(self, value: $ux) -> AxResult {
                unsafe { vmx::vmwrite(self as u32, value as u64).map_err(as_axerr) }
            }
        }
    };
}

macro_rules! define_vmcs_fields_ro {
    ($field_enum:ident, $ty:ty) => {
        vmcs_read!($field_enum, $ty);
    };
}

macro_rules! define_vmcs_fields_rw {
    ($field_enum:ident, $ty:ty) => {
        vmcs_read!($field_enum, $ty);
        vmcs_write!($field_enum, $ty);
    };
}

/// 16-Bit Control Fields. (SDM Vol. 3D, Appendix B.1.1)
#[derive(Clone, Copy, Debug)]
pub enum VmcsControl16 {
    /// Virtual-processor identifier (VPID).
    VPID = 0x0,
    /// Posted-interrupt notification vector.
    POSTED_INTERRUPT_NOTIFICATION_VECTOR = 0x2,
    /// EPTP index.
    EPTP_INDEX = 0x4,
}
define_vmcs_fields_rw!(VmcsControl16, u16);

/// 64-Bit Control Fields. (SDM Vol. 3D, Appendix B.2.1)
#[derive(Clone, Copy, Debug)]
pub enum VmcsControl64 {
    /// Address of I/O bitmap A (full).
    IO_BITMAP_A_ADDR = 0x2000,
    /// Address of I/O bitmap B (full).
    IO_BITMAP_B_ADDR = 0x2002,
    /// Address of MSR bitmaps (full).
    MSR_BITMAPS_ADDR = 0x2004,
    /// VM-exit MSR-store address (full).
    VMEXIT_MSR_STORE_ADDR = 0x2006,
    /// VM-exit MSR-load address (full).
    VMEXIT_MSR_LOAD_ADDR = 0x2008,
    /// VM-entry MSR-load address (full).
    VMENTRY_MSR_LOAD_ADDR = 0x200A,
    /// Executive-VMCS pointer (full).
    EXECUTIVE_VMCS_PTR = 0x200C,
    /// PML address (full).
    PML_ADDR = 0x200E,
    /// TSC offset (full).
    TSC_OFFSET = 0x2010,
    /// Virtual-APIC address (full).
    VIRT_APIC_ADDR = 0x2012,
    /// APIC-access address (full).
    APIC_ACCESS_ADDR = 0x2014,
    /// Posted-interrupt descriptor address (full).
    POSTED_INTERRUPT_DESC_ADDR = 0x2016,
    /// VM-function controls (full).
    VM_FUNCTION_CONTROLS = 0x2018,
    /// EPT pointer (full).
    EPTP = 0x201A,
    /// EOI-exit bitmap 0 (full).
    EOI_EXIT0 = 0x201C,
    /// EOI-exit bitmap 1 (full).
    EOI_EXIT1 = 0x201E,
    /// EOI-exit bitmap 2 (full).
    EOI_EXIT2 = 0x2020,
    /// EOI-exit bitmap 3 (full).
    EOI_EXIT3 = 0x2022,
    /// EPTP-list address (full).
    EPTP_LIST_ADDR = 0x2024,
    /// VMREAD-bitmap address (full).
    VMREAD_BITMAP_ADDR = 0x2026,
    /// VMWRITE-bitmap address (full).
    VMWRITE_BITMAP_ADDR = 0x2028,
    /// Virtualization-exception information address (full).
    VIRT_EXCEPTION_INFO_ADDR = 0x202A,
    /// XSS-exiting bitmap (full).
    XSS_EXITING_BITMAP = 0x202C,
    /// ENCLS-exiting bitmap (full).
    ENCLS_EXITING_BITMAP = 0x202E,
    /// Sub-page-permission-table pointer (full).
    SUBPAGE_PERM_TABLE_PTR = 0x2030,
    /// TSC multiplier (full).
    TSC_MULTIPLIER = 0x2032,
}
define_vmcs_fields_rw!(VmcsControl64, u64);

/// 32-Bit Control Fields. (SDM Vol. 3D, Appendix B.3.1)
#[derive(Clone, Copy, Debug)]
pub enum VmcsControl32 {
    /// Pin-based VM-execution controls.
    PINBASED_EXEC_CONTROLS = 0x4000,
    /// Primary processor-based VM-execution controls.
    PRIMARY_PROCBASED_EXEC_CONTROLS = 0x4002,
    /// Exception bitmap.
    EXCEPTION_BITMAP = 0x4004,
    /// Page-fault error-code mask.
    PAGE_FAULT_ERR_CODE_MASK = 0x4006,
    /// Page-fault error-code match.
    PAGE_FAULT_ERR_CODE_MATCH = 0x4008,
    /// CR3-target count.
    CR3_TARGET_COUNT = 0x400A,
    /// VM-exit controls.
    VMEXIT_CONTROLS = 0x400C,
    /// VM-exit MSR-store count.
    VMEXIT_MSR_STORE_COUNT = 0x400E,
    /// VM-exit MSR-load count.
    VMEXIT_MSR_LOAD_COUNT = 0x4010,
    /// VM-entry controls.
    VMENTRY_CONTROLS = 0x4012,
    /// VM-entry MSR-load count.
    VMENTRY_MSR_LOAD_COUNT = 0x4014,
    /// VM-entry interruption-information field.
    VMENTRY_INTERRUPTION_INFO_FIELD = 0x4016,
    /// VM-entry exception error code.
    VMENTRY_EXCEPTION_ERR_CODE = 0x4018,
    /// VM-entry instruction length.
    VMENTRY_INSTRUCTION_LEN = 0x401A,
    /// TPR threshold.
    TPR_THRESHOLD = 0x401C,
    /// Secondary processor-based VM-execution controls.
    SECONDARY_PROCBASED_EXEC_CONTROLS = 0x401E,
    /// PLE_Gap.
    PLE_GAP = 0x4020,
    /// PLE_Window.
    PLE_WINDOW = 0x4022,
}
define_vmcs_fields_rw!(VmcsControl32, u32);

/// Natural-Width Control Fields. (SDM Vol. 3D, Appendix B.4.1)
#[derive(Clone, Copy, Debug)]
pub enum VmcsControlNW {
    /// CR0 guest/host mask.
    CR0_GUEST_HOST_MASK = 0x6000,
    /// CR4 guest/host mask.
    CR4_GUEST_HOST_MASK = 0x6002,
    /// CR0 read shadow.
    CR0_READ_SHADOW = 0x6004,
    /// CR4 read shadow.
    CR4_READ_SHADOW = 0x6006,
    /// CR3-target value 0.
    CR3_TARGET_VALUE0 = 0x6008,
    /// CR3-target value 1.
    CR3_TARGET_VALUE1 = 0x600A,
    /// CR3-target value 2.
    CR3_TARGET_VALUE2 = 0x600C,
    /// CR3-target value 3.
    CR3_TARGET_VALUE3 = 0x600E,
}
define_vmcs_fields_rw!(VmcsControlNW, usize);

/// 16-Bit Guest-State Fields. (SDM Vol. 3D, Appendix B.1.2)
pub enum VmcsGuest16 {
    /// Guest ES selector.
    ES_SELECTOR = 0x800,
    /// Guest CS selector.
    CS_SELECTOR = 0x802,
    /// Guest SS selector.
    SS_SELECTOR = 0x804,
    /// Guest DS selector.
    DS_SELECTOR = 0x806,
    /// Guest FS selector.
    FS_SELECTOR = 0x808,
    /// Guest GS selector.
    GS_SELECTOR = 0x80a,
    /// Guest LDTR selector.
    LDTR_SELECTOR = 0x80c,
    /// Guest TR selector.
    TR_SELECTOR = 0x80e,
    /// Guest interrupt status.
    INTERRUPT_STATUS = 0x810,
    /// PML index.
    PML_INDEX = 0x812,
}
define_vmcs_fields_rw!(VmcsGuest16, u16);

/// 64-Bit Guest-State Fields. (SDM Vol. 3D, Appendix B.2.3)
#[derive(Clone, Copy, Debug)]
pub enum VmcsGuest64 {
    /// VMCS link pointer (full).
    LINK_PTR = 0x2800,
    /// Guest IA32_DEBUGCTL (full).
    IA32_DEBUGCTL = 0x2802,
    /// Guest IA32_PAT (full).
    IA32_PAT = 0x2804,
    /// Guest IA32_EFER (full).
    IA32_EFER = 0x2806,
    /// Guest IA32_PERF_GLOBAL_CTRL (full).
    IA32_PERF_GLOBAL_CTRL = 0x2808,
    /// Guest PDPTE0 (full).
    PDPTE0 = 0x280A,
    /// Guest PDPTE1 (full).
    PDPTE1 = 0x280C,
    /// Guest PDPTE2 (full).
    PDPTE2 = 0x280E,
    /// Guest PDPTE3 (full).
    PDPTE3 = 0x2810,
    /// Guest IA32_BNDCFGS (full).
    IA32_BNDCFGS = 0x2812,
    /// Guest IA32_RTIT_CTL (full).
    IA32_RTIT_CTL = 0x2814,
}
define_vmcs_fields_rw!(VmcsGuest64, u64);

/// 32-Bit Guest-State Fields. (SDM Vol. 3D, Appendix B.3.3)
#[derive(Clone, Copy, Debug)]
pub enum VmcsGuest32 {
    /// Guest ES limit.
    ES_LIMIT = 0x4800,
    /// Guest CS limit.
    CS_LIMIT = 0x4802,
    /// Guest SS limit.
    SS_LIMIT = 0x4804,
    /// Guest DS limit.
    DS_LIMIT = 0x4806,
    /// Guest FS limit.
    FS_LIMIT = 0x4808,
    /// Guest GS limit.
    GS_LIMIT = 0x480A,
    /// Guest LDTR limit.
    LDTR_LIMIT = 0x480C,
    /// Guest TR limit.
    TR_LIMIT = 0x480E,
    /// Guest GDTR limit.
    GDTR_LIMIT = 0x4810,
    /// Guest IDTR limit.
    IDTR_LIMIT = 0x4812,
    /// Guest ES access rights.
    ES_ACCESS_RIGHTS = 0x4814,
    /// Guest CS access rights.
    CS_ACCESS_RIGHTS = 0x4816,
    /// Guest SS access rights.
    SS_ACCESS_RIGHTS = 0x4818,
    /// Guest DS access rights.
    DS_ACCESS_RIGHTS = 0x481A,
    /// Guest FS access rights.
    FS_ACCESS_RIGHTS = 0x481C,
    /// Guest GS access rights.
    GS_ACCESS_RIGHTS = 0x481E,
    /// Guest LDTR access rights.
    LDTR_ACCESS_RIGHTS = 0x4820,
    /// Guest TR access rights.
    TR_ACCESS_RIGHTS = 0x4822,
    /// Guest interruptibility state.
    INTERRUPTIBILITY_STATE = 0x4824,
    /// Guest activity state.
    ACTIVITY_STATE = 0x4826,
    /// Guest SMBASE.
    SMBASE = 0x4828,
    /// Guest IA32_SYSENTER_CS.
    IA32_SYSENTER_CS = 0x482A,
    /// VMX-preemption timer value.
    VMX_PREEMPTION_TIMER_VALUE = 0x482E,
}
define_vmcs_fields_rw!(VmcsGuest32, u32);

/// Natural-Width Guest-State Fields. (SDM Vol. 3D, Appendix B.4.3)
#[derive(Clone, Copy, Debug)]
pub enum VmcsGuestNW {
    /// Guest CR0.
    CR0 = 0x6800,
    /// Guest CR3.
    CR3 = 0x6802,
    /// Guest CR4.
    CR4 = 0x6804,
    /// Guest ES base.
    ES_BASE = 0x6806,
    /// Guest CS base.
    CS_BASE = 0x6808,
    /// Guest SS base.
    SS_BASE = 0x680A,
    /// Guest DS base.
    DS_BASE = 0x680C,
    /// Guest FS base.
    FS_BASE = 0x680E,
    /// Guest GS base.
    GS_BASE = 0x6810,
    /// Guest LDTR base.
    LDTR_BASE = 0x6812,
    /// Guest TR base.
    TR_BASE = 0x6814,
    /// Guest GDTR base.
    GDTR_BASE = 0x6816,
    /// Guest IDTR base.
    IDTR_BASE = 0x6818,
    /// Guest DR7.
    DR7 = 0x681A,
    /// Guest RSP.
    RSP = 0x681C,
    /// Guest RIP.
    RIP = 0x681E,
    /// Guest RFLAGS.
    RFLAGS = 0x6820,
    /// Guest pending debug exceptions.
    PENDING_DBG_EXCEPTIONS = 0x6822,
    /// Guest IA32_SYSENTER_ESP.
    IA32_SYSENTER_ESP = 0x6824,
    /// Guest IA32_SYSENTER_EIP.
    IA32_SYSENTER_EIP = 0x6826,
}
define_vmcs_fields_rw!(VmcsGuestNW, usize);

/// 16-Bit Host-State Fields. (SDM Vol. 3D, Appendix B.1.3)
#[derive(Clone, Copy, Debug)]
pub enum VmcsHost16 {
    /// Host ES selector.
    ES_SELECTOR = 0xC00,
    /// Host CS selector.
    CS_SELECTOR = 0xC02,
    /// Host SS selector.
    SS_SELECTOR = 0xC04,
    /// Host DS selector.
    DS_SELECTOR = 0xC06,
    /// Host FS selector.
    FS_SELECTOR = 0xC08,
    /// Host GS selector.
    GS_SELECTOR = 0xC0A,
    /// Host TR selector.
    TR_SELECTOR = 0xC0C,
}
define_vmcs_fields_rw!(VmcsHost16, u16);

/// 64-Bit Host-State Fields. (SDM Vol. 3D, Appendix B.2.4)
#[derive(Clone, Copy, Debug)]
pub enum VmcsHost64 {
    /// Host IA32_PAT (full).
    IA32_PAT = 0x2C00,
    /// Host IA32_EFER (full).
    IA32_EFER = 0x2C02,
    /// Host IA32_PERF_GLOBAL_CTRL (full).
    IA32_PERF_GLOBAL_CTRL = 0x2C04,
}
define_vmcs_fields_rw!(VmcsHost64, u64);

/// 32-Bit Host-State Field. (SDM Vol. 3D, Appendix B.3.4)
#[derive(Clone, Copy, Debug)]
pub enum VmcsHost32 {
    /// Host IA32_SYSENTER_CS.
    IA32_SYSENTER_CS = 0x4C00,
}
define_vmcs_fields_rw!(VmcsHost32, u32);

/// Natural-Width Host-State Fields. (SDM Vol. 3D, Appendix B.4.4)
#[derive(Clone, Copy, Debug)]
pub enum VmcsHostNW {
    /// Host CR0.
    CR0 = 0x6C00,
    /// Host CR3.
    CR3 = 0x6C02,
    /// Host CR4.
    CR4 = 0x6C04,
    /// Host FS base.
    FS_BASE = 0x6C06,
    /// Host GS base.
    GS_BASE = 0x6C08,
    /// Host TR base.
    TR_BASE = 0x6C0A,
    /// Host GDTR base.
    GDTR_BASE = 0x6C0C,
    /// Host IDTR base.
    IDTR_BASE = 0x6C0E,
    /// Host IA32_SYSENTER_ESP.
    IA32_SYSENTER_ESP = 0x6C10,
    /// Host IA32_SYSENTER_EIP.
    IA32_SYSENTER_EIP = 0x6C12,
    /// Host RSP.
    RSP = 0x6C14,
    /// Host RIP.
    RIP = 0x6C16,
}
define_vmcs_fields_rw!(VmcsHostNW, usize);

/// 64-Bit Read-Only Data Fields. (SDM Vol. 3D, Appendix B.2.2)
#[derive(Clone, Copy, Debug)]
pub enum VmcsReadOnly64 {
    /// Guest-physical address (full).
    GUEST_PHYSICAL_ADDR = 0x2400,
}
define_vmcs_fields_ro!(VmcsReadOnly64, u64);

/// 32-Bit Read-Only Data Fields. (SDM Vol. 3D, Appendix B.3.2)
#[derive(Clone, Copy, Debug)]
pub enum VmcsReadOnly32 {
    /// VM-instruction error.
    VM_INSTRUCTION_ERROR = 0x4400,
    /// Exit reason.
    EXIT_REASON = 0x4402,
    /// VM-exit interruption information.
    VMEXIT_INTERRUPTION_INFO = 0x4404,
    /// VM-exit interruption error code.
    VMEXIT_INTERRUPTION_ERR_CODE = 0x4406,
    /// IDT-vectoring information field.
    IDT_VECTORING_INFO = 0x4408,
    /// IDT-vectoring error code.
    IDT_VECTORING_ERR_CODE = 0x440A,
    /// VM-exit instruction length.
    VMEXIT_INSTRUCTION_LEN = 0x440C,
    /// VM-exit instruction information.
    VMEXIT_INSTRUCTION_INFO = 0x440E,
}
define_vmcs_fields_ro!(VmcsReadOnly32, u32);

/// Natural-Width Read-Only Data Fields. (SDM Vol. 3D, Appendix B.4.2)
#[derive(Clone, Copy, Debug)]
pub enum VmcsReadOnlyNW {
    /// Exit qualification.
    EXIT_QUALIFICATION = 0x6400,
    /// I/O RCX.
    IO_RCX = 0x6402,
    /// I/O RSI.
    IO_RSI = 0x6404,
    /// I/O RDI.
    IO_RDI = 0x6406,
    /// I/O RIP.
    IO_RIP = 0x6408,
    /// Guest-linear address.
    GUEST_LINEAR_ADDR = 0x640A,
}
define_vmcs_fields_ro!(VmcsReadOnlyNW, usize);

/// VM-Exit Informations. (SDM Vol. 3C, Section 24.9.1)
#[derive(Debug)]
pub struct VmxExitInfo {
    /// VM-entry failure. (0 = true VM exit; 1 = VM-entry failure)
    pub entry_failure: bool,
    /// Basic exit reason.
    pub exit_reason: VmxExitReason,
    /// For VM exits resulting from instruction execution, this field receives
    /// the length in bytes of the instruction whose execution led to the VM exit.
    pub exit_instruction_length: u32,
    /// Guest `RIP` where the VM exit occurs.
    pub guest_rip: usize,
}

/// VM-Entry/VM-Exit Interruption-Information Field. (SDM Vol. 3C, Section 24.8.3, 24.9.2)
#[derive(Debug)]
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
        bits.set_bit(11, self.err_code.is_some());
        bits.set_bit(31, self.valid);
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
    /// Port number. (as specified in DX or in an immediate operand)
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
    pub lmsw_source_data: u8,
}

/// Type of APIC-access, used in Exit Qualification for APIC Accesses. (SDM Vol. 3C, Section 28.2.2, Table 28-6)
#[derive(Debug)]
pub enum ApicAccessExitType {
    /// Linear access for data read.
    LinearDataRead = 0,
    /// Linear access for data write.
    LinearDataWrite = 1,
    /// Linear access for instruction fetch.
    LinearInstructionFetch = 2,
    /// Linear access for event delivery.
    LinearEventDelivery = 3,
    /// Linear access for monitoring.
    LinearMonitoring = 4,
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

pub mod controls {
    pub use x86::vmx::vmcs::control::{EntryControls, ExitControls};
    pub use x86::vmx::vmcs::control::{PinbasedControls, PrimaryControls, SecondaryControls};
}

pub fn set_control(
    control: VmcsControl32,
    capability_msr: Msr,
    old_value: u32,
    set: u32,
    clear: u32,
) -> AxResult {
    let cap = capability_msr.read();
    let allowed0 = cap as u32;
    let allowed1 = (cap >> 32) as u32;
    assert_eq!(allowed0 & allowed1, allowed0);
    debug!(
        "set {:?}: {:#x} (+{:#x}, -{:#x})",
        control, old_value, set, clear
    );
    if (set & clear) != 0 {
        return ax_err!(
            InvalidInput,
            format_args!("can not set and clear the same bit in {:?}", control)
        );
    }
    if (allowed1 & set) != set {
        // failed if set 0-bits in allowed1
        return ax_err!(
            Unsupported,
            format_args!("can not set bits {:#x} in {:?}", set, control)
        );
    }
    if (allowed0 & clear) != 0 {
        // failed if clear 1-bits in allowed0
        return ax_err!(
            Unsupported,
            format_args!("can not clear bits {:#x} in {:?}", clear, control)
        );
    }
    // SDM Vol. 3C, Section 31.5.1, Algorithm 3
    let flexible = !allowed0 & allowed1; // therse bits can be either 0 or 1
    let unknown = flexible & !(set | clear); // hypervisor untouched bits
    let default = unknown & old_value; // these bits keep unchanged in old value
    let fixed1 = allowed0; // these bits are fixed to 1
    control.write(fixed1 | default | set)?;
    Ok(())
}

pub fn set_ept_pointer(pml4_paddr: HostPhysAddr) -> AxResult {
    use super::instructions::{InvEptType, invept};
    let eptp = super::structs::EPTPointer::from_table_phys(pml4_paddr).bits();
    VmcsControl64::EPTP.write(eptp)?;
    unsafe { invept(InvEptType::SingleContext, eptp).map_err(as_axerr)? };
    Ok(())
}

pub fn instruction_error() -> VmxInstructionError {
    VmcsReadOnly32::VM_INSTRUCTION_ERROR.read().unwrap().into()
}

pub fn exit_info() -> AxResult<VmxExitInfo> {
    let full_reason = VmcsReadOnly32::EXIT_REASON.read()?;
    Ok(VmxExitInfo {
        exit_reason: full_reason
            .get_bits(0..16)
            .try_into()
            .expect("Unknown VM-exit reason"),
        entry_failure: full_reason.get_bit(31),
        exit_instruction_length: VmcsReadOnly32::VMEXIT_INSTRUCTION_LEN.read()?,
        guest_rip: VmcsGuestNW::RIP.read()?,
    })
}

pub fn raw_interrupt_exit_info() -> AxResult<u32> {
    VmcsReadOnly32::VMEXIT_INTERRUPTION_INFO.read()
}

pub fn interrupt_exit_info() -> AxResult<VmxInterruptInfo> {
    // SDM Vol. 3C, Section 24.9.2
    let info = VmcsReadOnly32::VMEXIT_INTERRUPTION_INFO.read()?;
    Ok(VmxInterruptInfo {
        vector: info.get_bits(0..8) as u8,
        int_type: VmxInterruptionType::try_from(info.get_bits(8..11) as u8).unwrap(),
        err_code: if info.get_bit(11) {
            Some(VmcsReadOnly32::VMEXIT_INTERRUPTION_ERR_CODE.read()?)
        } else {
            None
        },
        valid: info.get_bit(31),
    })
}

pub fn inject_event(vector: u8, err_code: Option<u32>) -> AxResult {
    // SDM Vol. 3C, Section 24.8.3
    let err_code = if VmxInterruptionType::vector_has_error_code(vector) {
        err_code.or_else(|| Some(VmcsReadOnly32::VMEXIT_INTERRUPTION_ERR_CODE.read().unwrap()))
    } else {
        None
    };
    let int_info = VmxInterruptInfo::from(vector, err_code);
    if let Some(err_code) = int_info.err_code {
        VmcsControl32::VMENTRY_EXCEPTION_ERR_CODE.write(err_code)?;
    }
    if int_info.int_type.is_soft() {
        VmcsControl32::VMENTRY_INSTRUCTION_LEN
            .write(VmcsReadOnly32::VMEXIT_INSTRUCTION_LEN.read()?)?;
    }
    VmcsControl32::VMENTRY_INTERRUPTION_INFO_FIELD.write(int_info.bits())?;
    Ok(())
}

pub fn io_exit_info() -> AxResult<VmxIoExitInfo> {
    // SDM Vol. 3C, Section 27.2.1, Table 27-5
    let qualification = VmcsReadOnlyNW::EXIT_QUALIFICATION.read()?;
    Ok(VmxIoExitInfo {
        access_size: qualification.get_bits(0..3) as u8 + 1,
        is_in: qualification.get_bit(3),
        is_string: qualification.get_bit(4),
        is_repeat: qualification.get_bit(5),
        port: qualification.get_bits(16..32) as u16,
    })
}

pub fn ept_violation_info() -> AxResult<NestedPageFaultInfo> {
    // SDM Vol. 3C, Section 27.2.1, Table 27-7
    let qualification = VmcsReadOnlyNW::EXIT_QUALIFICATION.read()?;
    let fault_guest_paddr = VmcsReadOnly64::GUEST_PHYSICAL_ADDR.read()? as usize;
    let mut access_flags = MappingFlags::empty();
    if qualification.get_bit(0) {
        access_flags |= MappingFlags::READ;
    }
    if qualification.get_bit(1) {
        access_flags |= MappingFlags::WRITE;
    }
    if qualification.get_bit(2) {
        access_flags |= MappingFlags::EXECUTE;
    }
    Ok(NestedPageFaultInfo {
        access_flags,
        fault_guest_paddr: GuestPhysAddr::from(fault_guest_paddr),
    })
}

pub fn update_efer() -> AxResult {
    use x86_64::registers::control::EferFlags;

    let efer = VmcsGuest64::IA32_EFER.read()?;
    let mut guest_efer = EferFlags::from_bits_truncate(efer);

    if guest_efer.contains(EferFlags::LONG_MODE_ENABLE)
        && guest_efer.contains(EferFlags::LONG_MODE_ACTIVE)
    {
        // debug!("Guest IA32_EFER LONG_MODE_ACTIVE is set, just return");
        return Ok(());
    }

    guest_efer.set(EferFlags::LONG_MODE_ACTIVE, true);

    // debug!(
    //     "Guest IA32_EFER from {:?} update to {:?}",
    //     EferFlags::from_bits_truncate(efer),
    //     guest_efer
    // );

    VmcsGuest64::IA32_EFER.write(guest_efer.bits())?;

    use controls::EntryControls as EntryCtrl;
    set_control(
        VmcsControl32::VMENTRY_CONTROLS,
        Msr::IA32_VMX_TRUE_ENTRY_CTLS,
        VmcsControl32::VMENTRY_CONTROLS.read()?,
        (EntryCtrl::IA32E_MODE_GUEST).bits(),
        0,
    )?;

    Ok(())
}

pub fn cr_access_info() -> AxResult<CrAccessInfo> {
    let qualification = VmcsReadOnlyNW::EXIT_QUALIFICATION.read()?;
    // debug!("cr_access_info qualification {:#x}", qualification);

    Ok(CrAccessInfo {
        cr_number: qualification.get_bits(0..4) as u8,
        access_type: qualification.get_bits(4..6) as u8,
        lmsw_op_type: qualification.get_bits(6..7) as u8,
        gpr: qualification.get_bits(8..12) as u8,
        lmsw_source_data: qualification.get_bits(16..32) as u8,
    })
}

pub fn apic_access_exit_info() -> AxResult<ApicAccessExitInfo> {
    let qualification = VmcsReadOnlyNW::EXIT_QUALIFICATION.read()?;
    // debug!("apic_access_info qualification {:#x}", qualification);

    Ok(ApicAccessExitInfo {
        offset: qualification.get_bits(0..12) as u16,
        access_type: ApicAccessExitType::try_from(qualification.get_bits(12..16) as u8).unwrap(),
        non_event_delivery_asynchronous: qualification.get_bit(16),
    })
}
