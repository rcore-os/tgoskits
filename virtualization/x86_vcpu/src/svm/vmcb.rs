#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

use super::{
    definitions::{SvmExitCode, SvmIntercept},
    structs::VmcbFrame,
};
use crate::{X86HostOps, X86VcpuResult};

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

register_bitfields![u32,
    pub InterceptCrRw [
        READ_CR0 0, READ_CR3 3, READ_CR4 4, READ_CR8 8,
        WRITE_CR0 16, WRITE_CR3 19, WRITE_CR4 20, WRITE_CR8 24,
    ],
    pub InterceptDrRw [
        READ_DR0 0, READ_DR7 7,
        WRITE_DR0 16, WRITE_DR7 23,
    ],
    pub InterceptExceptions [
        DE 0, DB 1, BP 3, OF 4, UD 6, DF 8, GP 13, PF 14, MC 18,
    ],
    pub VmcbCleanBits [
        INTERCEPTS 0, IOPM 1, ASID 2, TPR 3, NP 4, CRx 5, DRx 6,
        DT 7, SEG 8, CR2 9, LBR 10, AVIC 11, CET 12,
    ],
];

register_bitfields![u64,
    pub NestedCtl [
        NP_ENABLE 0, SEV_ENABLE 1, SEV_ES_ENABLE 2, GMET_ENABLE 3,
        SSCheckEn 4, VTE_ENABLE 5, RO_GPT_EN 6, INVLPGB_TLBSYNC 7,
    ],
];

register_bitfields![u8,
    pub VmcbTlbControl [
        CONTROL OFFSET(0) NUMBITS(3) [
            DoNothing = 0,
            FlushAllOnVmrun = 1,
            FlushGuestTlb = 3,
            FlushGuestNonGlobalTlb = 7,
        ]
    ]
];

register_structs![
    pub VmcbControlArea {
        (0x0000 => pub intercept_cr: ReadWrite<u32, InterceptCrRw::Register>),
        (0x0004 => pub intercept_dr: ReadWrite<u32, InterceptDrRw::Register>),
        (0x0008 => pub intercept_exceptions: ReadWrite<u32, InterceptExceptions::Register>),
        (0x000c => pub(super) intercept_vector3: ReadWrite<u32>),
        (0x0010 => pub(super) intercept_vector4: ReadWrite<u32>),
        (0x0014 => pub(super) intercept_vector5: ReadWrite<u32>),
        (0x0018 => _reserved_0018),
        (0x003c => pub pause_filter_thresh: ReadWrite<u16>),
        (0x003e => pub pause_filter_count: ReadWrite<u16>),
        (0x0040 => pub iopm_base_pa: ReadWrite<u64>),
        (0x0048 => pub msrpm_base_pa: ReadWrite<u64>),
        (0x0050 => pub tsc_offset: ReadWrite<u64>),
        (0x0058 => pub guest_asid: ReadWrite<u32>),
        (0x005c => pub tlb_control: ReadWrite<u8, VmcbTlbControl::Register>),
        (0x005d => _reserved_005d),
        (0x0060 => pub int_control: ReadWrite<u32>),
        (0x0064 => pub int_vector: ReadWrite<u32>),
        (0x0068 => pub int_state: ReadWrite<u32>),
        (0x006c => _reserved_006c),
        (0x0070 => pub exit_code: ReadWrite<u64>),
        (0x0078 => pub exit_info_1: ReadWrite<u64>),
        (0x0080 => pub exit_info_2: ReadWrite<u64>),
        (0x0088 => pub exit_int_info: ReadWrite<u32>),
        (0x008c => pub exit_int_info_err: ReadWrite<u32>),
        (0x0090 => pub nested_ctl: ReadWrite<u64, NestedCtl::Register>),
        (0x0098 => pub avic_vapic_bar: ReadWrite<u64>),
        (0x00a0 => pub ghcb_gpa: ReadWrite<u64>),
        (0x00a8 => pub event_inj: ReadWrite<u32>),
        (0x00ac => pub event_inj_err: ReadWrite<u32>),
        (0x00b0 => pub nested_cr3: ReadWrite<u64>),
        (0x00b8 => pub virt_ext: ReadWrite<u64>),
        (0x00c0 => pub clean_bits: ReadWrite<u32, VmcbCleanBits::Register>),
        (0x00c4 => pub _rsvd5: ReadWrite<u32>),
        (0x00c8 => pub next_rip: ReadWrite<u64>),
        (0x00d0 => pub insn_len: ReadWrite<u8>),
        (0x00d1 => pub insn_bytes: [ReadWrite<u8>; 15]),
        (0x00e0 => pub avic_backing_page: ReadWrite<u64>),
        (0x00e8 => _reserved_00e8),
        (0x00f0 => pub avic_logical_id: ReadWrite<u64>),
        (0x00f8 => pub avic_physical_id: ReadWrite<u64>),
        (0x0100 => _reserved_0100),
        (0x0108 => pub vmsa_pa: ReadWrite<u64>),
        (0x0110 => _reserved_0110),
        (0x0120 => pub bus_lock_counter: ReadWrite<u16>),
        (0x0122 => _reserved_0122),
        (0x0138 => pub allowed_sev_features: ReadWrite<u64>),
        (0x0140 => pub guest_sev_features: ReadWrite<u64>),
        (0x0148 => _reserved_0148),
        (0x0400 => @END),
    }
];

register_structs![
    pub VmcbSegment {
        (0x0 => pub selector: ReadWrite<u16>),
        (0x2 => pub attr: ReadWrite<u16>),
        (0x4 => pub limit: ReadWrite<u32>),
        (0x8 => pub base: ReadWrite<u64>),
        (0x10 => @END),
    }
];

register_structs![
    pub VmcbStateSaveArea {
        (0x0000 => pub es: VmcbSegment),
        (0x0010 => pub cs: VmcbSegment),
        (0x0020 => pub ss: VmcbSegment),
        (0x0030 => pub ds: VmcbSegment),
        (0x0040 => pub fs: VmcbSegment),
        (0x0050 => pub gs: VmcbSegment),
        (0x0060 => pub gdtr: VmcbSegment),
        (0x0070 => pub ldtr: VmcbSegment),
        (0x0080 => pub idtr: VmcbSegment),
        (0x0090 => pub tr: VmcbSegment),
        (0x00a0 => _reserved_00a0),
        (0x00cb => pub cpl: ReadWrite<u8>),
        (0x00cc => _reserved_00cc),
        (0x00d0 => pub efer: ReadWrite<u64>),
        (0x00d8 => _reserved_00d8),
        (0x0148 => pub cr4: ReadWrite<u64>),
        (0x0150 => pub cr3: ReadWrite<u64>),
        (0x0158 => pub cr0: ReadWrite<u64>),
        (0x0160 => pub dr7: ReadWrite<u64>),
        (0x0168 => pub dr6: ReadWrite<u64>),
        (0x0170 => pub rflags: ReadWrite<u64>),
        (0x0178 => pub rip: ReadWrite<u64>),
        (0x0180 => _reserved_0180),
        (0x01d8 => pub rsp: ReadWrite<u64>),
        (0x01e0 => pub s_cet: ReadWrite<u64>),
        (0x01e8 => pub ssp: ReadWrite<u64>),
        (0x01f0 => pub isst_addr: ReadWrite<u64>),
        (0x01f8 => pub rax: ReadWrite<u64>),
        (0x0200 => pub star: ReadWrite<u64>),
        (0x0208 => pub lstar: ReadWrite<u64>),
        (0x0210 => pub cstar: ReadWrite<u64>),
        (0x0218 => pub sfmask: ReadWrite<u64>),
        (0x0220 => pub kernel_gs_base: ReadWrite<u64>),
        (0x0228 => pub sysenter_cs: ReadWrite<u64>),
        (0x0230 => pub sysenter_esp: ReadWrite<u64>),
        (0x0238 => pub sysenter_eip: ReadWrite<u64>),
        (0x0240 => pub cr2: ReadWrite<u64>),
        (0x0248 => _reserved_0248),
        (0x0268 => pub g_pat: ReadWrite<u64>),
        (0x0270 => pub dbgctl: ReadWrite<u64>),
        (0x0278 => pub br_from: ReadWrite<u64>),
        (0x0280 => pub br_to: ReadWrite<u64>),
        (0x0288 => pub last_excp_from: ReadWrite<u64>),
        (0x0290 => pub last_excp_to: ReadWrite<u64>),
        (0x0298 => _reserved_0298),
        (0x0c00 => @END),
    }
];

register_structs![
    pub VmcbStruct {
        (0x0000 => pub control: VmcbControlArea),
        (0x0400 => pub state: VmcbStateSaveArea),
        (0x1000 => @END),
    }
];

impl<H: X86HostOps> VmcbFrame<H> {
    /// # Safety
    ///
    /// The caller must ensure the VMCB page is mapped and no mutable reference
    /// to the same frame exists for the returned reference lifetime.
    pub unsafe fn as_vmcb_ref(&self) -> &VmcbStruct {
        unsafe { self.as_ptr_vmcb().as_ref().unwrap() }
    }

    /// # Safety
    ///
    /// The caller must ensure the VMCB page is mapped and uniquely owned for
    /// the returned mutable reference lifetime.
    pub unsafe fn as_vmcb(&mut self) -> &mut VmcbStruct {
        unsafe { self.as_mut_ptr_vmcb().as_mut().unwrap() }
    }
}

impl VmcbStruct {
    pub fn clear_control(&mut self) {
        unsafe { core::ptr::write_bytes(&mut self.control as *mut _ as *mut u8, 0, 0x400) };
    }

    pub fn exit_info(&self) -> X86VcpuResult<SvmExitInfo> {
        Ok(SvmExitInfo {
            exit_code: self.control.exit_code.get().try_into(),
            exit_info_1: self.control.exit_info_1.get(),
            exit_info_2: self.control.exit_info_2.get(),
            guest_rip: self.state.rip.get(),
            guest_next_rip: self.control.next_rip.get(),
        })
    }
}

pub fn set_vmcb_segment(seg: &mut VmcbSegment, selector: u16, attr: u16) {
    seg.selector.set(selector);
    seg.base.set(0);
    seg.limit.set(0xffff);
    seg.attr.set(attr);
}

impl VmcbControlArea {
    /// Enable or disable one interception without changing other controls.
    pub fn set_intercept(&mut self, intercept: SvmIntercept, enabled: bool) {
        let (register, mask) = self.intercept_register(intercept);
        let bits = register.get();
        register.set(if enabled { bits | mask } else { bits & !mask });
    }

    fn intercept_register(&self, intercept: SvmIntercept) -> (&ReadWrite<u32>, u32) {
        use super::definitions::SvmIntercept::*;
        match intercept {
            INTR => (&self.intercept_vector3, InterceptVec3::INTR.bits()),
            NMI => (&self.intercept_vector3, InterceptVec3::NMI.bits()),
            SMI => (&self.intercept_vector3, InterceptVec3::SMI.bits()),
            INIT => (&self.intercept_vector3, InterceptVec3::INIT.bits()),
            VINTR => (&self.intercept_vector3, InterceptVec3::VINTR.bits()),
            CR0_SEL_WRITE => (&self.intercept_vector3, InterceptVec3::CR0_SEL_WRITE.bits()),
            IDTR_READ => (&self.intercept_vector3, InterceptVec3::IDTR_READ.bits()),
            GDTR_READ => (&self.intercept_vector3, InterceptVec3::GDTR_READ.bits()),
            LDTR_READ => (&self.intercept_vector3, InterceptVec3::LDTR_READ.bits()),
            TR_READ => (&self.intercept_vector3, InterceptVec3::TR_READ.bits()),
            IDTR_WRITE => (&self.intercept_vector3, InterceptVec3::IDTR_WRITE.bits()),
            GDTR_WRITE => (&self.intercept_vector3, InterceptVec3::GDTR_WRITE.bits()),
            LDTR_WRITE => (&self.intercept_vector3, InterceptVec3::LDTR_WRITE.bits()),
            TR_WRITE => (&self.intercept_vector3, InterceptVec3::TR_WRITE.bits()),
            RDTSC => (&self.intercept_vector3, InterceptVec3::RDTSC.bits()),
            RDPMC => (&self.intercept_vector3, InterceptVec3::RDPMC.bits()),
            PUSHF => (&self.intercept_vector3, InterceptVec3::PUSHF.bits()),
            POPF => (&self.intercept_vector3, InterceptVec3::POPF.bits()),
            CPUID => (&self.intercept_vector3, InterceptVec3::CPUID.bits()),
            RSM => (&self.intercept_vector3, InterceptVec3::RSM.bits()),
            IRET => (&self.intercept_vector3, InterceptVec3::IRET.bits()),
            SWINT => (&self.intercept_vector3, InterceptVec3::SWINT.bits()),
            INVD => (&self.intercept_vector3, InterceptVec3::INVD.bits()),
            PAUSE => (&self.intercept_vector3, InterceptVec3::PAUSE.bits()),
            HLT => (&self.intercept_vector3, InterceptVec3::HLT.bits()),
            INVLPG => (&self.intercept_vector3, InterceptVec3::INVLPG.bits()),
            INVLPGA => (&self.intercept_vector3, InterceptVec3::INVLPGA.bits()),
            IOIO_PROT => (&self.intercept_vector3, InterceptVec3::IOIO_PROT.bits()),
            MSR_PROT => (&self.intercept_vector3, InterceptVec3::MSR_PROT.bits()),
            TASK_SWITCH => (&self.intercept_vector3, InterceptVec3::TASK_SWITCH.bits()),
            FERR_FREEZE => (&self.intercept_vector3, InterceptVec3::FERR_FREEZE.bits()),
            SHUTDOWN => (&self.intercept_vector3, InterceptVec3::SHUTDOWN.bits()),
            VMRUN => (&self.intercept_vector4, InterceptVec4::VMRUN.bits()),
            VMMCALL => (&self.intercept_vector4, InterceptVec4::VMMCALL.bits()),
            VMLOAD => (&self.intercept_vector4, InterceptVec4::VMLOAD.bits()),
            VMSAVE => (&self.intercept_vector4, InterceptVec4::VMSAVE.bits()),
            STGI => (&self.intercept_vector4, InterceptVec4::STGI.bits()),
            CLGI => (&self.intercept_vector4, InterceptVec4::CLGI.bits()),
            SKINIT => (&self.intercept_vector4, InterceptVec4::SKINIT.bits()),
            RDTSCP => (&self.intercept_vector4, InterceptVec4::RDTSCP.bits()),
            ICEBP => (&self.intercept_vector4, InterceptVec4::ICEBP.bits()),
            WBINVD => (&self.intercept_vector4, InterceptVec4::WBINVD.bits()),
            MONITOR => (&self.intercept_vector4, InterceptVec4::MONITOR.bits()),
            MWAIT => (&self.intercept_vector4, InterceptVec4::MWAIT.bits()),
            MWAIT_CONDITIONAL => (
                &self.intercept_vector4,
                InterceptVec4::MWAIT_CONDITIONAL.bits(),
            ),
            XSETBV => (&self.intercept_vector4, InterceptVec4::XSETBV.bits()),
            RDPRU => (&self.intercept_vector4, InterceptVec4::RDPRU.bits()),
            EFER_WRITE_TRAP => (
                &self.intercept_vector4,
                InterceptVec4::EFER_WRITE_TRAP.bits(),
            ),
            INVLPGB => (&self.intercept_vector5, InterceptVec5::INVLPGB.bits()),
            INVLPGB_ILLEGAL => (
                &self.intercept_vector5,
                InterceptVec5::INVLPGB_ILLEGAL.bits(),
            ),
            INVPCID => (&self.intercept_vector5, InterceptVec5::INVPCID.bits()),
            MCOMMIT => (&self.intercept_vector5, InterceptVec5::MCOMMIT.bits()),
            TLBSYNC => (&self.intercept_vector5, InterceptVec5::TLBSYNC.bits()),
        }
    }
}

#[derive(Debug)]
pub struct SvmExitInfo {
    pub exit_code: core::result::Result<SvmExitCode, u64>,
    pub exit_info_1: u64,
    pub exit_info_2: u64,
    pub guest_rip: u64,
    pub guest_next_rip: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_updates_preserve_other_vectors_and_reserved_bits() {
        // Every field in the hardware image accepts zero; no references or
        // nonzero-only values are present in the VMCB control area.
        let mut control: VmcbControlArea = unsafe { core::mem::zeroed() };
        control.intercept_vector4.set(1 << 31);
        for (intercept, vector, mask) in [
            (SvmIntercept::INTR, 3, 1),
            (SvmIntercept::SHUTDOWN, 3, 1 << 31),
            (SvmIntercept::VMRUN, 4, 1),
            (SvmIntercept::EFER_WRITE_TRAP, 4, 1 << 15),
            (SvmIntercept::INVLPGB, 5, 1),
            (SvmIntercept::TLBSYNC, 5, 1 << 4),
        ] {
            control.set_intercept(intercept, true);
            let words = [
                control.intercept_vector3.get(),
                control.intercept_vector4.get(),
                control.intercept_vector5.get(),
            ];
            let mut expected = [0, 1 << 31, 0];
            expected[vector - 3] |= mask;
            assert_eq!(words, expected);
            control.set_intercept(intercept, false);
            assert_eq!(control.intercept_vector3.get(), 0);
            assert_eq!(control.intercept_vector4.get(), 1 << 31);
            assert_eq!(control.intercept_vector5.get(), 0);
        }
    }

    #[test]
    fn vmcb_size_check() {
        use core::mem::size_of;

        assert_eq!(size_of::<VmcbControlArea>(), 0x400);
        assert_eq!(size_of::<VmcbStateSaveArea>(), 0xc00);
        assert_eq!(size_of::<VmcbStruct>(), 0x1000);
    }

    #[test]
    fn vmcb_offset_check() {
        use memoffset::offset_of;

        assert_eq!(offset_of!(VmcbStruct, control), 0x0000);
        assert_eq!(offset_of!(VmcbStruct, state), 0x0400);

        assert_eq!(offset_of!(VmcbControlArea, intercept_cr), 0x0000);
        assert_eq!(offset_of!(VmcbControlArea, intercept_dr), 0x0004);
        assert_eq!(offset_of!(VmcbControlArea, intercept_exceptions), 0x0008);
        assert_eq!(offset_of!(VmcbControlArea, intercept_vector3), 0x000c);
        assert_eq!(offset_of!(VmcbControlArea, intercept_vector4), 0x0010);
        assert_eq!(offset_of!(VmcbControlArea, intercept_vector5), 0x0014);
        assert_eq!(offset_of!(VmcbControlArea, pause_filter_thresh), 0x003c);
        assert_eq!(offset_of!(VmcbControlArea, pause_filter_count), 0x003e);
        assert_eq!(offset_of!(VmcbControlArea, iopm_base_pa), 0x0040);
        assert_eq!(offset_of!(VmcbControlArea, msrpm_base_pa), 0x0048);
        assert_eq!(offset_of!(VmcbControlArea, tsc_offset), 0x0050);
        assert_eq!(offset_of!(VmcbControlArea, guest_asid), 0x0058);
        assert_eq!(offset_of!(VmcbControlArea, tlb_control), 0x005c);
        assert_eq!(offset_of!(VmcbControlArea, int_control), 0x0060);
        assert_eq!(offset_of!(VmcbControlArea, int_vector), 0x0064);
        assert_eq!(offset_of!(VmcbControlArea, int_state), 0x0068);
        assert_eq!(offset_of!(VmcbControlArea, exit_code), 0x0070);
        assert_eq!(offset_of!(VmcbControlArea, exit_info_1), 0x0078);
        assert_eq!(offset_of!(VmcbControlArea, exit_info_2), 0x0080);
        assert_eq!(offset_of!(VmcbControlArea, exit_int_info), 0x0088);
        assert_eq!(offset_of!(VmcbControlArea, exit_int_info_err), 0x008c);
        assert_eq!(offset_of!(VmcbControlArea, nested_ctl), 0x0090);
        assert_eq!(offset_of!(VmcbControlArea, avic_vapic_bar), 0x0098);
        assert_eq!(offset_of!(VmcbControlArea, ghcb_gpa), 0x00a0);
        assert_eq!(offset_of!(VmcbControlArea, event_inj), 0x00a8);
        assert_eq!(offset_of!(VmcbControlArea, event_inj_err), 0x00ac);
        assert_eq!(offset_of!(VmcbControlArea, nested_cr3), 0x00b0);
        assert_eq!(offset_of!(VmcbControlArea, virt_ext), 0x00b8);
        assert_eq!(offset_of!(VmcbControlArea, clean_bits), 0x00c0);
        assert_eq!(offset_of!(VmcbControlArea, next_rip), 0x00c8);
        assert_eq!(offset_of!(VmcbControlArea, insn_len), 0x00d0);
        assert_eq!(offset_of!(VmcbControlArea, insn_bytes), 0x00d1);
        assert_eq!(offset_of!(VmcbControlArea, avic_backing_page), 0x00e0);
        assert_eq!(offset_of!(VmcbControlArea, avic_logical_id), 0x00f0);
        assert_eq!(offset_of!(VmcbControlArea, avic_physical_id), 0x00f8);

        assert_eq!(offset_of!(VmcbStateSaveArea, es), 0x0000);
        assert_eq!(offset_of!(VmcbStateSaveArea, cs), 0x0010);
        assert_eq!(offset_of!(VmcbStateSaveArea, ss), 0x0020);
        assert_eq!(offset_of!(VmcbStateSaveArea, ds), 0x0030);
        assert_eq!(offset_of!(VmcbStateSaveArea, fs), 0x0040);
        assert_eq!(offset_of!(VmcbStateSaveArea, gs), 0x0050);
        assert_eq!(offset_of!(VmcbStateSaveArea, gdtr), 0x0060);
        assert_eq!(offset_of!(VmcbStateSaveArea, ldtr), 0x0070);
        assert_eq!(offset_of!(VmcbStateSaveArea, idtr), 0x0080);
        assert_eq!(offset_of!(VmcbStateSaveArea, tr), 0x0090);
        assert_eq!(offset_of!(VmcbStateSaveArea, cpl), 0x00cb);
        assert_eq!(offset_of!(VmcbStateSaveArea, efer), 0x00d0);
        assert_eq!(offset_of!(VmcbStateSaveArea, cr4), 0x0148);
        assert_eq!(offset_of!(VmcbStateSaveArea, cr3), 0x0150);
        assert_eq!(offset_of!(VmcbStateSaveArea, cr0), 0x0158);
        assert_eq!(offset_of!(VmcbStateSaveArea, dr7), 0x0160);
        assert_eq!(offset_of!(VmcbStateSaveArea, dr6), 0x0168);
        assert_eq!(offset_of!(VmcbStateSaveArea, rflags), 0x0170);
        assert_eq!(offset_of!(VmcbStateSaveArea, rip), 0x0178);
        assert_eq!(offset_of!(VmcbStateSaveArea, rsp), 0x01d8);
        assert_eq!(offset_of!(VmcbStateSaveArea, s_cet), 0x01e0);
        assert_eq!(offset_of!(VmcbStateSaveArea, ssp), 0x01e8);
        assert_eq!(offset_of!(VmcbStateSaveArea, isst_addr), 0x01f0);
        assert_eq!(offset_of!(VmcbStateSaveArea, rax), 0x01f8);
        assert_eq!(offset_of!(VmcbStateSaveArea, star), 0x0200);
        assert_eq!(offset_of!(VmcbStateSaveArea, lstar), 0x0208);
        assert_eq!(offset_of!(VmcbStateSaveArea, cstar), 0x0210);
        assert_eq!(offset_of!(VmcbStateSaveArea, sfmask), 0x0218);
        assert_eq!(offset_of!(VmcbStateSaveArea, kernel_gs_base), 0x0220);
        assert_eq!(offset_of!(VmcbStateSaveArea, sysenter_cs), 0x0228);
        assert_eq!(offset_of!(VmcbStateSaveArea, sysenter_esp), 0x0230);
        assert_eq!(offset_of!(VmcbStateSaveArea, sysenter_eip), 0x0238);
        assert_eq!(offset_of!(VmcbStateSaveArea, cr2), 0x0240);
        assert_eq!(offset_of!(VmcbStateSaveArea, g_pat), 0x0268);
        assert_eq!(offset_of!(VmcbStateSaveArea, dbgctl), 0x0270);
        assert_eq!(offset_of!(VmcbStateSaveArea, br_from), 0x0278);
        assert_eq!(offset_of!(VmcbStateSaveArea, br_to), 0x0280);
        assert_eq!(offset_of!(VmcbStateSaveArea, last_excp_from), 0x0288);
        assert_eq!(offset_of!(VmcbStateSaveArea, last_excp_to), 0x0290);
    }
}
