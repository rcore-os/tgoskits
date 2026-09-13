//! AMD SVM exit and interception encodings.

use core::convert::TryFrom;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Decoded AMD SVM hardware exit reason, preserving unknown raw values on decode.
pub enum SvmExitCode {
    /// AMD APM `CR_READ` exit or interception.
    CrRead(u8),
    /// AMD APM `CR_WRITE` exit or interception.
    CrWrite(u8),
    /// AMD APM `DR_READ` exit or interception.
    DrRead(u8),
    /// AMD APM `DR_WRITE` exit or interception.
    DrWrite(u8),
    /// AMD APM `EXCP` exit or interception.
    Excp(u8),
    /// AMD APM `INTR` exit or interception.
    Intr,
    /// AMD APM `NMI` exit or interception.
    Nmi,
    /// AMD APM `SMI` exit or interception.
    Smi,
    /// AMD APM `INIT` exit or interception.
    Init,
    /// AMD APM `VINTR` exit or interception.
    Vintr,
    /// AMD APM `CR0_SEL_WRITE` exit or interception.
    Cr0SelWrite,
    /// AMD APM `IDTR_READ` exit or interception.
    IdtrRead,
    /// AMD APM `GDTR_READ` exit or interception.
    GdtrRead,
    /// AMD APM `LDTR_READ` exit or interception.
    LdtrRead,
    /// AMD APM `TR_READ` exit or interception.
    TrRead,
    /// AMD APM `IDTR_WRITE` exit or interception.
    IdtrWrite,
    /// AMD APM `GDTR_WRITE` exit or interception.
    GdtrWrite,
    /// AMD APM `LDTR_WRITE` exit or interception.
    LdtrWrite,
    /// AMD APM `TR_WRITE` exit or interception.
    TrWrite,
    /// AMD APM `RDTSC` exit or interception.
    Rdtsc,
    /// AMD APM `RDPMC` exit or interception.
    Rdpmc,
    /// AMD APM `PUSHF` exit or interception.
    Pushf,
    /// AMD APM `POPF` exit or interception.
    Popf,
    /// AMD APM `CPUID` exit or interception.
    Cpuid,
    /// AMD APM `RSM` exit or interception.
    Rsm,
    /// AMD APM `IRET` exit or interception.
    Iret,
    /// AMD APM `SWINT` exit or interception.
    Swint,
    /// AMD APM `INVD` exit or interception.
    Invd,
    /// AMD APM `PAUSE` exit or interception.
    Pause,
    /// AMD APM `HLT` exit or interception.
    Hlt,
    /// AMD APM `INVLPG` exit or interception.
    Invlpg,
    /// AMD APM `INVLPGA` exit or interception.
    Invlpga,
    /// AMD APM `IOIO` exit or interception.
    Ioio,
    /// AMD APM `MSR` exit or interception.
    Msr,
    /// AMD APM `TASK_SWITCH` exit or interception.
    TaskSwitch,
    /// AMD APM `FERR_FREEZE` exit or interception.
    FerrFreeze,
    /// AMD APM `SHUTDOWN` exit or interception.
    Shutdown,
    /// AMD APM `VMRUN` exit or interception.
    Vmrun,
    /// AMD APM `VMMCALL` exit or interception.
    Vmmcall,
    /// AMD APM `VMLOAD` exit or interception.
    Vmload,
    /// AMD APM `VMSAVE` exit or interception.
    Vmsave,
    /// AMD APM `STGI` exit or interception.
    Stgi,
    /// AMD APM `CLGI` exit or interception.
    Clgi,
    /// AMD APM `SKINIT` exit or interception.
    Skinit,
    /// AMD APM `RDTSCP` exit or interception.
    Rdtscp,
    /// AMD APM `ICEBP` exit or interception.
    Icebp,
    /// AMD APM `WBINVD` exit or interception.
    Wbinvd,
    /// AMD APM `MONITOR` exit or interception.
    Monitor,
    /// AMD APM `MWAIT` exit or interception.
    Mwait,
    /// AMD APM `MWAIT_CONDITIONAL` exit or interception.
    MwaitConditional,
    /// AMD APM `XSETBV` exit or interception.
    Xsetbv,
    /// AMD APM `RDPRU` exit or interception.
    Rdpru,
    /// AMD APM `EFER_WRITE_TRAP` exit or interception.
    EferWriteTrap,
    /// AMD APM `CR_WRITE_TRAP` exit or interception.
    CrWriteTrap(u8),
    /// AMD APM `INVLPGB` exit or interception.
    Invlpgb,
    /// AMD APM `INVLPGB_ILLEGAL` exit or interception.
    InvlpgbIllegal,
    /// AMD APM `INVPCID` exit or interception.
    Invpcid,
    /// AMD APM `MCOMMIT` exit or interception.
    Mcommit,
    /// AMD APM `TLBSYNC` exit or interception.
    Tlbsync,
    /// AMD APM `NPF` exit or interception.
    Npf,
    /// AMD APM `AVIC_INCOMPLETE_IPI` exit or interception.
    AvicIncompleteIpi,
    /// AMD APM `AVIC_NOACCEL` exit or interception.
    AvicNoaccel,
    /// AMD APM `VMGEXIT` exit or interception.
    Vmgexit,
    /// AMD APM `INVALID` exit or interception.
    Invalid,
    /// AMD APM `BUSY` exit or interception.
    Busy,
}

impl TryFrom<u64> for SvmExitCode {
    type Error = u64;

    fn try_from(val: u64) -> Result<Self, Self::Error> {
        match val as i64 {
            0x00..=0x0f => Ok(Self::CrRead(val as u8)),
            0x10..=0x1f => Ok(Self::CrWrite(val as u8 - 0x10)),
            0x20..=0x2f => Ok(Self::DrRead(val as u8 - 0x20)),
            0x30..=0x3f => Ok(Self::DrWrite(val as u8 - 0x30)),
            0x40..=0x5f => Ok(Self::Excp(val as u8 - 0x40)),
            0x60 => Ok(Self::Intr),
            0x61 => Ok(Self::Nmi),
            0x62 => Ok(Self::Smi),
            0x63 => Ok(Self::Init),
            0x64 => Ok(Self::Vintr),
            0x65 => Ok(Self::Cr0SelWrite),
            0x66 => Ok(Self::IdtrRead),
            0x67 => Ok(Self::GdtrRead),
            0x68 => Ok(Self::LdtrRead),
            0x69 => Ok(Self::TrRead),
            0x6a => Ok(Self::IdtrWrite),
            0x6b => Ok(Self::GdtrWrite),
            0x6c => Ok(Self::LdtrWrite),
            0x6d => Ok(Self::TrWrite),
            0x6e => Ok(Self::Rdtsc),
            0x6f => Ok(Self::Rdpmc),
            0x70 => Ok(Self::Pushf),
            0x71 => Ok(Self::Popf),
            0x72 => Ok(Self::Cpuid),
            0x73 => Ok(Self::Rsm),
            0x74 => Ok(Self::Iret),
            0x75 => Ok(Self::Swint),
            0x76 => Ok(Self::Invd),
            0x77 => Ok(Self::Pause),
            0x78 => Ok(Self::Hlt),
            0x79 => Ok(Self::Invlpg),
            0x7a => Ok(Self::Invlpga),
            0x7b => Ok(Self::Ioio),
            0x7c => Ok(Self::Msr),
            0x7d => Ok(Self::TaskSwitch),
            0x7e => Ok(Self::FerrFreeze),
            0x7f => Ok(Self::Shutdown),
            0x80 => Ok(Self::Vmrun),
            0x81 => Ok(Self::Vmmcall),
            0x82 => Ok(Self::Vmload),
            0x83 => Ok(Self::Vmsave),
            0x84 => Ok(Self::Stgi),
            0x85 => Ok(Self::Clgi),
            0x86 => Ok(Self::Skinit),
            0x87 => Ok(Self::Rdtscp),
            0x88 => Ok(Self::Icebp),
            0x89 => Ok(Self::Wbinvd),
            0x8a => Ok(Self::Monitor),
            0x8b => Ok(Self::Mwait),
            0x8c => Ok(Self::MwaitConditional),
            0x8d => Ok(Self::Xsetbv),
            0x8e => Ok(Self::Rdpru),
            0x8f => Ok(Self::EferWriteTrap),
            0x90..=0x9f => Ok(Self::CrWriteTrap(val as u8 - 0x90)),
            0xa0 => Ok(Self::Invlpgb),
            0xa1 => Ok(Self::InvlpgbIllegal),
            0xa2 => Ok(Self::Invpcid),
            0xa3 => Ok(Self::Mcommit),
            0xa4 => Ok(Self::Tlbsync),
            0x400 => Ok(Self::Npf),
            0x401 => Ok(Self::AvicIncompleteIpi),
            0x402 => Ok(Self::AvicNoaccel),
            0x403 => Ok(Self::Vmgexit),
            -1 => Ok(Self::Invalid),
            -2 => Ok(Self::Busy),
            _ => Err(val),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An instruction or event interception supported by the VMCB control image.
pub enum SvmIntercept {
    /// AMD APM `INTR` exit or interception.
    Intr,
    /// AMD APM `NMI` exit or interception.
    Nmi,
    /// AMD APM `SMI` exit or interception.
    Smi,
    /// AMD APM `INIT` exit or interception.
    Init,
    /// AMD APM `VINTR` exit or interception.
    Vintr,
    /// AMD APM `CR0_SEL_WRITE` exit or interception.
    Cr0SelWrite,
    /// AMD APM `IDTR_READ` exit or interception.
    IdtrRead,
    /// AMD APM `GDTR_READ` exit or interception.
    GdtrRead,
    /// AMD APM `LDTR_READ` exit or interception.
    LdtrRead,
    /// AMD APM `TR_READ` exit or interception.
    TrRead,
    /// AMD APM `IDTR_WRITE` exit or interception.
    IdtrWrite,
    /// AMD APM `GDTR_WRITE` exit or interception.
    GdtrWrite,
    /// AMD APM `LDTR_WRITE` exit or interception.
    LdtrWrite,
    /// AMD APM `TR_WRITE` exit or interception.
    TrWrite,
    /// AMD APM `RDTSC` exit or interception.
    Rdtsc,
    /// AMD APM `RDPMC` exit or interception.
    Rdpmc,
    /// AMD APM `PUSHF` exit or interception.
    Pushf,
    /// AMD APM `POPF` exit or interception.
    Popf,
    /// AMD APM `CPUID` exit or interception.
    Cpuid,
    /// AMD APM `RSM` exit or interception.
    Rsm,
    /// AMD APM `IRET` exit or interception.
    Iret,
    /// AMD APM `SWINT` exit or interception.
    Swint,
    /// AMD APM `INVD` exit or interception.
    Invd,
    /// AMD APM `PAUSE` exit or interception.
    Pause,
    /// AMD APM `HLT` exit or interception.
    Hlt,
    /// AMD APM `INVLPG` exit or interception.
    Invlpg,
    /// AMD APM `INVLPGA` exit or interception.
    Invlpga,
    /// AMD APM `IOIO_PROT` exit or interception.
    IoioProt,
    /// AMD APM `MSR_PROT` exit or interception.
    MsrProt,
    /// AMD APM `TASK_SWITCH` exit or interception.
    TaskSwitch,
    /// AMD APM `FERR_FREEZE` exit or interception.
    FerrFreeze,
    /// AMD APM `SHUTDOWN` exit or interception.
    Shutdown,
    /// AMD APM `VMRUN` exit or interception.
    Vmrun,
    /// AMD APM `VMMCALL` exit or interception.
    Vmmcall,
    /// AMD APM `VMLOAD` exit or interception.
    Vmload,
    /// AMD APM `VMSAVE` exit or interception.
    Vmsave,
    /// AMD APM `STGI` exit or interception.
    Stgi,
    /// AMD APM `CLGI` exit or interception.
    Clgi,
    /// AMD APM `SKINIT` exit or interception.
    Skinit,
    /// AMD APM `RDTSCP` exit or interception.
    Rdtscp,
    /// AMD APM `ICEBP` exit or interception.
    Icebp,
    /// AMD APM `WBINVD` exit or interception.
    Wbinvd,
    /// AMD APM `MONITOR` exit or interception.
    Monitor,
    /// AMD APM `MWAIT` exit or interception.
    Mwait,
    /// AMD APM `MWAIT_CONDITIONAL` exit or interception.
    MwaitConditional,
    /// AMD APM `XSETBV` exit or interception.
    Xsetbv,
    /// AMD APM `RDPRU` exit or interception.
    Rdpru,
    /// AMD APM `EFER_WRITE_TRAP` exit or interception.
    EferWriteTrap,
    /// AMD APM `INVLPGB` exit or interception.
    Invlpgb,
    /// AMD APM `INVLPGB_ILLEGAL` exit or interception.
    InvlpgbIllegal,
    /// AMD APM `INVPCID` exit or interception.
    Invpcid,
    /// AMD APM `MCOMMIT` exit or interception.
    Mcommit,
    /// AMD APM `TLBSYNC` exit or interception.
    Tlbsync,
}
