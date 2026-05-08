use ax_errno::AxResult;
use axaddrspace::{GuestPhysAddr, MappingFlags};
use axvcpu::AxVCpuExitReason;

use crate::{
    context_frame::LoongArchContextFrame,
    registers::{GCSR_BADI, GCSR_BADV, GCSR_ESTAT, gcsr_read},
};

const ECODE_HVC: usize = 0x17;
const ECODE_PIL: usize = 0x1;
const ECODE_PIS: usize = 0x2;
const ECODE_PIF: usize = 0x3;
const ECODE_PME: usize = 0x4;
const ECODE_PPI: usize = 0x5;
const ECODE_TLBR: usize = 0x8;
const ECODE_RSE: usize = 0x10;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    Synchronous = 0,
    Irq         = 1,
}

impl TryFrom<u8> for TrapKind {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Synchronous),
            1 => Ok(Self::Irq),
            _ => Err(()),
        }
    }
}

fn get_exception_code() -> usize {
    let estat = unsafe { gcsr_read::<GCSR_ESTAT>() };
    (estat >> 16) & 0x3f
}

fn get_exception_subcode() -> usize {
    let estat = unsafe { gcsr_read::<GCSR_ESTAT>() };
    (estat >> 22) & 0x1ff
}

fn get_badv() -> usize {
    unsafe { gcsr_read::<GCSR_BADV>() }
}

fn get_badi() -> usize {
    unsafe { gcsr_read::<GCSR_BADI>() }
}

pub fn handle_exception_sync(ctx: &mut LoongArchContextFrame) -> AxResult<AxVCpuExitReason> {
    let ecode = get_exception_code();
    let esubcode = get_exception_subcode();

    log::trace!(
        "LoongArch handle_exception_sync: ecode={:#x}, esubcode={:#x}, sepc={:#x}",
        ecode,
        esubcode,
        ctx.sepc
    );

    match ecode {
        ECODE_HVC => {
            let nr = ctx.get_a0() as u64;
            let args = [
                ctx.get_a1() as u64,
                ctx.get_a2() as u64,
                ctx.get_a3() as u64,
                ctx.get_a4() as u64,
                ctx.get_a5() as u64,
                ctx.get_a6() as u64,
            ];
            ctx.sepc += 4;
            Ok(AxVCpuExitReason::Hypercall { nr, args })
        }
        ECODE_PIL | ECODE_PIS | ECODE_PIF | ECODE_PME | ECODE_PPI => {
            let badv = get_badv();
            let mut access_flags = MappingFlags::empty();
            if matches!(ecode, ECODE_PIS | ECODE_PME) {
                access_flags |= MappingFlags::WRITE;
            } else if ecode == ECODE_PIF {
                access_flags |= MappingFlags::EXECUTE;
            } else {
                access_flags |= MappingFlags::READ;
            }
            Ok(AxVCpuExitReason::NestedPageFault {
                addr: GuestPhysAddr::from(badv),
                access_flags,
            })
        }
        ECODE_TLBR => Ok(AxVCpuExitReason::NestedPageFault {
            addr: GuestPhysAddr::from(get_badv()),
            access_flags: MappingFlags::READ,
        }),
        ECODE_RSE => Ok(AxVCpuExitReason::Halt),
        _ => panic!(
            "Unhandled synchronous exception: ecode={:#x}, esubcode={:#x}, sepc={:#x}, \
             badv={:#x}, badi={:#x}",
            ecode,
            esubcode,
            ctx.sepc,
            get_badv(),
            get_badi()
        ),
    }
}

pub fn handle_exception_irq(_ctx: &mut LoongArchContextFrame) -> AxResult<AxVCpuExitReason> {
    Ok(AxVCpuExitReason::ExternalInterrupt { vector: 0 })
}

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(include_str!("exception.S"));

#[cfg(target_arch = "loongarch64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
unsafe extern "C" fn vmexit_trampoline() -> ! {
    core::arch::naked_asm!(
        "addi.d $t0, $sp, 288",
        "ld.d $t1, $t0, 0",
        "move $sp, $t1",
        "ld.d $ra, $sp, 0",
        "ld.d $s0, $sp, 8",
        "ld.d $s1, $sp, 16",
        "ld.d $s2, $sp, 24",
        "ld.d $s3, $sp, 32",
        "ld.d $s4, $sp, 40",
        "ld.d $s5, $sp, 48",
        "ld.d $s6, $sp, 56",
        "ld.d $s7, $sp, 64",
        "ld.d $s8, $sp, 72",
        "ld.d $fp, $sp, 80",
        "ld.d $tp, $sp, 88",
        "ld.d $r21, $sp, 96",
        "addi.d $sp, $sp, 14 * 8",
        "jr $ra",
    )
}
