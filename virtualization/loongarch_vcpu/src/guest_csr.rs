use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use crate::{
    context_frame::LoongArchContextFrame,
    host::LoongArchHostOps,
    host_cpu::host_cpucfg,
    registers::{
        crmd_exception_clear_mask, crmd_interrupt_enable_value, crmd_saved_state,
        crmd_with_direct_addressing, ecfg_vs_value_from, estat_exception_mask,
        estat_exception_value, guest_tcfg_enable_mask, guest_tcfg_enabled, guest_tcfg_initval,
        guest_tcfg_periodic, guest_ticlr_has_timer_interrupt_clear, prmd_saved_state_mask,
    },
    trap::{
        INT_TIMER, LOCAL_INTERRUPT_MASK, TIMER_BIT, advance_guest_pc, decode_interrupt_vector,
        extract_field, get_badi, get_guest_pc,
    },
    types::{LoongArchVcpuId, LoongArchVmExit, LoongArchVmId},
};

const CPUCFG2_CRYPTO: usize = 1 << 9;
const CSR_CRMD: usize = 0x0;
const CSR_PRMD: usize = 0x1;
const CSR_EUEN: usize = 0x2;
const CSR_MISC: usize = 0x3;
const CSR_ECFG: usize = 0x4;
const CSR_ESTAT: usize = 0x5;
const CSR_ERA: usize = 0x6;
const CSR_BADV: usize = 0x7;
const CSR_BADI: usize = 0x8;
const CSR_EENTRY: usize = 0xc;
const CSR_TLBIDX: usize = 0x10;
const CSR_TLBEHI: usize = 0x11;
const CSR_TLBELO0: usize = 0x12;
const CSR_TLBELO1: usize = 0x13;
const CSR_ASID: usize = 0x18;
const CSR_PGDL: usize = 0x19;
const CSR_PGDH: usize = 0x1a;
const CSR_PGD: usize = 0x1b;
const CSR_PWCL: usize = 0x1c;
const CSR_PWCH: usize = 0x1d;
const CSR_STLBPS: usize = 0x1e;
const CSR_RAVCFG: usize = 0x1f;
const CSR_CPUID: usize = 0x20;
const CSR_PRCFG1: usize = 0x21;
const CSR_PRCFG2: usize = 0x22;
const CSR_PRCFG3: usize = 0x23;
const CSR_TID: usize = 0x40;
const CSR_LLBCTL: usize = 0x60;
const CSR_TLBRENTRY: usize = 0x88;
const CSR_TLBRBADV: usize = 0x89;
const CSR_TLBRERA: usize = 0x8a;
const CSR_TLBRSAVE: usize = 0x8b;
const CSR_TLBRELO0: usize = 0x8c;
const CSR_TLBRELO1: usize = 0x8d;
const CSR_TLBREHI: usize = 0x8e;
const CSR_TLBRPRMD: usize = 0x8f;
const CSR_DMW0: usize = 0x180;
const CSR_DMW1: usize = 0x181;
const CSR_DMW2: usize = 0x182;
const CSR_DMW3: usize = 0x183;
const CSR_TLBRERA_ISTLBR: usize = 1;
const CSR_TLBREHI_PS_MASK: usize = 0x3f;
const CSR_TLBREHI_VPPN_MASK: usize = 0x0000_ffff_ffff_e000;
const DEFAULT_TLB_PAGE_SHIFT: usize = 12;

static IDLE_EXIT_LOGS: AtomicUsize = AtomicUsize::new(0);
static GUEST_TIMER_LOGS: AtomicUsize = AtomicUsize::new(0);

fn guest_exception_vector_size(ctx: &LoongArchContextFrame) -> usize {
    let vs = ecfg_vs_value_from(ctx.gcsr_ectl);
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

pub(crate) fn inject_guest_regular_exception(
    ctx: &mut LoongArchContextFrame,
    ecode: usize,
    esubcode: usize,
    badv: usize,
) {
    let pc = get_guest_pc(ctx);
    ctx.gcsr_badv = badv;
    ctx.gcsr_badi = get_badi(ctx);
    ctx.gcsr_tlbehi = badv & !0x1fff;
    ctx.gcsr_estat =
        (ctx.gcsr_estat & !estat_exception_mask()) | estat_exception_value(ecode, esubcode);
    ctx.gcsr_prmd = (ctx.gcsr_prmd & !prmd_saved_state_mask()) | crmd_saved_state(ctx.gcsr_crmd);
    ctx.gcsr_era = pc;
    ctx.gcsr_crmd &= !crmd_exception_clear_mask();
    ctx.sepc = ctx.gcsr_eentry + ecode * guest_exception_vector_size(ctx);
}

pub(crate) fn inject_guest_interrupt_at(ctx: &mut LoongArchContextFrame, vector: usize, pc: usize) {
    ctx.gcsr_prmd = (ctx.gcsr_prmd & !prmd_saved_state_mask()) | crmd_saved_state(ctx.gcsr_crmd);
    ctx.gcsr_era = pc;
    ctx.gcsr_crmd &= !crmd_exception_clear_mask();
    ctx.sepc = ctx.gcsr_eentry + (64 + vector) * guest_exception_vector_size(ctx);
}

pub(crate) fn inject_guest_tlb_refill(ctx: &mut LoongArchContextFrame, badv: usize) {
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
        (ctx.gcsr_tlbrprmd & !prmd_saved_state_mask()) | crmd_saved_state(ctx.gcsr_crmd);
    ctx.gcsr_pgd = guest_pgd(ctx);
    ctx.gcsr_crmd = crmd_with_direct_addressing(ctx.gcsr_crmd);
    ctx.sepc = ctx.gcsr_tlbrentry;
}

pub(crate) fn emulate_cpucfg(ctx: &mut LoongArchContextFrame, ins: usize) -> LoongArchVmExit {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let cpucfg_idx = ctx.x[rj];
    let mut value = if cpucfg_idx > 20 {
        0
    } else {
        host_cpucfg(cpucfg_idx)
    };
    if cpucfg_idx == 2 {
        value &= !CPUCFG2_CRYPTO;
    }
    ctx.set_gpr(rd, value);
    advance_guest_pc(ctx);
    LoongArchVmExit::Nothing
}

pub(crate) fn emulate_csrx<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) -> LoongArchVmExit {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let csr = extract_field(ins, 10, 14);

    emulate_guest_csr::<H>(ctx, rd, rj, csr, vm_id, vcpu_id, guest_timer_token);
    advance_guest_pc(ctx);
    LoongArchVmExit::Nothing
}

/// Timer CSR numbers (matching GCSR encoding in LoongArch LVZ).
const CSR_TCFG: usize = 0x41;
const CSR_TVAL: usize = 0x42;
const CSR_TICLR: usize = 0x44;

fn read_guest_csr(ctx: &LoongArchContextFrame, csr: usize) -> usize {
    match csr {
        CSR_CRMD => ctx.gcsr_crmd,
        CSR_PRMD => ctx.gcsr_prmd,
        CSR_EUEN => ctx.gcsr_euen,
        CSR_MISC => ctx.gcsr_misc,
        CSR_ECFG => ctx.gcsr_ectl,
        CSR_ESTAT => ctx.gcsr_estat,
        CSR_ERA => ctx.gcsr_era,
        CSR_BADV => ctx.gcsr_badv,
        CSR_BADI => ctx.gcsr_badi,
        CSR_EENTRY => ctx.gcsr_eentry,
        CSR_TLBIDX => ctx.gcsr_tlbidx,
        CSR_TLBEHI => ctx.gcsr_tlbehi,
        CSR_TLBELO0 => ctx.gcsr_tlbelo0,
        CSR_TLBELO1 => ctx.gcsr_tlbelo1,
        CSR_ASID => ctx.gcsr_asid,
        CSR_PGDL => ctx.gcsr_pgdl,
        CSR_PGDH => ctx.gcsr_pgdh,
        CSR_PGD => guest_pgd(ctx),
        CSR_PWCL => ctx.gcsr_pwcl,
        CSR_PWCH => ctx.gcsr_pwch,
        CSR_STLBPS => ctx.gcsr_stlbps,
        CSR_RAVCFG => ctx.gcsr_ravcfg,
        CSR_CPUID => ctx.gcsr_cpuid,
        CSR_PRCFG1 => ctx.gcsr_prcfg1,
        CSR_PRCFG2 => ctx.gcsr_prcfg2,
        CSR_PRCFG3 => ctx.gcsr_prcfg3,
        0x30 => ctx.gcsr_save0,
        0x31 => ctx.gcsr_save1,
        0x32 => ctx.gcsr_save2,
        0x33 => ctx.gcsr_save3,
        0x34 => ctx.gcsr_save4,
        0x35 => ctx.gcsr_save5,
        0x36 => ctx.gcsr_save6,
        0x37 => ctx.gcsr_save7,
        0x38 => ctx.gcsr_save8,
        0x39 => ctx.gcsr_save9,
        0x3a => ctx.gcsr_save10,
        0x3b => ctx.gcsr_save11,
        0x3c => ctx.gcsr_save12,
        0x3d => ctx.gcsr_save13,
        0x3e => ctx.gcsr_save14,
        0x3f => ctx.gcsr_save15,
        CSR_TID => ctx.gcsr_tid,
        CSR_TCFG => ctx.gcsr_tcfg,
        CSR_TVAL => ctx.gcsr_tval,
        CSR_TICLR => ctx.gcsr_ticlr,
        CSR_LLBCTL => ctx.gcsr_llbctl,
        CSR_TLBRENTRY => ctx.gcsr_tlbrentry,
        CSR_TLBRBADV => ctx.gcsr_tlbrbadv,
        CSR_TLBRERA => ctx.gcsr_tlbrera,
        CSR_TLBRSAVE => ctx.gcsr_tlbrsave,
        CSR_TLBRELO0 => ctx.gcsr_tlbrelo0,
        CSR_TLBRELO1 => ctx.gcsr_tlbrelo1,
        CSR_TLBREHI => ctx.gcsr_tlbrehi,
        CSR_TLBRPRMD => ctx.gcsr_tlbrprmd,
        CSR_DMW0 => ctx.gcsr_dmw0,
        CSR_DMW1 => ctx.gcsr_dmw1,
        CSR_DMW2 => ctx.gcsr_dmw2,
        CSR_DMW3 => ctx.gcsr_dmw3,
        _ => 0,
    }
}

fn write_guest_csr<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    csr: usize,
    value: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) {
    match csr {
        CSR_CRMD => ctx.gcsr_crmd = value,
        CSR_PRMD => ctx.gcsr_prmd = value,
        CSR_EUEN => ctx.gcsr_euen = value,
        CSR_MISC => ctx.gcsr_misc = value,
        CSR_ECFG => ctx.gcsr_ectl = value,
        CSR_ESTAT => {
            ctx.gcsr_estat = (ctx.gcsr_estat & !0x3) | (value & 0x3);
        }
        CSR_ERA => ctx.gcsr_era = value,
        CSR_BADV => ctx.gcsr_badv = value,
        CSR_BADI => ctx.gcsr_badi = value,
        CSR_EENTRY => ctx.gcsr_eentry = value,
        CSR_TLBIDX => ctx.gcsr_tlbidx = value,
        CSR_TLBEHI => ctx.gcsr_tlbehi = value,
        CSR_TLBELO0 => ctx.gcsr_tlbelo0 = value,
        CSR_TLBELO1 => ctx.gcsr_tlbelo1 = value,
        CSR_ASID => ctx.gcsr_asid = value,
        CSR_PGDL => ctx.gcsr_pgdl = value,
        CSR_PGDH => ctx.gcsr_pgdh = value,
        CSR_PGD => ctx.gcsr_pgd = value,
        CSR_PWCL => ctx.gcsr_pwcl = value,
        CSR_PWCH => ctx.gcsr_pwch = value,
        CSR_STLBPS => ctx.gcsr_stlbps = value,
        CSR_RAVCFG => ctx.gcsr_ravcfg = value,
        CSR_CPUID => ctx.gcsr_cpuid = value,
        CSR_PRCFG1 => ctx.gcsr_prcfg1 = value,
        CSR_PRCFG2 => ctx.gcsr_prcfg2 = value,
        CSR_PRCFG3 => ctx.gcsr_prcfg3 = value,
        0x30 => ctx.gcsr_save0 = value,
        0x31 => ctx.gcsr_save1 = value,
        0x32 => ctx.gcsr_save2 = value,
        0x33 => ctx.gcsr_save3 = value,
        0x34 => ctx.gcsr_save4 = value,
        0x35 => ctx.gcsr_save5 = value,
        0x36 => ctx.gcsr_save6 = value,
        0x37 => ctx.gcsr_save7 = value,
        0x38 => ctx.gcsr_save8 = value,
        0x39 => ctx.gcsr_save9 = value,
        0x3a => ctx.gcsr_save10 = value,
        0x3b => ctx.gcsr_save11 = value,
        0x3c => ctx.gcsr_save12 = value,
        0x3d => ctx.gcsr_save13 = value,
        0x3e => ctx.gcsr_save14 = value,
        0x3f => ctx.gcsr_save15 = value,
        CSR_TID => ctx.gcsr_tid = value,
        CSR_TCFG | CSR_TVAL | CSR_TICLR => {
            write_guest_timer_csr::<H>(ctx, csr, value, vm_id, vcpu_id, guest_timer_token)
        }
        CSR_LLBCTL => ctx.gcsr_llbctl = value,
        CSR_TLBRENTRY => ctx.gcsr_tlbrentry = value,
        CSR_TLBRBADV => ctx.gcsr_tlbrbadv = value,
        CSR_TLBRERA => ctx.gcsr_tlbrera = value,
        CSR_TLBRSAVE => ctx.gcsr_tlbrsave = value,
        CSR_TLBRELO0 => ctx.gcsr_tlbrelo0 = value,
        CSR_TLBRELO1 => ctx.gcsr_tlbrelo1 = value,
        CSR_TLBREHI => ctx.gcsr_tlbrehi = value,
        CSR_TLBRPRMD => ctx.gcsr_tlbrprmd = value,
        CSR_DMW0 => ctx.gcsr_dmw0 = value,
        CSR_DMW1 => ctx.gcsr_dmw1 = value,
        CSR_DMW2 => ctx.gcsr_dmw2 = value,
        CSR_DMW3 => ctx.gcsr_dmw3 = value,
        _ => log::debug!(
            "LoongArch GSPR CSR write ignored: csr={:#x}, value={:#x}",
            csr,
            value
        ),
    }
}

fn guest_timer_periodic(ctx: &LoongArchContextFrame) -> bool {
    guest_tcfg_periodic(ctx.gcsr_tcfg)
}

fn guest_timer_init_ticks(ctx: &LoongArchContextFrame) -> u64 {
    guest_tcfg_initval(ctx.gcsr_tcfg) as u64
}

fn mark_guest_timer_expired(ctx: &mut LoongArchContextFrame) {
    ctx.gcsr_tval = 0;
    ctx.gcsr_estat |= TIMER_BIT;
}

fn cancel_guest_timer<H: LoongArchHostOps>(guest_timer_token: &mut Option<usize>) {
    if let Some(token) = guest_timer_token.take() {
        H::cancel_timer(token);
    }
}

fn register_guest_timer<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) {
    cancel_guest_timer::<H>(guest_timer_token);

    if !guest_tcfg_enabled(ctx.gcsr_tcfg) {
        return;
    }

    let init_ticks = guest_timer_init_ticks(ctx);
    if init_ticks == 0 {
        mark_guest_timer_expired(ctx);
        if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
            log::trace!(
                "LoongArch guest timer immediate: tcfg={:#x}, estat={:#x}",
                ctx.gcsr_tcfg,
                ctx.gcsr_estat
            );
        }
        return;
    }

    ctx.gcsr_tval = init_ticks as usize;
    let delay_ns = H::ticks_to_nanos(init_ticks);
    let deadline_ns = H::current_time_nanos().saturating_add(delay_ns);
    if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
        log::trace!(
            "LoongArch guest timer arm: tcfg={:#x}, init_ticks={}, delay_ns={}, deadline_ns={}",
            ctx.gcsr_tcfg,
            init_ticks,
            delay_ns,
            deadline_ns
        );
    }
    let Some(token) = H::register_timer(
        Duration::from_nanos(deadline_ns),
        Box::new(move |_| H::inject_interrupt(vm_id, vcpu_id, INT_TIMER)),
    ) else {
        mark_guest_timer_expired(ctx);
        if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
            log::warn!(
                "LoongArch guest timer capacity exhausted: tcfg={:#x}, estat={:#x}",
                ctx.gcsr_tcfg,
                ctx.gcsr_estat
            );
        }
        return;
    };
    *guest_timer_token = Some(token);
}

fn write_guest_timer_csr<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    csr: usize,
    value: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) {
    match csr {
        CSR_TCFG => {
            ctx.gcsr_tcfg = value;
            register_guest_timer::<H>(ctx, vm_id, vcpu_id, guest_timer_token);
        }
        CSR_TVAL => {
            ctx.gcsr_tval = value;
        }
        CSR_TICLR => {
            ctx.gcsr_ticlr = value;
            if guest_ticlr_has_timer_interrupt_clear(value) {
                ctx.gcsr_estat &= !TIMER_BIT;
                if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
                    log::warn!(
                        "LoongArch guest timer clear: tcfg={:#x}, ticlr={:#x}, periodic={}, \
                         estat={:#x}",
                        ctx.gcsr_tcfg,
                        value,
                        guest_timer_periodic(ctx),
                        ctx.gcsr_estat
                    );
                }
                if guest_timer_periodic(ctx) {
                    register_guest_timer::<H>(ctx, vm_id, vcpu_id, guest_timer_token);
                } else {
                    ctx.gcsr_tcfg &= !guest_tcfg_enable_mask();
                    cancel_guest_timer::<H>(guest_timer_token);
                }
            }
        }
        _ => {}
    }
}

fn emulate_guest_csr<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    rd: usize,
    rj: usize,
    csr: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) {
    let old_value = read_guest_csr(ctx, csr);
    let mut return_value = old_value;

    if rj != 0 {
        let new_value = if rj == 1 {
            ctx.x[rd]
        } else {
            let mask = ctx.x[rj];
            return_value &= mask;
            (old_value & !mask) | (ctx.x[rd] & mask)
        };
        write_guest_csr::<H>(ctx, csr, new_value, vm_id, vcpu_id, guest_timer_token);
    }

    ctx.set_gpr(rd, return_value);
}

pub(crate) fn emulate_cacop(ctx: &mut LoongArchContextFrame, _ins: usize) -> LoongArchVmExit {
    log::trace!(
        "LoongArch GSPR cacop emulation skipped at guest_pc={:#x}",
        get_guest_pc(ctx)
    );
    advance_guest_pc(ctx);
    LoongArchVmExit::Nothing
}

pub(crate) fn emulate_idle(ctx: &mut LoongArchContextFrame, ins: usize) -> LoongArchVmExit {
    let level = extract_field(ins, 0, 15);
    let pending_enabled = ctx.gcsr_estat & ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK;
    let idle_log_index = IDLE_EXIT_LOGS.fetch_add(1, Ordering::Relaxed);
    if idle_log_index < 64 || idle_log_index.is_power_of_two() {
        log::trace!(
            "LoongArch guest idle: pc={:#x}, level={:#x}, pending_enabled={:#x}, eentry={:#x}, \
             crmd={:#x}, estat={:#x}, ecfg={:#x}, tcfg={:#x}, tval={:#x}, ticlr={:#x}",
            get_guest_pc(ctx),
            level,
            pending_enabled,
            ctx.gcsr_eentry,
            ctx.gcsr_crmd,
            ctx.gcsr_estat,
            ctx.gcsr_ectl,
            ctx.gcsr_tcfg,
            ctx.gcsr_tval,
            ctx.gcsr_ticlr,
        );
    }
    if ctx.gcsr_eentry != 0
        && ctx.gcsr_crmd & crmd_interrupt_enable_value() != 0
        && let Some(vector) = decode_interrupt_vector(pending_enabled)
    {
        if idle_log_index < 64 || idle_log_index.is_power_of_two() {
            log::trace!(
                "LoongArch guest idle has pending interrupt: pc={:#x}, vector={}, \
                 pending_enabled={:#x}",
                get_guest_pc(ctx),
                vector,
                pending_enabled,
            );
        }
        inject_guest_interrupt_at(ctx, vector, get_guest_pc(ctx).wrapping_add(4));
        return LoongArchVmExit::Nothing;
    }
    advance_guest_pc(ctx);
    LoongArchVmExit::Idle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LoongArchHostPhysAddr, LoongArchHostVirtAddr,
        registers::{
            guest_tcfg_enabled, guest_tcfg_periodic, guest_tcfg_value,
            guest_ticlr_clear_timer_value,
        },
    };

    struct TimerCapacityExhaustedHost;

    impl LoongArchHostOps for TimerCapacityExhaustedHost {
        fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
            LoongArchHostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            1_000
        }

        fn ticks_to_nanos(ticks: u64) -> u64 {
            ticks
        }

        fn register_timer(
            _deadline: Duration,
            _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> Option<usize> {
            None
        }

        fn cancel_timer(_token: usize) {}

        fn inject_interrupt(_vm_id: usize, _vcpu_id: usize, _vector: usize) {}
    }

    #[test]
    fn timer_capacity_failure_expires_the_guest_timer_without_a_token() {
        let timer_config = guest_tcfg_value(true, false, 400);
        let mut ctx = LoongArchContextFrame::default();
        let mut guest_timer_token = None;

        write_guest_timer_csr::<TimerCapacityExhaustedHost>(
            &mut ctx,
            CSR_TCFG,
            timer_config,
            1,
            2,
            &mut guest_timer_token,
        );

        assert!(guest_timer_token.is_none());
        assert_eq!(ctx.gcsr_tval, 0);
        assert_ne!(ctx.gcsr_estat & TIMER_BIT, 0);
        assert!(guest_tcfg_enabled(ctx.gcsr_tcfg));
    }

    #[test]
    fn periodic_timer_capacity_failure_reasserts_the_cleared_event() {
        let mut ctx = LoongArchContextFrame {
            gcsr_tcfg: guest_tcfg_value(true, true, 400),
            gcsr_estat: TIMER_BIT,
            ..LoongArchContextFrame::default()
        };
        let mut guest_timer_token = None;

        write_guest_timer_csr::<TimerCapacityExhaustedHost>(
            &mut ctx,
            CSR_TICLR,
            guest_ticlr_clear_timer_value(),
            1,
            2,
            &mut guest_timer_token,
        );

        assert!(guest_timer_token.is_none());
        assert_eq!(ctx.gcsr_tval, 0);
        assert_ne!(ctx.gcsr_estat & TIMER_BIT, 0);
        assert!(guest_tcfg_enabled(ctx.gcsr_tcfg));
        assert!(guest_tcfg_periodic(ctx.gcsr_tcfg));
    }
}
