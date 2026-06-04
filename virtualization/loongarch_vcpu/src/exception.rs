use alloc::boxed::Box;
use core::time::Duration;

use ax_errno::AxResult;
use axvcpu::{AxVCpuExitReason, GuestPhysAddr, MappingFlags, VCpuId, VMId};

use crate::{context_frame::LoongArchContextFrame, host};

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
const CSR_TICLR_TI: usize = 0x1;
const CSR_TCFG_EN: usize = 1 << 0;
const CSR_TCFG_PERIODIC: usize = 1 << 1;
const CSR_TCFG_INITVAL_MASK: usize = !0x3;
const CSR_CRMD_PLV_MASK: usize = 0b11;
const CSR_CRMD_IE: usize = 1 << 2;
const CSR_CRMD_DA: usize = 1 << 3;
const CSR_CRMD_PG: usize = 1 << 4;
const CSR_ESTAT_EXC_MASK: usize = (0x3f << 16) | (0x1ff << 22);
const CSR_ECFG_VS_SHIFT: usize = 16;
const CSR_ECFG_VS_MASK: usize = 0b111 << CSR_ECFG_VS_SHIFT;
const CSR_TLBRERA_ISTLBR: usize = 1;
const CSR_TLBREHI_PS_MASK: usize = 0x3f;
const CSR_TLBREHI_VPPN_MASK: usize = 0x0000_ffff_ffff_e000;
const DEFAULT_TLB_PAGE_SHIFT: usize = 12;
const GUEST_RAM_START: usize = 0x0008_0000;
const QEMU_VIRT_MMIO_START: usize = 0x1000_0000;
const QEMU_VIRT_MMIO_END: usize = 0x8000_0000;
const GUEST_RAM_END: usize = QEMU_VIRT_MMIO_START;

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

fn is_host_tlb_refill(ctx: &LoongArchContextFrame) -> bool {
    ctx.host_tlbrera & 0x1 != 0
}

fn get_guest_pc(ctx: &LoongArchContextFrame) -> usize {
    if is_host_tlb_refill(ctx) {
        ctx.host_tlbrera & !0x1
    } else if ctx.host_era != 0 {
        ctx.host_era
    } else {
        ctx.gcsr_era
    }
}

fn direct_map_guest_addr_to_gpa(addr: usize) -> usize {
    if matches!(addr >> 48, 0x8000 | 0x9000 | 0xa000) {
        addr & 0x0000_ffff_ffff_ffff
    } else if matches!(addr >> 44, 0x8..=0xa) {
        addr & 0x0000_0fff_ffff_ffff
    } else {
        addr
    }
}

fn get_refill_access_flags(ctx: &LoongArchContextFrame) -> MappingFlags {
    let badv = direct_map_guest_addr_to_gpa(get_badv(ctx));
    let pc_gpa = direct_map_guest_addr_to_gpa(get_guest_pc(ctx));
    if badv == pc_gpa {
        MappingFlags::EXECUTE
    } else {
        MappingFlags::READ | MappingFlags::WRITE
    }
}

fn guest_paging_enabled(ctx: &LoongArchContextFrame) -> bool {
    ctx.gcsr_crmd & CSR_CRMD_PG != 0
}

fn is_guest_direct_mapped_va(addr: usize) -> bool {
    matches!(addr >> 48, 0x8000 | 0x9000 | 0xa000) || matches!(addr >> 44, 0x8..=0xa)
}

fn is_known_guest_physical_addr(addr: usize) -> bool {
    (GUEST_RAM_START..GUEST_RAM_END).contains(&addr)
        || (QEMU_VIRT_MMIO_START..QEMU_VIRT_MMIO_END).contains(&addr)
}

fn should_inject_guest_virtual_fault(
    ctx: &LoongArchContextFrame,
    badv: usize,
    from_tlb_refill: bool,
) -> bool {
    let is_direct = is_guest_direct_mapped_va(badv);
    let known_physical = is_known_guest_physical_addr(badv);

    if from_tlb_refill {
        if ctx.gcsr_tlbrentry == 0 || is_direct {
            false
        } else {
            !known_physical
        }
    } else if !guest_paging_enabled(ctx) || is_direct || ctx.gcsr_eentry == 0 {
        false
    } else {
        !known_physical
    }
}

fn guest_exception_vector_size(ctx: &LoongArchContextFrame) -> usize {
    let vs = (ctx.gcsr_ectl & CSR_ECFG_VS_MASK) >> CSR_ECFG_VS_SHIFT;
    if vs == 0 { 0 } else { (1 << vs) * 4 }
}

fn guest_pgd(ctx: &LoongArchContextFrame) -> usize {
    let badv = if ctx.gcsr_tlbrera & CSR_TLBRERA_ISTLBR != 0 {
        ctx.gcsr_tlbrbadv
    } else {
        ctx.gcsr_badv
    };

    if badv >> (usize::BITS - 1) != 0 {
        ctx.gcsr_pgdh
    } else {
        ctx.gcsr_pgdl
    }
}

fn inject_guest_regular_exception(
    ctx: &mut LoongArchContextFrame,
    ecode: usize,
    esubcode: usize,
    badv: usize,
) {
    let pc = get_guest_pc(ctx);
    ctx.gcsr_badv = badv;
    ctx.gcsr_badi = get_badi(ctx);
    ctx.gcsr_tlbehi = badv & !0x1fff;
    ctx.gcsr_estat = (ctx.gcsr_estat & !CSR_ESTAT_EXC_MASK)
        | ((ecode & 0x3f) << 16)
        | ((esubcode & 0x1ff) << 22);
    ctx.gcsr_prmd = (ctx.gcsr_prmd & !0b111) | (ctx.gcsr_crmd & (CSR_CRMD_PLV_MASK | CSR_CRMD_IE));
    ctx.gcsr_era = pc;
    ctx.gcsr_crmd &= !(CSR_CRMD_PLV_MASK | CSR_CRMD_IE);
    ctx.sepc = ctx.gcsr_eentry + ecode * guest_exception_vector_size(ctx);
}

fn inject_guest_interrupt(ctx: &mut LoongArchContextFrame, vector: usize) {
    let pc = get_guest_pc(ctx);
    ctx.gcsr_prmd = (ctx.gcsr_prmd & !0b111) | (ctx.gcsr_crmd & (CSR_CRMD_PLV_MASK | CSR_CRMD_IE));
    ctx.gcsr_era = pc;
    ctx.gcsr_crmd &= !(CSR_CRMD_PLV_MASK | CSR_CRMD_IE);
    ctx.sepc = ctx.gcsr_eentry + (64 + vector) * guest_exception_vector_size(ctx);
}

fn inject_guest_tlb_refill(ctx: &mut LoongArchContextFrame, badv: usize) {
    let pc = get_guest_pc(ctx);
    let page_shift = match ctx.gcsr_stlbps & CSR_TLBREHI_PS_MASK {
        0 => DEFAULT_TLB_PAGE_SHIFT,
        shift => shift,
    };
    let pair_mask = (1usize << (page_shift + 1)) - 1;
    let vppn = (badv & !pair_mask) & CSR_TLBREHI_VPPN_MASK;

    ctx.gcsr_tlbrbadv = badv;
    ctx.gcsr_tlbrehi =
        (ctx.gcsr_tlbrehi & !(CSR_TLBREHI_VPPN_MASK | CSR_TLBREHI_PS_MASK)) | vppn | page_shift;
    ctx.gcsr_tlbrera = (pc & !0x3) | CSR_TLBRERA_ISTLBR;
    ctx.gcsr_tlbrprmd =
        (ctx.gcsr_tlbrprmd & !0b111) | (ctx.gcsr_crmd & (CSR_CRMD_PLV_MASK | CSR_CRMD_IE));
    ctx.gcsr_pgd = guest_pgd(ctx);
    ctx.gcsr_crmd =
        (ctx.gcsr_crmd | CSR_CRMD_DA) & !(CSR_CRMD_PG | CSR_CRMD_PLV_MASK | CSR_CRMD_IE);
    ctx.sepc = ctx.gcsr_tlbrentry;
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

fn ack_host_timer_interrupt() {
    unsafe {
        let value = CSR_TICLR_TI;
        core::arch::asm!("csrwr {}, 0x44", inout(reg) value => _);
    }
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

fn emulate_csrx(
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) -> AxVCpuExitReason {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let csr = extract_field(ins, 10, 14);

    match csr {
        CSR_TCFG | CSR_TVAL | CSR_TICLR => {
            emulate_timer_csr(ctx, rd, rj, csr, vm_id, vcpu_id, guest_timer_token)
        }
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

fn read_guest_timer_csr(ctx: &LoongArchContextFrame, csr: usize) -> usize {
    match csr {
        CSR_TCFG => ctx.gcsr_tcfg,
        CSR_TVAL => ctx.gcsr_tval,
        CSR_TICLR => ctx.gcsr_ticlr,
        _ => 0,
    }
}

fn guest_timer_periodic(ctx: &LoongArchContextFrame) -> bool {
    ctx.gcsr_tcfg & CSR_TCFG_PERIODIC != 0
}

fn guest_timer_init_ticks(ctx: &LoongArchContextFrame) -> u64 {
    (ctx.gcsr_tcfg & CSR_TCFG_INITVAL_MASK) as u64
}

fn cancel_guest_timer(guest_timer_token: &mut Option<usize>) {
    if let Some(token) = guest_timer_token.take() {
        host::cancel_timer(token);
    }
}

fn register_guest_timer(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    cancel_guest_timer(guest_timer_token);

    if ctx.gcsr_tcfg & CSR_TCFG_EN == 0 {
        return;
    }

    let init_ticks = guest_timer_init_ticks(ctx);
    if init_ticks == 0 {
        ctx.gcsr_tval = 0;
        ctx.gcsr_estat |= TIMER_BIT;
        return;
    }

    ctx.gcsr_tval = init_ticks as usize;
    let delay_ns = host::ticks_to_nanos(init_ticks);
    let deadline_ns = host::current_time_nanos().saturating_add(delay_ns);
    let token = host::register_timer(
        Duration::from_nanos(deadline_ns),
        Box::new(move |_| host::inject_interrupt(vm_id, vcpu_id, INT_TIMER)),
    );
    *guest_timer_token = Some(token);
}

fn write_guest_timer_csr(
    ctx: &mut LoongArchContextFrame,
    csr: usize,
    value: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    match csr {
        CSR_TCFG => {
            ctx.gcsr_tcfg = value;
            register_guest_timer(ctx, vm_id, vcpu_id, guest_timer_token);
        }
        CSR_TVAL => {
            ctx.gcsr_tval = value;
        }
        CSR_TICLR => {
            ctx.gcsr_ticlr = value;
            if value & CSR_TICLR_TI != 0 {
                ctx.gcsr_estat &= !TIMER_BIT;
                if guest_timer_periodic(ctx) {
                    register_guest_timer(ctx, vm_id, vcpu_id, guest_timer_token);
                } else {
                    ctx.gcsr_tcfg &= !CSR_TCFG_EN;
                    cancel_guest_timer(guest_timer_token);
                }
            }
        }
        _ => {}
    }
}

fn emulate_timer_csr(
    ctx: &mut LoongArchContextFrame,
    rd: usize,
    rj: usize,
    csr: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    let old_value = read_guest_timer_csr(ctx, csr);
    let mut return_value = old_value;

    if rj != 0 {
        let new_value = if rj == 1 {
            ctx.x[rd]
        } else {
            let mask = ctx.x[rj];
            return_value &= mask;
            (old_value & !mask) | (ctx.x[rd] & mask)
        };
        write_guest_timer_csr(ctx, csr, new_value, vm_id, vcpu_id, guest_timer_token);
        log::debug!(
            "Timer CSR emulation: csr={:#x} <- {:#x} (old={:#x})",
            csr,
            new_value,
            old_value
        );
    } else {
        log::debug!("Timer CSR emulation: csr={:#x} -> {:#x}", csr, old_value);
    }

    ctx.set_gpr(rd, return_value);
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
    let pending_enabled = ctx.gcsr_estat & ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK;
    if ctx.gcsr_eentry != 0
        && ctx.gcsr_crmd & CSR_CRMD_IE != 0
        && let Some(vector) = decode_interrupt_vector(pending_enabled)
    {
        inject_guest_interrupt(ctx, vector);
        return AxVCpuExitReason::Nothing;
    }
    advance_guest_pc(ctx);
    AxVCpuExitReason::Idle
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

fn emulate_gspr(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) -> AxVCpuExitReason {
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
        return emulate_csrx(ctx, ins, vm_id, vcpu_id, guest_timer_token);
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

pub fn handle_exception_sync(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) -> AxResult<AxVCpuExitReason> {
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

    if is_host_tlb_refill(ctx) {
        let badv = get_badv(ctx);
        if should_inject_guest_virtual_fault(ctx, badv, true) {
            inject_guest_tlb_refill(ctx, badv);
            return Ok(AxVCpuExitReason::Nothing);
        }
        ctx.sepc = get_guest_pc(ctx);
        return Ok(AxVCpuExitReason::NestedPageFault {
            addr: GuestPhysAddr::from(direct_map_guest_addr_to_gpa(badv)),
            access_flags: get_refill_access_flags(ctx),
        });
    }

    if ecode == 0 && decode_interrupt_vector(get_guest_interrupt_status(ctx)).is_some() {
        return handle_exception_irq(ctx);
    }

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
        ECODE_GSPR => Ok(emulate_gspr(ctx, vm_id, vcpu_id, guest_timer_token)),
        ECODE_PIL | ECODE_PIS | ECODE_PIF | ECODE_PME | ECODE_PNR | ECODE_PNX | ECODE_PPI => {
            let badv = get_badv(ctx);
            if should_inject_guest_virtual_fault(ctx, badv, false) {
                inject_guest_regular_exception(ctx, ecode, esubcode, badv);
                return Ok(AxVCpuExitReason::Nothing);
            }
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
    // A host timer can be pending alongside a synchronous guest exit. It is a
    // host scheduling event here, not necessarily a guest timer interrupt.
    if get_guest_interrupt_status(ctx) & TIMER_BIT != 0 {
        ack_host_timer_interrupt();
    }
    result
}

pub fn handle_exception_irq(ctx: &mut LoongArchContextFrame) -> AxResult<AxVCpuExitReason> {
    let guest_is = get_guest_interrupt_status(ctx);
    let is = guest_is;

    if let Some(vector) = decode_interrupt_vector(is) {
        log::trace!(
            "LoongArch guest irq exit: vector={}, guest_is={:#x}, sepc={:#x}, gera={:#x}",
            vector,
            guest_is,
            get_guest_pc(ctx),
            ctx.gcsr_era
        );

        // Host timer exits drive the scheduler and Axvisor timer wheel. Do not
        // translate every host tick into a guest timer interrupt: doing so can
        // interrupt Linux before it has initialized its guest exception state.
        if vector == INT_TIMER {
            ack_host_timer_interrupt();
        }

        return Ok(AxVCpuExitReason::ExternalInterrupt {
            vector: vector as u64,
        });
    }

    log::trace!(
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
