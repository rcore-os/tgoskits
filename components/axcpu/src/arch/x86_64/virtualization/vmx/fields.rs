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

//! Typed VMCS field encodings from Intel SDM Appendix B.

use core::{fmt, marker::PhantomData};

/// Read-only VMCS field access marker.
#[derive(Clone, Copy, Debug)]
pub struct VmcsReadOnly;
/// Read/write VMCS field access marker.
#[derive(Clone, Copy, Debug)]
pub struct VmcsReadWrite;

mod private {
    pub trait Value: Copy {
        fn from_raw(value: u64) -> Self;
        fn into_raw(self) -> u64;
    }
}

/// Sealed set of unsigned widths supported by VMCS fields.
pub trait VmcsValue: private::Value {}

macro_rules! value_widths {
    ($($word:ty),* $(,)?) => {$(
        impl private::Value for $word {
            fn from_raw(value: u64) -> Self { value as Self }
            fn into_raw(self) -> u64 { self as u64 }
        }
        impl VmcsValue for $word {}
    )*};
}
value_widths!(u16, u32, u64, usize);

/// A hardware field with a fixed value width and access direction.
#[derive(Clone, Copy)]
pub struct VmcsField<T, A> {
    encoding: u32,
    name: &'static str,
    word: PhantomData<(T, A)>,
}

impl<T, A> VmcsField<T, A> {
    const fn new(encoding: u32, name: &'static str) -> Self {
        Self {
            encoding,
            name,
            word: PhantomData,
        }
    }
    /// Returns the architectural VMREAD/VMWRITE field encoding.
    pub const fn encoding(self) -> u32 {
        self.encoding
    }
}

impl<T, A> fmt::Debug for VmcsField<T, A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name)
    }
}

pub(super) fn from_raw<T: VmcsValue>(value: u64) -> T {
    T::from_raw(value)
}
pub(super) fn into_raw<T: VmcsValue>(value: T) -> u64 {
    value.into_raw()
}

/// 16-Bit Control Fields. (SDM Vol. 3D, Appendix B.1.1)
pub struct VmcsControl16;

impl VmcsControl16 {
    /// Virtual-processor identifier (VPID).
    pub const VPID: VmcsField<u16, VmcsReadWrite> = VmcsField::new(0x0, "VmcsControl16::VPID");
    /// Posted-interrupt notification vector.
    pub const POSTED_INTERRUPT_NOTIFICATION_VECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x2, "VmcsControl16::POSTED_INTERRUPT_NOTIFICATION_VECTOR");
    /// EPTP index.
    pub const EPTP_INDEX: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x4, "VmcsControl16::EPTP_INDEX");
}

/// 64-Bit Control Fields. (SDM Vol. 3D, Appendix B.2.1)
pub struct VmcsControl64;

impl VmcsControl64 {
    /// Address of I/O bitmap A (full).
    pub const IO_BITMAP_A_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2000, "VmcsControl64::IO_BITMAP_A_ADDR");
    /// Address of I/O bitmap B (full).
    pub const IO_BITMAP_B_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2002, "VmcsControl64::IO_BITMAP_B_ADDR");
    /// Address of MSR bitmaps (full).
    pub const MSR_BITMAPS_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2004, "VmcsControl64::MSR_BITMAPS_ADDR");
    /// VM-exit MSR-store address (full).
    pub const VMEXIT_MSR_STORE_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2006, "VmcsControl64::VMEXIT_MSR_STORE_ADDR");
    /// VM-exit MSR-load address (full).
    pub const VMEXIT_MSR_LOAD_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2008, "VmcsControl64::VMEXIT_MSR_LOAD_ADDR");
    /// VM-entry MSR-load address (full).
    pub const VMENTRY_MSR_LOAD_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x200A, "VmcsControl64::VMENTRY_MSR_LOAD_ADDR");
    /// Executive-VMCS pointer (full).
    pub const EXECUTIVE_VMCS_PTR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x200C, "VmcsControl64::EXECUTIVE_VMCS_PTR");
    /// PML address (full).
    pub const PML_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x200E, "VmcsControl64::PML_ADDR");
    /// TSC offset (full).
    pub const TSC_OFFSET: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2010, "VmcsControl64::TSC_OFFSET");
    /// Virtual-APIC address (full).
    pub const VIRT_APIC_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2012, "VmcsControl64::VIRT_APIC_ADDR");
    /// APIC-access address (full).
    pub const APIC_ACCESS_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2014, "VmcsControl64::APIC_ACCESS_ADDR");
    /// Posted-interrupt descriptor address (full).
    pub const POSTED_INTERRUPT_DESC_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2016, "VmcsControl64::POSTED_INTERRUPT_DESC_ADDR");
    /// VM-function controls (full).
    pub const VM_FUNCTION_CONTROLS: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2018, "VmcsControl64::VM_FUNCTION_CONTROLS");
    /// EPT pointer (full).
    pub const EPTP: VmcsField<u64, VmcsReadWrite> = VmcsField::new(0x201A, "VmcsControl64::EPTP");
    /// EOI-exit bitmap 0 (full).
    pub const EOI_EXIT0: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x201C, "VmcsControl64::EOI_EXIT0");
    /// EOI-exit bitmap 1 (full).
    pub const EOI_EXIT1: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x201E, "VmcsControl64::EOI_EXIT1");
    /// EOI-exit bitmap 2 (full).
    pub const EOI_EXIT2: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2020, "VmcsControl64::EOI_EXIT2");
    /// EOI-exit bitmap 3 (full).
    pub const EOI_EXIT3: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2022, "VmcsControl64::EOI_EXIT3");
    /// EPTP-list address (full).
    pub const EPTP_LIST_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2024, "VmcsControl64::EPTP_LIST_ADDR");
    /// VMREAD-bitmap address (full).
    pub const VMREAD_BITMAP_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2026, "VmcsControl64::VMREAD_BITMAP_ADDR");
    /// VMWRITE-bitmap address (full).
    pub const VMWRITE_BITMAP_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2028, "VmcsControl64::VMWRITE_BITMAP_ADDR");
    /// Virtualization-exception information address (full).
    pub const VIRT_EXCEPTION_INFO_ADDR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x202A, "VmcsControl64::VIRT_EXCEPTION_INFO_ADDR");
    /// XSS-exiting bitmap (full).
    pub const XSS_EXITING_BITMAP: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x202C, "VmcsControl64::XSS_EXITING_BITMAP");
    /// ENCLS-exiting bitmap (full).
    pub const ENCLS_EXITING_BITMAP: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x202E, "VmcsControl64::ENCLS_EXITING_BITMAP");
    /// Sub-page-permission-table pointer (full).
    pub const SUBPAGE_PERM_TABLE_PTR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2030, "VmcsControl64::SUBPAGE_PERM_TABLE_PTR");
    /// TSC multiplier (full).
    pub const TSC_MULTIPLIER: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2032, "VmcsControl64::TSC_MULTIPLIER");
}

/// 32-Bit Control Fields. (SDM Vol. 3D, Appendix B.3.1)
pub struct VmcsControl32;

impl VmcsControl32 {
    /// Pin-based VM-execution controls.
    pub const PINBASED_EXEC_CONTROLS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4000, "VmcsControl32::PINBASED_EXEC_CONTROLS");
    /// Primary processor-based VM-execution controls.
    pub const PRIMARY_PROCBASED_EXEC_CONTROLS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4002, "VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS");
    /// Exception bitmap.
    pub const EXCEPTION_BITMAP: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4004, "VmcsControl32::EXCEPTION_BITMAP");
    /// Page-fault error-code mask.
    pub const PAGE_FAULT_ERR_CODE_MASK: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4006, "VmcsControl32::PAGE_FAULT_ERR_CODE_MASK");
    /// Page-fault error-code match.
    pub const PAGE_FAULT_ERR_CODE_MATCH: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4008, "VmcsControl32::PAGE_FAULT_ERR_CODE_MATCH");
    /// CR3-target count.
    pub const CR3_TARGET_COUNT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x400A, "VmcsControl32::CR3_TARGET_COUNT");
    /// VM-exit controls.
    pub const VMEXIT_CONTROLS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x400C, "VmcsControl32::VMEXIT_CONTROLS");
    /// VM-exit MSR-store count.
    pub const VMEXIT_MSR_STORE_COUNT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x400E, "VmcsControl32::VMEXIT_MSR_STORE_COUNT");
    /// VM-exit MSR-load count.
    pub const VMEXIT_MSR_LOAD_COUNT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4010, "VmcsControl32::VMEXIT_MSR_LOAD_COUNT");
    /// VM-entry controls.
    pub const VMENTRY_CONTROLS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4012, "VmcsControl32::VMENTRY_CONTROLS");
    /// VM-entry MSR-load count.
    pub const VMENTRY_MSR_LOAD_COUNT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4014, "VmcsControl32::VMENTRY_MSR_LOAD_COUNT");
    /// VM-entry interruption-information field.
    pub const VMENTRY_INTERRUPTION_INFO_FIELD: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4016, "VmcsControl32::VMENTRY_INTERRUPTION_INFO_FIELD");
    /// VM-entry exception error code.
    pub const VMENTRY_EXCEPTION_ERR_CODE: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4018, "VmcsControl32::VMENTRY_EXCEPTION_ERR_CODE");
    /// VM-entry instruction length.
    pub const VMENTRY_INSTRUCTION_LEN: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x401A, "VmcsControl32::VMENTRY_INSTRUCTION_LEN");
    /// TPR threshold.
    pub const TPR_THRESHOLD: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x401C, "VmcsControl32::TPR_THRESHOLD");
    /// Secondary processor-based VM-execution controls.
    pub const SECONDARY_PROCBASED_EXEC_CONTROLS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x401E, "VmcsControl32::SECONDARY_PROCBASED_EXEC_CONTROLS");
    /// PLE_Gap.
    pub const PLE_GAP: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4020, "VmcsControl32::PLE_GAP");
    /// PLE_Window.
    pub const PLE_WINDOW: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4022, "VmcsControl32::PLE_WINDOW");
}

/// Natural-Width Control Fields. (SDM Vol. 3D, Appendix B.4.1)
pub struct VmcsControlNW;

impl VmcsControlNW {
    /// CR0 guest/host mask.
    pub const CR0_GUEST_HOST_MASK: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6000, "VmcsControlNW::CR0_GUEST_HOST_MASK");
    /// CR4 guest/host mask.
    pub const CR4_GUEST_HOST_MASK: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6002, "VmcsControlNW::CR4_GUEST_HOST_MASK");
    /// CR0 read shadow.
    pub const CR0_READ_SHADOW: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6004, "VmcsControlNW::CR0_READ_SHADOW");
    /// CR4 read shadow.
    pub const CR4_READ_SHADOW: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6006, "VmcsControlNW::CR4_READ_SHADOW");
    /// CR3-target value 0.
    pub const CR3_TARGET_VALUE0: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6008, "VmcsControlNW::CR3_TARGET_VALUE0");
    /// CR3-target value 1.
    pub const CR3_TARGET_VALUE1: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x600A, "VmcsControlNW::CR3_TARGET_VALUE1");
    /// CR3-target value 2.
    pub const CR3_TARGET_VALUE2: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x600C, "VmcsControlNW::CR3_TARGET_VALUE2");
    /// CR3-target value 3.
    pub const CR3_TARGET_VALUE3: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x600E, "VmcsControlNW::CR3_TARGET_VALUE3");
}

/// 16-Bit Guest-State Fields. (SDM Vol. 3D, Appendix B.1.2)
pub struct VmcsGuest16;

impl VmcsGuest16 {
    /// Guest ES selector.
    pub const ES_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x800, "VmcsGuest16::ES_SELECTOR");
    /// Guest CS selector.
    pub const CS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x802, "VmcsGuest16::CS_SELECTOR");
    /// Guest SS selector.
    pub const SS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x804, "VmcsGuest16::SS_SELECTOR");
    /// Guest DS selector.
    pub const DS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x806, "VmcsGuest16::DS_SELECTOR");
    /// Guest FS selector.
    pub const FS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x808, "VmcsGuest16::FS_SELECTOR");
    /// Guest GS selector.
    pub const GS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x80a, "VmcsGuest16::GS_SELECTOR");
    /// Guest LDTR selector.
    pub const LDTR_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x80c, "VmcsGuest16::LDTR_SELECTOR");
    /// Guest TR selector.
    pub const TR_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x80e, "VmcsGuest16::TR_SELECTOR");
    /// Guest interrupt status.
    pub const INTERRUPT_STATUS: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x810, "VmcsGuest16::INTERRUPT_STATUS");
    /// PML index.
    pub const PML_INDEX: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0x812, "VmcsGuest16::PML_INDEX");
}

/// 64-Bit Guest-State Fields. (SDM Vol. 3D, Appendix B.2.3)
pub struct VmcsGuest64;

impl VmcsGuest64 {
    /// VMCS link pointer (full).
    pub const LINK_PTR: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2800, "VmcsGuest64::LINK_PTR");
    /// Guest IA32_DEBUGCTL (full).
    pub const IA32_DEBUGCTL: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2802, "VmcsGuest64::IA32_DEBUGCTL");
    /// Guest IA32_PAT (full).
    pub const IA32_PAT: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2804, "VmcsGuest64::IA32_PAT");
    /// Guest IA32_EFER (full).
    pub const IA32_EFER: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2806, "VmcsGuest64::IA32_EFER");
    /// Guest IA32_PERF_GLOBAL_CTRL (full).
    pub const IA32_PERF_GLOBAL_CTRL: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2808, "VmcsGuest64::IA32_PERF_GLOBAL_CTRL");
    /// Guest PDPTE0 (full).
    pub const PDPTE0: VmcsField<u64, VmcsReadWrite> = VmcsField::new(0x280A, "VmcsGuest64::PDPTE0");
    /// Guest PDPTE1 (full).
    pub const PDPTE1: VmcsField<u64, VmcsReadWrite> = VmcsField::new(0x280C, "VmcsGuest64::PDPTE1");
    /// Guest PDPTE2 (full).
    pub const PDPTE2: VmcsField<u64, VmcsReadWrite> = VmcsField::new(0x280E, "VmcsGuest64::PDPTE2");
    /// Guest PDPTE3 (full).
    pub const PDPTE3: VmcsField<u64, VmcsReadWrite> = VmcsField::new(0x2810, "VmcsGuest64::PDPTE3");
    /// Guest IA32_BNDCFGS (full).
    pub const IA32_BNDCFGS: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2812, "VmcsGuest64::IA32_BNDCFGS");
    /// Guest IA32_RTIT_CTL (full).
    pub const IA32_RTIT_CTL: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2814, "VmcsGuest64::IA32_RTIT_CTL");
}

/// 32-Bit Guest-State Fields. (SDM Vol. 3D, Appendix B.3.3)
pub struct VmcsGuest32;

impl VmcsGuest32 {
    /// Guest ES limit.
    pub const ES_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4800, "VmcsGuest32::ES_LIMIT");
    /// Guest CS limit.
    pub const CS_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4802, "VmcsGuest32::CS_LIMIT");
    /// Guest SS limit.
    pub const SS_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4804, "VmcsGuest32::SS_LIMIT");
    /// Guest DS limit.
    pub const DS_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4806, "VmcsGuest32::DS_LIMIT");
    /// Guest FS limit.
    pub const FS_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4808, "VmcsGuest32::FS_LIMIT");
    /// Guest GS limit.
    pub const GS_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x480A, "VmcsGuest32::GS_LIMIT");
    /// Guest LDTR limit.
    pub const LDTR_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x480C, "VmcsGuest32::LDTR_LIMIT");
    /// Guest TR limit.
    pub const TR_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x480E, "VmcsGuest32::TR_LIMIT");
    /// Guest GDTR limit.
    pub const GDTR_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4810, "VmcsGuest32::GDTR_LIMIT");
    /// Guest IDTR limit.
    pub const IDTR_LIMIT: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4812, "VmcsGuest32::IDTR_LIMIT");
    /// Guest ES access rights.
    pub const ES_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4814, "VmcsGuest32::ES_ACCESS_RIGHTS");
    /// Guest CS access rights.
    pub const CS_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4816, "VmcsGuest32::CS_ACCESS_RIGHTS");
    /// Guest SS access rights.
    pub const SS_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4818, "VmcsGuest32::SS_ACCESS_RIGHTS");
    /// Guest DS access rights.
    pub const DS_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x481A, "VmcsGuest32::DS_ACCESS_RIGHTS");
    /// Guest FS access rights.
    pub const FS_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x481C, "VmcsGuest32::FS_ACCESS_RIGHTS");
    /// Guest GS access rights.
    pub const GS_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x481E, "VmcsGuest32::GS_ACCESS_RIGHTS");
    /// Guest LDTR access rights.
    pub const LDTR_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4820, "VmcsGuest32::LDTR_ACCESS_RIGHTS");
    /// Guest TR access rights.
    pub const TR_ACCESS_RIGHTS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4822, "VmcsGuest32::TR_ACCESS_RIGHTS");
    /// Guest interruptibility state.
    pub const INTERRUPTIBILITY_STATE: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4824, "VmcsGuest32::INTERRUPTIBILITY_STATE");
    /// Guest activity state.
    pub const ACTIVITY_STATE: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4826, "VmcsGuest32::ACTIVITY_STATE");
    /// Guest SMBASE.
    pub const SMBASE: VmcsField<u32, VmcsReadWrite> = VmcsField::new(0x4828, "VmcsGuest32::SMBASE");
    /// Guest IA32_SYSENTER_CS.
    pub const IA32_SYSENTER_CS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x482A, "VmcsGuest32::IA32_SYSENTER_CS");
    /// VMX-preemption timer value.
    pub const VMX_PREEMPTION_TIMER_VALUE: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x482E, "VmcsGuest32::VMX_PREEMPTION_TIMER_VALUE");
}

/// Natural-Width Guest-State Fields. (SDM Vol. 3D, Appendix B.4.3)
pub struct VmcsGuestNW;

impl VmcsGuestNW {
    /// Guest CR0.
    pub const CR0: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6800, "VmcsGuestNW::CR0");
    /// Guest CR3.
    pub const CR3: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6802, "VmcsGuestNW::CR3");
    /// Guest CR4.
    pub const CR4: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6804, "VmcsGuestNW::CR4");
    /// Guest ES base.
    pub const ES_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6806, "VmcsGuestNW::ES_BASE");
    /// Guest CS base.
    pub const CS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6808, "VmcsGuestNW::CS_BASE");
    /// Guest SS base.
    pub const SS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x680A, "VmcsGuestNW::SS_BASE");
    /// Guest DS base.
    pub const DS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x680C, "VmcsGuestNW::DS_BASE");
    /// Guest FS base.
    pub const FS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x680E, "VmcsGuestNW::FS_BASE");
    /// Guest GS base.
    pub const GS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6810, "VmcsGuestNW::GS_BASE");
    /// Guest LDTR base.
    pub const LDTR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6812, "VmcsGuestNW::LDTR_BASE");
    /// Guest TR base.
    pub const TR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6814, "VmcsGuestNW::TR_BASE");
    /// Guest GDTR base.
    pub const GDTR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6816, "VmcsGuestNW::GDTR_BASE");
    /// Guest IDTR base.
    pub const IDTR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6818, "VmcsGuestNW::IDTR_BASE");
    /// Guest DR7.
    pub const DR7: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x681A, "VmcsGuestNW::DR7");
    /// Guest RSP.
    pub const RSP: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x681C, "VmcsGuestNW::RSP");
    /// Guest RIP.
    pub const RIP: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x681E, "VmcsGuestNW::RIP");
    /// Guest RFLAGS.
    pub const RFLAGS: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6820, "VmcsGuestNW::RFLAGS");
    /// Guest pending debug exceptions.
    pub const PENDING_DBG_EXCEPTIONS: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6822, "VmcsGuestNW::PENDING_DBG_EXCEPTIONS");
    /// Guest IA32_SYSENTER_ESP.
    pub const IA32_SYSENTER_ESP: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6824, "VmcsGuestNW::IA32_SYSENTER_ESP");
    /// Guest IA32_SYSENTER_EIP.
    pub const IA32_SYSENTER_EIP: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6826, "VmcsGuestNW::IA32_SYSENTER_EIP");
}

/// 16-Bit Host-State Fields. (SDM Vol. 3D, Appendix B.1.3)
pub struct VmcsHost16;

impl VmcsHost16 {
    /// Host ES selector.
    pub const ES_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC00, "VmcsHost16::ES_SELECTOR");
    /// Host CS selector.
    pub const CS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC02, "VmcsHost16::CS_SELECTOR");
    /// Host SS selector.
    pub const SS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC04, "VmcsHost16::SS_SELECTOR");
    /// Host DS selector.
    pub const DS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC06, "VmcsHost16::DS_SELECTOR");
    /// Host FS selector.
    pub const FS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC08, "VmcsHost16::FS_SELECTOR");
    /// Host GS selector.
    pub const GS_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC0A, "VmcsHost16::GS_SELECTOR");
    /// Host TR selector.
    pub const TR_SELECTOR: VmcsField<u16, VmcsReadWrite> =
        VmcsField::new(0xC0C, "VmcsHost16::TR_SELECTOR");
}

/// 64-Bit Host-State Fields. (SDM Vol. 3D, Appendix B.2.4)
pub struct VmcsHost64;

impl VmcsHost64 {
    /// Host IA32_PAT (full).
    pub const IA32_PAT: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2C00, "VmcsHost64::IA32_PAT");
    /// Host IA32_EFER (full).
    pub const IA32_EFER: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2C02, "VmcsHost64::IA32_EFER");
    /// Host IA32_PERF_GLOBAL_CTRL (full).
    pub const IA32_PERF_GLOBAL_CTRL: VmcsField<u64, VmcsReadWrite> =
        VmcsField::new(0x2C04, "VmcsHost64::IA32_PERF_GLOBAL_CTRL");
}

/// 32-Bit Host-State Field. (SDM Vol. 3D, Appendix B.3.4)
pub struct VmcsHost32;

impl VmcsHost32 {
    /// Host IA32_SYSENTER_CS.
    pub const IA32_SYSENTER_CS: VmcsField<u32, VmcsReadWrite> =
        VmcsField::new(0x4C00, "VmcsHost32::IA32_SYSENTER_CS");
}

/// Natural-Width Host-State Fields. (SDM Vol. 3D, Appendix B.4.4)
pub struct VmcsHostNW;

impl VmcsHostNW {
    /// Host CR0.
    pub const CR0: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6C00, "VmcsHostNW::CR0");
    /// Host CR3.
    pub const CR3: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6C02, "VmcsHostNW::CR3");
    /// Host CR4.
    pub const CR4: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6C04, "VmcsHostNW::CR4");
    /// Host FS base.
    pub const FS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C06, "VmcsHostNW::FS_BASE");
    /// Host GS base.
    pub const GS_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C08, "VmcsHostNW::GS_BASE");
    /// Host TR base.
    pub const TR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C0A, "VmcsHostNW::TR_BASE");
    /// Host GDTR base.
    pub const GDTR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C0C, "VmcsHostNW::GDTR_BASE");
    /// Host IDTR base.
    pub const IDTR_BASE: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C0E, "VmcsHostNW::IDTR_BASE");
    /// Host IA32_SYSENTER_ESP.
    pub const IA32_SYSENTER_ESP: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C10, "VmcsHostNW::IA32_SYSENTER_ESP");
    /// Host IA32_SYSENTER_EIP.
    pub const IA32_SYSENTER_EIP: VmcsField<usize, VmcsReadWrite> =
        VmcsField::new(0x6C12, "VmcsHostNW::IA32_SYSENTER_EIP");
    /// Host RSP.
    pub const RSP: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6C14, "VmcsHostNW::RSP");
    /// Host RIP.
    pub const RIP: VmcsField<usize, VmcsReadWrite> = VmcsField::new(0x6C16, "VmcsHostNW::RIP");
}

/// 64-Bit Read-Only Data Fields. (SDM Vol. 3D, Appendix B.2.2)
pub struct VmcsReadOnly64;

impl VmcsReadOnly64 {
    /// Guest-physical address (full).
    pub const GUEST_PHYSICAL_ADDR: VmcsField<u64, VmcsReadOnly> =
        VmcsField::new(0x2400, "VmcsReadOnly64::GUEST_PHYSICAL_ADDR");
}

/// 32-Bit Read-Only Data Fields. (SDM Vol. 3D, Appendix B.3.2)
pub struct VmcsReadOnly32;

impl VmcsReadOnly32 {
    /// VM-instruction error.
    pub const VM_INSTRUCTION_ERROR: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x4400, "VmcsReadOnly32::VM_INSTRUCTION_ERROR");
    /// Exit reason.
    pub const EXIT_REASON: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x4402, "VmcsReadOnly32::EXIT_REASON");
    /// VM-exit interruption information.
    pub const VMEXIT_INTERRUPTION_INFO: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x4404, "VmcsReadOnly32::VMEXIT_INTERRUPTION_INFO");
    /// VM-exit interruption error code.
    pub const VMEXIT_INTERRUPTION_ERR_CODE: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x4406, "VmcsReadOnly32::VMEXIT_INTERRUPTION_ERR_CODE");
    /// IDT-vectoring information field.
    pub const IDT_VECTORING_INFO: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x4408, "VmcsReadOnly32::IDT_VECTORING_INFO");
    /// IDT-vectoring error code.
    pub const IDT_VECTORING_ERR_CODE: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x440A, "VmcsReadOnly32::IDT_VECTORING_ERR_CODE");
    /// VM-exit instruction length.
    pub const VMEXIT_INSTRUCTION_LEN: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x440C, "VmcsReadOnly32::VMEXIT_INSTRUCTION_LEN");
    /// VM-exit instruction information.
    pub const VMEXIT_INSTRUCTION_INFO: VmcsField<u32, VmcsReadOnly> =
        VmcsField::new(0x440E, "VmcsReadOnly32::VMEXIT_INSTRUCTION_INFO");
}

/// Natural-Width Read-Only Data Fields. (SDM Vol. 3D, Appendix B.4.2)
pub struct VmcsReadOnlyNW;

impl VmcsReadOnlyNW {
    /// Exit qualification.
    pub const EXIT_QUALIFICATION: VmcsField<usize, VmcsReadOnly> =
        VmcsField::new(0x6400, "VmcsReadOnlyNW::EXIT_QUALIFICATION");
    /// I/O RCX.
    pub const IO_RCX: VmcsField<usize, VmcsReadOnly> =
        VmcsField::new(0x6402, "VmcsReadOnlyNW::IO_RCX");
    /// I/O RSI.
    pub const IO_RSI: VmcsField<usize, VmcsReadOnly> =
        VmcsField::new(0x6404, "VmcsReadOnlyNW::IO_RSI");
    /// I/O RDI.
    pub const IO_RDI: VmcsField<usize, VmcsReadOnly> =
        VmcsField::new(0x6406, "VmcsReadOnlyNW::IO_RDI");
    /// I/O RIP.
    pub const IO_RIP: VmcsField<usize, VmcsReadOnly> =
        VmcsField::new(0x6408, "VmcsReadOnlyNW::IO_RIP");
    /// Guest-linear address.
    pub const GUEST_LINEAR_ADDR: VmcsField<usize, VmcsReadOnly> =
        VmcsField::new(0x640A, "VmcsReadOnlyNW::GUEST_LINEAR_ADDR");
}
