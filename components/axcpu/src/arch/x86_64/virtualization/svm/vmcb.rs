//! AMD VMCB control and state-save images.
//!
//! These images may only be accessed while hardware is not executing the guest.
//! Storage and its mapping are supplied by the host; no allocator is required.

use tock_registers::register_structs;
pub use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    registers::ReadWrite,
};

use super::exit::{SvmExitCode, SvmIntercept};

bitflags::bitflags! {
    /// Independent instruction and event interception controls.
    struct InterceptVec3: u32 {
        const INTR = 1 << 0;
        const NMI = 1 << 1;
        const SMI = 1 << 2;
        const INIT = 1 << 3;
        const VINTR = 1 << 4;
        const CR0_SEL_WRITE = 1 << 5;
        const IDTR_READ = 1 << 6;
        const GDTR_READ = 1 << 7;
        const LDTR_READ = 1 << 8;
        const TR_READ = 1 << 9;
        const IDTR_WRITE = 1 << 10;
        const GDTR_WRITE = 1 << 11;
        const LDTR_WRITE = 1 << 12;
        const TR_WRITE = 1 << 13;
        const RDTSC = 1 << 14;
        const RDPMC = 1 << 15;
        const PUSHF = 1 << 16;
        const POPF = 1 << 17;
        const CPUID = 1 << 18;
        const RSM = 1 << 19;
        const IRET = 1 << 20;
        const SWINT = 1 << 21;
        const INVD = 1 << 22;
        const PAUSE = 1 << 23;
        const HLT = 1 << 24;
        const INVLPG = 1 << 25;
        const INVLPGA = 1 << 26;
        const IOIO_PROT = 1 << 27;
        const MSR_PROT = 1 << 28;
        const TASK_SWITCH = 1 << 29;
        const FERR_FREEZE = 1 << 30;
        const SHUTDOWN = 1 << 31;
    }
}

bitflags::bitflags! {
    /// Independent instruction and event interception controls.
    struct InterceptVec4: u32 {
        const VMRUN = 1 << 0;
        const VMMCALL = 1 << 1;
        const VMLOAD = 1 << 2;
        const VMSAVE = 1 << 3;
        const STGI = 1 << 4;
        const CLGI = 1 << 5;
        const SKINIT = 1 << 6;
        const RDTSCP = 1 << 7;
        const ICEBP = 1 << 8;
        const WBINVD = 1 << 9;
        const MONITOR = 1 << 10;
        const MWAIT = 1 << 11;
        const MWAIT_CONDITIONAL = 1 << 12;
        const XSETBV = 1 << 13;
        const RDPRU = 1 << 14;
        const EFER_WRITE_TRAP = 1 << 15;
    }
}

bitflags::bitflags! {
    /// Independent instruction and event interception controls.
    struct InterceptVec5: u32 {
        const INVLPGB = 1 << 0;
        const INVLPGB_ILLEGAL = 1 << 1;
        const INVPCID = 1 << 2;
        const MCOMMIT = 1 << 3;
        const TLBSYNC = 1 << 4;
    }
}

bitflags::bitflags! {
    /// AMD VMCB intercept cr rw bits.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct VmcbCrIntercept: u32 {
        /// AMD APM `READ_CR0` bit.
        const READ_CR0 = 1 << 0;
        /// AMD APM `READ_CR3` bit.
        const READ_CR3 = 1 << 3;
        /// AMD APM `READ_CR4` bit.
        const READ_CR4 = 1 << 4;
        /// AMD APM `READ_CR8` bit.
        const READ_CR8 = 1 << 8;
        /// AMD APM `WRITE_CR0` bit.
        const WRITE_CR0 = 1 << 16;
        /// AMD APM `WRITE_CR3` bit.
        const WRITE_CR3 = 1 << 19;
        /// AMD APM `WRITE_CR4` bit.
        const WRITE_CR4 = 1 << 20;
        /// AMD APM `WRITE_CR8` bit.
        const WRITE_CR8 = 1 << 24;
    }
}

bitflags::bitflags! {
    /// AMD VMCB intercept dr rw bits.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct VmcbDrIntercept: u32 {
        /// AMD APM `READ_DR0` bit.
        const READ_DR0 = 1 << 0;
        /// AMD APM `READ_DR7` bit.
        const READ_DR7 = 1 << 7;
        /// AMD APM `WRITE_DR0` bit.
        const WRITE_DR0 = 1 << 16;
        /// AMD APM `WRITE_DR7` bit.
        const WRITE_DR7 = 1 << 23;
    }
}

bitflags::bitflags! {
    /// AMD VMCB intercept exceptions bits.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct VmcbExceptionIntercept: u32 {
        /// AMD APM `DE` bit.
        const DE = 1 << 0;
        /// AMD APM `DB` bit.
        const DB = 1 << 1;
        /// AMD APM `BP` bit.
        const BP = 1 << 3;
        /// AMD APM `OF` bit.
        const OF = 1 << 4;
        /// AMD APM `UD` bit.
        const UD = 1 << 6;
        /// AMD APM `DF` bit.
        const DF = 1 << 8;
        /// AMD APM `GP` bit.
        const GP = 1 << 13;
        /// AMD APM `PF` bit.
        const PF = 1 << 14;
        /// AMD APM `MC` bit.
        const MC = 1 << 18;
    }
}

bitflags::bitflags! {
    /// AMD VMCB vmcb clean bits bits.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct VmcbCleanBits: u32 {
        /// AMD APM `INTERCEPTS` bit.
        const INTERCEPTS = 1 << 0;
        /// AMD APM `IOPM` bit.
        const IOPM = 1 << 1;
        /// AMD APM `ASID` bit.
        const ASID = 1 << 2;
        /// AMD APM `TPR` bit.
        const TPR = 1 << 3;
        /// AMD APM `NP` bit.
        const NP = 1 << 4;
        /// AMD APM `CR_X` bit.
        const CR_X = 1 << 5;
        /// AMD APM `DR_X` bit.
        const DR_X = 1 << 6;
        /// AMD APM `DT` bit.
        const DT = 1 << 7;
        /// AMD APM `SEG` bit.
        const SEG = 1 << 8;
        /// AMD APM `CR2` bit.
        const CR2 = 1 << 9;
        /// AMD APM `LBR` bit.
        const LBR = 1 << 10;
        /// AMD APM `AVIC` bit.
        const AVIC = 1 << 11;
        /// AMD APM `CET` bit.
        const CET = 1 << 12;
    }
}

bitflags::bitflags! {
    /// AMD VMCB nested ctl bits.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct VmcbNestedControl: u64 {
        /// AMD APM `NP_ENABLE` bit.
        const NP_ENABLE = 1 << 0;
        /// AMD APM `SEV_ENABLE` bit.
        const SEV_ENABLE = 1 << 1;
        /// AMD APM `SEV_ES_ENABLE` bit.
        const SEV_ES_ENABLE = 1 << 2;
        /// AMD APM `GMET_ENABLE` bit.
        const GMET_ENABLE = 1 << 3;
        /// AMD APM `SS_CHECK_EN` bit.
        const SS_CHECK_EN = 1 << 4;
        /// AMD APM `VTE_ENABLE` bit.
        const VTE_ENABLE = 1 << 5;
        /// AMD APM `RO_GPT_EN` bit.
        const RO_GPT_EN = 1 << 6;
        /// AMD APM `INVLPGB_TLBSYNC` bit.
        const INVLPGB_TLBSYNC = 1 << 7;
    }
}

/// TLB invalidation requested by the next VMRUN.
/// Per-ASID operations require CPUID's flush-by-ASID capability.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmcbTlbControl {
    /// Retain all cached translations.
    DoNothing           = 0,
    /// Invalidate translations for every ASID.
    FlushAll            = 1,
    /// Invalidate all translations for this guest ASID.
    FlushGuest          = 3,
    /// Invalidate only non-global translations for this guest ASID.
    FlushGuestNonGlobal = 7,
}

register_structs![
    /// AMD APM `VmcbControlArea` hardware image.
    pub VmcbControlArea {
        /// Hardware image field at offset `0x0000`.
        (0x0000 => pub intercept_cr: ReadWrite<u32>),
        /// Hardware image field at offset `0x0004`.
        (0x0004 => pub intercept_dr: ReadWrite<u32>),
        /// Hardware image field at offset `0x0008`.
        (0x0008 => pub intercept_exceptions: ReadWrite<u32>),
        (0x000c => pub(crate) intercept_vector3: ReadWrite<u32>),
        (0x0010 => pub(crate) intercept_vector4: ReadWrite<u32>),
        (0x0014 => pub(crate) intercept_vector5: ReadWrite<u32>),
        (0x0018 => _reserved_0018),
        /// Hardware image field at offset `0x003c`.
        (0x003c => pub pause_filter_thresh: ReadWrite<u16>),
        /// Hardware image field at offset `0x003e`.
        (0x003e => pub pause_filter_count: ReadWrite<u16>),
        /// Hardware image field at offset `0x0040`.
        (0x0040 => pub iopm_base_pa: ReadWrite<u64>),
        /// Hardware image field at offset `0x0048`.
        (0x0048 => pub msrpm_base_pa: ReadWrite<u64>),
        /// Hardware image field at offset `0x0050`.
        (0x0050 => pub tsc_offset: ReadWrite<u64>),
        /// Hardware image field at offset `0x0058`.
        (0x0058 => pub guest_asid: ReadWrite<u32>),
        /// Hardware image field at offset `0x005c`.
        (0x005c => pub tlb_control: ReadWrite<u8>),
        (0x005d => _reserved_005d),
        /// Hardware image field at offset `0x0060`.
        (0x0060 => pub int_control: ReadWrite<u32>),
        /// Hardware image field at offset `0x0064`.
        (0x0064 => pub int_vector: ReadWrite<u32>),
        /// Hardware image field at offset `0x0068`.
        (0x0068 => pub int_state: ReadWrite<u32>),
        (0x006c => _reserved_006c),
        /// Hardware image field at offset `0x0070`.
        (0x0070 => pub exit_code: ReadWrite<u64>),
        /// Hardware image field at offset `0x0078`.
        (0x0078 => pub exit_info_1: ReadWrite<u64>),
        /// Hardware image field at offset `0x0080`.
        (0x0080 => pub exit_info_2: ReadWrite<u64>),
        /// Hardware image field at offset `0x0088`.
        (0x0088 => pub exit_int_info: ReadWrite<u32>),
        /// Hardware image field at offset `0x008c`.
        (0x008c => pub exit_int_info_err: ReadWrite<u32>),
        /// Hardware image field at offset `0x0090`.
        (0x0090 => pub nested_ctl: ReadWrite<u64>),
        /// Hardware image field at offset `0x0098`.
        (0x0098 => pub avic_vapic_bar: ReadWrite<u64>),
        /// Hardware image field at offset `0x00a0`.
        (0x00a0 => pub ghcb_gpa: ReadWrite<u64>),
        /// Hardware image field at offset `0x00a8`.
        (0x00a8 => pub event_inj: ReadWrite<u32>),
        /// Hardware image field at offset `0x00ac`.
        (0x00ac => pub event_inj_err: ReadWrite<u32>),
        /// Hardware image field at offset `0x00b0`.
        (0x00b0 => pub nested_cr3: ReadWrite<u64>),
        /// Hardware image field at offset `0x00b8`.
        (0x00b8 => pub virt_ext: ReadWrite<u64>),
        /// Hardware image field at offset `0x00c0`.
        (0x00c0 => pub clean_bits: ReadWrite<u32>),
        /// Hardware image field at offset `0x00c4`.
        (0x00c4 => pub _rsvd5: ReadWrite<u32>),
        /// Hardware image field at offset `0x00c8`.
        (0x00c8 => pub next_rip: ReadWrite<u64>),
        /// Hardware image field at offset `0x00d0`.
        (0x00d0 => pub insn_len: ReadWrite<u8>),
        /// Hardware image field at offset `0x00d1`.
        (0x00d1 => pub insn_bytes: [ReadWrite<u8>; 15]),
        /// Hardware image field at offset `0x00e0`.
        (0x00e0 => pub avic_backing_page: ReadWrite<u64>),
        (0x00e8 => _reserved_00e8),
        /// Hardware image field at offset `0x00f0`.
        (0x00f0 => pub avic_logical_id: ReadWrite<u64>),
        /// Hardware image field at offset `0x00f8`.
        (0x00f8 => pub avic_physical_id: ReadWrite<u64>),
        (0x0100 => _reserved_0100),
        /// Hardware image field at offset `0x0108`.
        (0x0108 => pub vmsa_pa: ReadWrite<u64>),
        (0x0110 => _reserved_0110),
        /// Hardware image field at offset `0x0120`.
        (0x0120 => pub bus_lock_counter: ReadWrite<u16>),
        (0x0122 => _reserved_0122),
        /// Hardware image field at offset `0x0138`.
        (0x0138 => pub allowed_sev_features: ReadWrite<u64>),
        /// Hardware image field at offset `0x0140`.
        (0x0140 => pub guest_sev_features: ReadWrite<u64>),
        (0x0148 => _reserved_0148),
        (0x0400 => @END),
    }
];

register_structs![
    /// AMD APM `VmcbSegment` hardware image.
    pub VmcbSegment {
        /// Hardware image field at offset `0x0`.
        (0x0 => pub selector: ReadWrite<u16>),
        /// Hardware image field at offset `0x2`.
        (0x2 => pub attr: ReadWrite<u16>),
        /// Hardware image field at offset `0x4`.
        (0x4 => pub limit: ReadWrite<u32>),
        /// Hardware image field at offset `0x8`.
        (0x8 => pub base: ReadWrite<u64>),
        (0x10 => @END),
    }
];

register_structs![
    /// AMD APM `VmcbStateSaveArea` hardware image.
    pub VmcbStateSaveArea {
        /// Hardware image field at offset `0x0000`.
        (0x0000 => pub es: VmcbSegment),
        /// Hardware image field at offset `0x0010`.
        (0x0010 => pub cs: VmcbSegment),
        /// Hardware image field at offset `0x0020`.
        (0x0020 => pub ss: VmcbSegment),
        /// Hardware image field at offset `0x0030`.
        (0x0030 => pub ds: VmcbSegment),
        /// Hardware image field at offset `0x0040`.
        (0x0040 => pub fs: VmcbSegment),
        /// Hardware image field at offset `0x0050`.
        (0x0050 => pub gs: VmcbSegment),
        /// Hardware image field at offset `0x0060`.
        (0x0060 => pub gdtr: VmcbSegment),
        /// Hardware image field at offset `0x0070`.
        (0x0070 => pub ldtr: VmcbSegment),
        /// Hardware image field at offset `0x0080`.
        (0x0080 => pub idtr: VmcbSegment),
        /// Hardware image field at offset `0x0090`.
        (0x0090 => pub tr: VmcbSegment),
        (0x00a0 => _reserved_00a0),
        /// Hardware image field at offset `0x00cb`.
        (0x00cb => pub cpl: ReadWrite<u8>),
        (0x00cc => _reserved_00cc),
        /// Hardware image field at offset `0x00d0`.
        (0x00d0 => pub efer: ReadWrite<u64>),
        (0x00d8 => _reserved_00d8),
        /// Hardware image field at offset `0x0148`.
        (0x0148 => pub cr4: ReadWrite<u64>),
        /// Hardware image field at offset `0x0150`.
        (0x0150 => pub cr3: ReadWrite<u64>),
        /// Hardware image field at offset `0x0158`.
        (0x0158 => pub cr0: ReadWrite<u64>),
        /// Hardware image field at offset `0x0160`.
        (0x0160 => pub dr7: ReadWrite<u64>),
        /// Hardware image field at offset `0x0168`.
        (0x0168 => pub dr6: ReadWrite<u64>),
        /// Hardware image field at offset `0x0170`.
        (0x0170 => pub rflags: ReadWrite<u64>),
        /// Hardware image field at offset `0x0178`.
        (0x0178 => pub rip: ReadWrite<u64>),
        (0x0180 => _reserved_0180),
        /// Hardware image field at offset `0x01d8`.
        (0x01d8 => pub rsp: ReadWrite<u64>),
        /// Hardware image field at offset `0x01e0`.
        (0x01e0 => pub s_cet: ReadWrite<u64>),
        /// Hardware image field at offset `0x01e8`.
        (0x01e8 => pub ssp: ReadWrite<u64>),
        /// Hardware image field at offset `0x01f0`.
        (0x01f0 => pub isst_addr: ReadWrite<u64>),
        /// Hardware image field at offset `0x01f8`.
        (0x01f8 => pub rax: ReadWrite<u64>),
        /// Hardware image field at offset `0x0200`.
        (0x0200 => pub star: ReadWrite<u64>),
        /// Hardware image field at offset `0x0208`.
        (0x0208 => pub lstar: ReadWrite<u64>),
        /// Hardware image field at offset `0x0210`.
        (0x0210 => pub cstar: ReadWrite<u64>),
        /// Hardware image field at offset `0x0218`.
        (0x0218 => pub sfmask: ReadWrite<u64>),
        /// Hardware image field at offset `0x0220`.
        (0x0220 => pub kernel_gs_base: ReadWrite<u64>),
        /// Hardware image field at offset `0x0228`.
        (0x0228 => pub sysenter_cs: ReadWrite<u64>),
        /// Hardware image field at offset `0x0230`.
        (0x0230 => pub sysenter_esp: ReadWrite<u64>),
        /// Hardware image field at offset `0x0238`.
        (0x0238 => pub sysenter_eip: ReadWrite<u64>),
        /// Hardware image field at offset `0x0240`.
        (0x0240 => pub cr2: ReadWrite<u64>),
        (0x0248 => _reserved_0248),
        /// Hardware image field at offset `0x0268`.
        (0x0268 => pub g_pat: ReadWrite<u64>),
        /// Hardware image field at offset `0x0270`.
        (0x0270 => pub dbgctl: ReadWrite<u64>),
        /// Hardware image field at offset `0x0278`.
        (0x0278 => pub br_from: ReadWrite<u64>),
        /// Hardware image field at offset `0x0280`.
        (0x0280 => pub br_to: ReadWrite<u64>),
        /// Hardware image field at offset `0x0288`.
        (0x0288 => pub last_excp_from: ReadWrite<u64>),
        /// Hardware image field at offset `0x0290`.
        (0x0290 => pub last_excp_to: ReadWrite<u64>),
        (0x0298 => _reserved_0298),
        (0x0c00 => @END),
    }
];

register_structs![
    /// AMD APM `VmcbImage` hardware image.
    pub VmcbImage {
        /// Hardware image field at offset `0x0000`.
        (0x0000 => pub control: VmcbControlArea),
        /// Hardware image field at offset `0x0400`.
        (0x0400 => pub state: VmcbStateSaveArea),
        (0x1000 => @END),
    }
];

impl VmcbImage {
    /// Clears the control image while preserving saved guest registers.
    pub fn clear_control(&mut self) {
        // SAFETY: exclusive access to an inactive image; every control field
        // accepts zero and the hardware-defined control area is exactly 1 KiB.
        unsafe { core::ptr::write_bytes(&mut self.control as *mut _ as *mut u8, 0, 0x400) };
    }

    /// Copies the completed exit before the image can be reused.
    pub fn exit_info(&self) -> SvmExitInfo {
        SvmExitInfo {
            exit_code: self.control.exit_code.get().try_into(),
            exit_info_1: self.control.exit_info_1.get(),
            exit_info_2: self.control.exit_info_2.get(),
            guest_rip: self.state.rip.get(),
            guest_next_rip: self.control.next_rip.get(),
        }
    }
}

/// Initializes a real-mode segment with the specified selector and attributes.
pub fn set_vmcb_segment(seg: &mut VmcbSegment, selector: u16, attr: u16) {
    seg.selector.set(selector);
    seg.base.set(0);
    seg.limit.set(0xffff);
    seg.attr.set(attr);
}

impl VmcbControlArea {
    /// Reports whether one instruction or event is configured to cause an exit.
    pub fn intercepts(&self, intercept: SvmIntercept) -> bool {
        let (register, mask) = self.intercept_register(intercept);
        register.get() & mask != 0
    }

    /// Enable or disable one interception without changing other controls.
    pub fn set_intercept(&mut self, intercept: SvmIntercept, enabled: bool) {
        let (register, mask) = self.intercept_register(intercept);
        let bits = register.get();
        register.set(if enabled { bits | mask } else { bits & !mask });
    }

    fn intercept_register(&self, intercept: SvmIntercept) -> (&ReadWrite<u32>, u32) {
        use super::exit::SvmIntercept::*;
        match intercept {
            Intr => (&self.intercept_vector3, InterceptVec3::INTR.bits()),
            Nmi => (&self.intercept_vector3, InterceptVec3::NMI.bits()),
            Smi => (&self.intercept_vector3, InterceptVec3::SMI.bits()),
            Init => (&self.intercept_vector3, InterceptVec3::INIT.bits()),
            Vintr => (&self.intercept_vector3, InterceptVec3::VINTR.bits()),
            Cr0SelWrite => (&self.intercept_vector3, InterceptVec3::CR0_SEL_WRITE.bits()),
            IdtrRead => (&self.intercept_vector3, InterceptVec3::IDTR_READ.bits()),
            GdtrRead => (&self.intercept_vector3, InterceptVec3::GDTR_READ.bits()),
            LdtrRead => (&self.intercept_vector3, InterceptVec3::LDTR_READ.bits()),
            TrRead => (&self.intercept_vector3, InterceptVec3::TR_READ.bits()),
            IdtrWrite => (&self.intercept_vector3, InterceptVec3::IDTR_WRITE.bits()),
            GdtrWrite => (&self.intercept_vector3, InterceptVec3::GDTR_WRITE.bits()),
            LdtrWrite => (&self.intercept_vector3, InterceptVec3::LDTR_WRITE.bits()),
            TrWrite => (&self.intercept_vector3, InterceptVec3::TR_WRITE.bits()),
            Rdtsc => (&self.intercept_vector3, InterceptVec3::RDTSC.bits()),
            Rdpmc => (&self.intercept_vector3, InterceptVec3::RDPMC.bits()),
            Pushf => (&self.intercept_vector3, InterceptVec3::PUSHF.bits()),
            Popf => (&self.intercept_vector3, InterceptVec3::POPF.bits()),
            Cpuid => (&self.intercept_vector3, InterceptVec3::CPUID.bits()),
            Rsm => (&self.intercept_vector3, InterceptVec3::RSM.bits()),
            Iret => (&self.intercept_vector3, InterceptVec3::IRET.bits()),
            Swint => (&self.intercept_vector3, InterceptVec3::SWINT.bits()),
            Invd => (&self.intercept_vector3, InterceptVec3::INVD.bits()),
            Pause => (&self.intercept_vector3, InterceptVec3::PAUSE.bits()),
            Hlt => (&self.intercept_vector3, InterceptVec3::HLT.bits()),
            Invlpg => (&self.intercept_vector3, InterceptVec3::INVLPG.bits()),
            Invlpga => (&self.intercept_vector3, InterceptVec3::INVLPGA.bits()),
            IoioProt => (&self.intercept_vector3, InterceptVec3::IOIO_PROT.bits()),
            MsrProt => (&self.intercept_vector3, InterceptVec3::MSR_PROT.bits()),
            TaskSwitch => (&self.intercept_vector3, InterceptVec3::TASK_SWITCH.bits()),
            FerrFreeze => (&self.intercept_vector3, InterceptVec3::FERR_FREEZE.bits()),
            Shutdown => (&self.intercept_vector3, InterceptVec3::SHUTDOWN.bits()),
            Vmrun => (&self.intercept_vector4, InterceptVec4::VMRUN.bits()),
            Vmmcall => (&self.intercept_vector4, InterceptVec4::VMMCALL.bits()),
            Vmload => (&self.intercept_vector4, InterceptVec4::VMLOAD.bits()),
            Vmsave => (&self.intercept_vector4, InterceptVec4::VMSAVE.bits()),
            Stgi => (&self.intercept_vector4, InterceptVec4::STGI.bits()),
            Clgi => (&self.intercept_vector4, InterceptVec4::CLGI.bits()),
            Skinit => (&self.intercept_vector4, InterceptVec4::SKINIT.bits()),
            Rdtscp => (&self.intercept_vector4, InterceptVec4::RDTSCP.bits()),
            Icebp => (&self.intercept_vector4, InterceptVec4::ICEBP.bits()),
            Wbinvd => (&self.intercept_vector4, InterceptVec4::WBINVD.bits()),
            Monitor => (&self.intercept_vector4, InterceptVec4::MONITOR.bits()),
            Mwait => (&self.intercept_vector4, InterceptVec4::MWAIT.bits()),
            MwaitConditional => (
                &self.intercept_vector4,
                InterceptVec4::MWAIT_CONDITIONAL.bits(),
            ),
            Xsetbv => (&self.intercept_vector4, InterceptVec4::XSETBV.bits()),
            Rdpru => (&self.intercept_vector4, InterceptVec4::RDPRU.bits()),
            EferWriteTrap => (
                &self.intercept_vector4,
                InterceptVec4::EFER_WRITE_TRAP.bits(),
            ),
            Invlpgb => (&self.intercept_vector5, InterceptVec5::INVLPGB.bits()),
            InvlpgbIllegal => (
                &self.intercept_vector5,
                InterceptVec5::INVLPGB_ILLEGAL.bits(),
            ),
            Invpcid => (&self.intercept_vector5, InterceptVec5::INVPCID.bits()),
            Mcommit => (&self.intercept_vector5, InterceptVec5::MCOMMIT.bits()),
            Tlbsync => (&self.intercept_vector5, InterceptVec5::TLBSYNC.bits()),
        }
    }
}

/// A by-value snapshot of the completed VMRUN exit.
#[derive(Clone, Copy, Debug)]
pub struct SvmExitInfo {
    /// Saved exit code value.
    pub exit_code: core::result::Result<SvmExitCode, u64>,
    /// Saved exit info 1 value.
    pub exit_info_1: u64,
    /// Saved exit info 2 value.
    pub exit_info_2: u64,
    /// Saved guest rip value.
    pub guest_rip: u64,
    /// Saved guest next rip value.
    pub guest_next_rip: u64,
}
