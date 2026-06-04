use ax_errno::AxResult;
use axvcpu::{AxVCpuExitReason, GuestPhysAddr, MappingFlags};

use crate::context_frame::LoongArchContextFrame;

const ECODE_HVC: usize = 0x17;
const ECODE_GSPR: usize = 0x16;
const ECODE_PIL: usize = 0x1;
const ECODE_PIS: usize = 0x2;
const ECODE_PIF: usize = 0x3;
const ECODE_PME: usize = 0x4;
const ECODE_PNR: usize = 0x5;
const ECODE_PNX: usize = 0x6;
const ECODE_PPI: usize = 0x7;
const ECODE_ADE: usize = 0x8;
const ESUBCODE_ADEF: usize = 0x0;
const ESUBCODE_ADEM: usize = 0x1;
const ECODE_RSE: usize = 0x10;
const LOCAL_INTERRUPT_MASK: usize = (1 << 13) - 1;
const INT_HWI0: usize = 2;
const INT_HWI7: usize = 9;
const INT_TIMER: usize = 11;
const INT_IPI: usize = 12;
const TIMER_BIT: usize = 1 << INT_TIMER;
const IPI_BIT: usize = 1 << INT_IPI;
const HWI_MASK: usize = ((1 << (INT_HWI7 + 1)) - 1) & !((1 << INT_HWI0) - 1);

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

fn get_exception_code(ctx: &LoongArchContextFrame) -> usize {
    (ctx.host_estat >> 16) & 0x3f
}

fn get_exception_subcode(ctx: &LoongArchContextFrame) -> usize {
    (ctx.host_estat >> 22) & 0x1ff
}

fn get_guest_pc(ctx: &LoongArchContextFrame) -> usize {
    if ctx.host_era != 0 {
        ctx.host_era
    } else {
        ctx.gcsr_era
    }
}

fn get_badv(ctx: &LoongArchContextFrame) -> usize {
    if ctx.host_tlbrera & 0x1 != 0 {
        ctx.host_tlbrbadv
    } else {
        ctx.host_badv
    }
}

fn get_badi(ctx: &LoongArchContextFrame) -> usize {
    ctx.host_badi
}

fn get_guest_interrupt_status(ctx: &LoongArchContextFrame) -> usize {
    ctx.host_estat & LOCAL_INTERRUPT_MASK
}

fn decode_interrupt_vector(is: usize) -> Option<usize> {
    if is & IPI_BIT != 0 {
        return Some(INT_IPI);
    }
    if is & TIMER_BIT != 0 {
        return Some(INT_TIMER);
    }

    let hwi = is & HWI_MASK;
    if hwi != 0 {
        return Some(hwi.trailing_zeros() as usize);
    }

    None
}

fn extract_field(value: usize, offset: usize, width: usize) -> usize {
    (value >> offset) & ((1usize << width) - 1)
}

fn advance_guest_pc(ctx: &mut LoongArchContextFrame) {
    ctx.sepc = get_guest_pc(ctx).wrapping_add(4);
}

fn emulate_cpucfg(ctx: &mut LoongArchContextFrame, ins: usize) -> AxVCpuExitReason {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let cpucfg_idx = ctx.x[rj];
    let value = if cpucfg_idx > 20 {
        0
    } else {
        let result: usize;
        unsafe {
            core::arch::asm!("cpucfg {}, {}", out(reg) result, in(reg) cpucfg_idx);
        }
        result
    };
    ctx.set_gpr(rd, value);
    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

fn emulate_csrx(ctx: &mut LoongArchContextFrame, ins: usize) -> AxVCpuExitReason {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let csr = extract_field(ins, 10, 14);

    match csr {
        CSR_TCFG | CSR_TVAL | CSR_TICLR => emulate_timer_csr(ctx, rd, rj, csr),
        _ => {
            match rj {
                0 => log::info!("LoongArch GSPR csrrd emulation: csr={:#x}", csr),
                _ => log::info!("LoongArch GSPR csrwr/csrxchg emulation: csr={:#x}", csr),
            }
            ctx.set_gpr(rd, 0);
        }
    }

    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

/// Timer CSR numbers (matching GCSR encoding in LoongArch LVZ).
const CSR_TCFG: usize = 0x41;
const CSR_TVAL: usize = 0x42;
const CSR_TICLR: usize = 0x44;

/// Emulate guest timer CSR accesses by passing through to the host hardware
/// timer. In the 1:1 vCPU model the host hardware timer is shared: when the
/// guest writes TCFG/TVAL we program the actual timer so that it fires, and
/// when it fires the IRQ handler injects the interrupt into the guest ESTAT.
fn emulate_timer_csr(ctx: &mut LoongArchContextFrame, rd: usize, rj: usize, csr: usize) {
    if rj == 0 {
        // csrrd – read the current host hardware timer value.
        let value = read_host_timer_csr(csr);
        ctx.set_gpr(rd, value);
        log::debug!("Timer CSR read: csr={:#x} -> {:#x}", csr, value);
    } else {
        // csrwr / csrxchg – write guest value to host hardware timer.
        let old_value = read_host_timer_csr(csr);
        let new_value = ctx.x[rj];
        write_host_timer_csr(csr, new_value);
        ctx.set_gpr(rd, old_value);
        log::debug!(
            "Timer CSR write: csr={:#x} <- {:#x} (old={:#x})",
            csr,
            new_value,
            old_value
        );

        // When the guest clears the timer interrupt via TICLR, also clear the
        // injected timer bit in the guest ESTAT so the guest doesn't re-take
        // the interrupt on the next VM entry.
        if csr == CSR_TICLR && (new_value & 0x1) != 0 {
            ctx.gcsr_estat &= !TIMER_BIT;
        }
    }
}

/// Read a host hardware timer CSR via inline assembly.
fn read_host_timer_csr(csr: usize) -> usize {
    let value: usize;
    unsafe {
        match csr {
            CSR_TCFG => core::arch::asm!("csrrd {}, 0x41", out(reg) value),
            CSR_TVAL => core::arch::asm!("csrrd {}, 0x42", out(reg) value),
            CSR_TICLR => core::arch::asm!("csrrd {}, 0x44", out(reg) value),
            _ => value = 0,
        }
    }
    value
}

/// Write a host hardware timer CSR via inline assembly.
fn write_host_timer_csr(csr: usize, value: usize) {
    unsafe {
        match csr {
            CSR_TCFG => core::arch::asm!("csrwr {}, 0x41", in(reg) value),
            CSR_TVAL => core::arch::asm!("csrwr {}, 0x42", in(reg) value),
            CSR_TICLR => core::arch::asm!("csrwr {}, 0x44", in(reg) value),
            _ => {}
        }
    }
}

fn emulate_cacop(ctx: &mut LoongArchContextFrame, _ins: usize) -> AxVCpuExitReason {
    log::info!(
        "LoongArch GSPR cacop emulation skipped at guest_pc={:#x}",
        get_guest_pc(ctx)
    );
    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

fn emulate_idle(ctx: &mut LoongArchContextFrame, ins: usize) -> AxVCpuExitReason {
    let level = extract_field(ins, 0, 15);
    log::debug!("LoongArch guest idle request: level={:#x}", level);
    advance_guest_pc(ctx);
    // Return Nothing instead of Halt so the guest busy-loops in its idle
    // handler. A Halt exit would permanently block the vCPU task because
    // there is no timer-based wakeup mechanism yet.
    AxVCpuExitReason::Nothing
}

fn emulate_iocsr(ctx: &mut LoongArchContextFrame, ins: usize) -> AxVCpuExitReason {
    let ty = extract_field(ins, 10, 3);
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let rj_value = ctx.x[rj];

    match ty {
        0 => {
            let value: usize;
            unsafe {
                core::arch::asm!("iocsrrd.b {}, {}", out(reg) value, in(reg) rj_value);
            }
            ctx.set_gpr(rd, (value as i8) as isize as usize);
        }
        1 => {
            let value: usize;
            unsafe {
                core::arch::asm!("iocsrrd.h {}, {}", out(reg) value, in(reg) rj_value);
            }
            ctx.set_gpr(rd, (value as i16) as isize as usize);
        }
        2 => {
            let value: usize;
            unsafe {
                core::arch::asm!("iocsrrd.w {}, {}", out(reg) value, in(reg) rj_value);
            }
            ctx.set_gpr(rd, (value as i32) as isize as usize);
        }
        3 => {
            let value: usize;
            unsafe {
                core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) rj_value);
            }
            ctx.set_gpr(rd, value);
        }
        4 => unsafe {
            core::arch::asm!("iocsrwr.b {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
        },
        5 => unsafe {
            core::arch::asm!("iocsrwr.h {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
        },
        6 => unsafe {
            core::arch::asm!("iocsrwr.w {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
        },
        7 => unsafe {
            core::arch::asm!("iocsrwr.d {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
        },
        _ => panic!("invalid LoongArch IOCSR opcode type: {ty}"),
    }

    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

fn emulate_gspr(ctx: &mut LoongArchContextFrame) -> AxVCpuExitReason {
    let ins = get_badi(ctx) as u32 as usize;
    const OPCODE_CPUCFG: usize = 0b0000000000000000011011;
    const OPCODE_CPUCFG_LEN: usize = 22;
    const OPCODE_CACOP: usize = 0b0000011000;
    const OPCODE_CACOP_LEN: usize = 10;
    const OPCODE_IDLE: usize = 0b0_0000_1100_1001_0001;
    const OPCODE_IDLE_LEN: usize = 17;
    const OPCODE_CSRX: usize = 0b00000100;
    const OPCODE_CSRX_LEN: usize = 8;
    const OPCODE_IOCSR: usize = 0b0000011001;
    const OPCODE_IOCSR_LEN: usize = 10;

    let matches = |opcode: usize, len: usize| -> bool {
        let shift = 32 - len;
        ((ins >> shift) & ((1usize << len) - 1)) == opcode
    };

    if matches(OPCODE_CPUCFG, OPCODE_CPUCFG_LEN) {
        return emulate_cpucfg(ctx, ins);
    }
    if matches(OPCODE_CACOP, OPCODE_CACOP_LEN) {
        return emulate_cacop(ctx, ins);
    }
    if matches(OPCODE_IDLE, OPCODE_IDLE_LEN) {
        return emulate_idle(ctx, ins);
    }
    if matches(OPCODE_CSRX, OPCODE_CSRX_LEN) {
        return emulate_csrx(ctx, ins);
    }
    if matches(OPCODE_IOCSR, OPCODE_IOCSR_LEN) {
        return emulate_iocsr(ctx, ins);
    }

    panic!(
        "Unhandled LoongArch GSPR instruction: pc={:#x}, badi={:#x}",
        get_guest_pc(ctx),
        ins
    );
}

pub fn handle_exception_sync(ctx: &mut LoongArchContextFrame) -> AxResult<AxVCpuExitReason> {
    let ecode = get_exception_code(ctx);
    let esubcode = get_exception_subcode(ctx);

    log::debug!(
        "LoongArch guest sync exit: ecode={:#x}, esubcode={:#x}, guest_pc={:#x}, host_era={:#x}, \
         gera={:#x}, badv={:#x}, badi={:#x}, host_estat={:#x}, tlbrera={:#x}",
        ecode,
        esubcode,
        get_guest_pc(ctx),
        ctx.host_era,
        ctx.gcsr_era,
        get_badv(ctx),
        get_badi(ctx),
        ctx.host_estat,
        ctx.host_tlbrera
    );

    let result = match ecode {
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
            advance_guest_pc(ctx);
            Ok(AxVCpuExitReason::Hypercall { nr, args })
        }
        ECODE_GSPR => Ok(emulate_gspr(ctx)),
        ECODE_PIL | ECODE_PIS | ECODE_PIF | ECODE_PME | ECODE_PNR | ECODE_PNX | ECODE_PPI => {
            let badv = get_badv(ctx);
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
        // Per the LoongArch manuals and hvisor, ecode=0x8 is ADE:
        // esubcode=0 => ADEF (instruction fetch address exception),
        // esubcode=1 => ADEM (data access address exception).
        // It is not a TLB refill / nested page fault, so retrying the vCPU
        // would only spin forever on the same synchronous exception.
        ECODE_ADE => panic!(
            "LoongArch guest address exception: kind={}, sepc={:#x}, gera={:#x}, badv={:#x}, \
             badi={:#x}",
            match esubcode {
                ESUBCODE_ADEF => "ADEF",
                ESUBCODE_ADEM => "ADEM",
                _ => "ADE",
            },
            get_guest_pc(ctx),
            ctx.gcsr_era,
            get_badv(ctx),
            get_badi(ctx)
        ),
        ECODE_RSE => Ok(AxVCpuExitReason::Halt),
        _ => panic!(
            "Unhandled synchronous exception: ecode={:#x}, esubcode={:#x}, sepc={:#x}, \
             gera={:#x}, badv={:#x}, badi={:#x}",
            ecode,
            esubcode,
            get_guest_pc(ctx),
            ctx.gcsr_era,
            get_badv(ctx),
            get_badi(ctx)
        ),
    };
    // When a timer interrupt is pending alongside a synchronous exception, the
    // sync exception wins in the hardware priority arbitration and the IRQ exit
    // is never taken. Forward the timer interrupt into the guest by setting the
    // timer bit in the guest ESTAT so that it is observed on the next VM entry.
    if get_guest_interrupt_status(ctx) & TIMER_BIT != 0 {
        ctx.gcsr_estat |= TIMER_BIT;
        // Acknowledge the host timer interrupt so it stops firing.
        unsafe {
            core::arch::asm!("csrwr {}, 0x44", in(reg) 1usize);
        }
    }
    result
}

pub fn handle_exception_irq(ctx: &mut LoongArchContextFrame) -> AxResult<AxVCpuExitReason> {
    let guest_is = get_guest_interrupt_status(ctx);
    let is = guest_is;

    if let Some(vector) = decode_interrupt_vector(is) {
        log::info!(
            "LoongArch guest irq exit: vector={}, guest_is={:#x}, sepc={:#x}, gera={:#x}",
            vector,
            guest_is,
            get_guest_pc(ctx),
            ctx.gcsr_era
        );

        // Inject the timer interrupt into the guest by setting the timer bit
        // in the guest ESTAT. On the next VM entry RESTORE_GUEST_REGS will
        // write this to GCSR ESTAT, so the guest sees the interrupt.
        if vector == INT_TIMER {
            ctx.gcsr_estat |= TIMER_BIT;
        }

        return Ok(AxVCpuExitReason::ExternalInterrupt {
            vector: vector as u64,
        });
    }

    log::warn!(
        "LoongArch guest irq exit with unknown status: guest_is={:#x}, saved_estat={:#x}, \
         sepc={:#x}, gera={:#x}",
        guest_is,
        ctx.host_estat,
        get_guest_pc(ctx),
        ctx.gcsr_era
    );
    Ok(AxVCpuExitReason::Nothing)
}

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(include_str!("exception.S"));

#[cfg(target_arch = "loongarch64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
unsafe extern "C" fn vmexit_trampoline() -> ! {
    core::arch::naked_asm!(
        "addi.d $t0, $sp, {ctx_size}",
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
        ctx_size = const core::mem::size_of::<LoongArchContextFrame>(),
    )
}
