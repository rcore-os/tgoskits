use core::{
    fmt,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};
use std::{boxed::Box, sync::Arc};

use super::{
    LoongArchContextFrame,
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
    types::{LoongArchVcpuId, LoongArchVcpuResult, LoongArchVmExit, LoongArchVmId},
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

pub(crate) struct GuestTimerRegistration<H: LoongArchHostOps> {
    handle: Option<H::TimerHandle>,
    /// Absolute host monotonic deadline of the current logical guest timer.
    ///
    /// Retained across `suspend` so `resume` re-arms the same instant; cleared
    /// by `cancel`/`quiet_timer` and when the timer already fired.
    deadline_ns: Option<u64>,
    next_generation: u64,
    active_generation: Arc<AtomicU64>,
    run: Option<super::super::irq::LoongArchRunPort>,
}

impl<H: LoongArchHostOps> fmt::Debug for GuestTimerRegistration<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuestTimerRegistration")
            .field("armed", &self.handle.is_some())
            .field("deadline_ns", &self.deadline_ns)
            .field("next_generation", &self.next_generation)
            .field(
                "active_generation",
                &self.active_generation.load(Ordering::Relaxed),
            )
            .field("run_bound", &self.run.is_some())
            .finish()
    }
}

impl<H: LoongArchHostOps> GuestTimerRegistration<H> {
    pub(crate) fn new() -> Self {
        Self {
            handle: None,
            deadline_ns: None,
            next_generation: 0,
            active_generation: Arc::new(AtomicU64::new(0)),
            run: None,
        }
    }

    /// Binds the exact run-bound publication capability for this execution
    /// period. Task context, before the guest timer can be armed.
    pub(crate) fn set_run_port(&mut self, port: super::super::irq::LoongArchRunPort) {
        self.run = Some(port);
    }

    fn next_generation(&mut self) -> LoongArchVcpuResult<u64> {
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(super::types::LoongArchVcpuError::TimerUnavailable)?;
        Ok(self.next_generation)
    }

    /// Permanently cancels the outstanding host registration and logical timer.
    ///
    /// Task context only, on the vCPU owner: the architecture cancel port waits
    /// for callback execution and payload reclamation before returning. A failed
    /// retirement keeps the stable handle so the owner can retry from the stop
    /// path instead of leaking a live producer.
    pub(crate) fn cancel(&mut self) -> LoongArchVcpuResult {
        self.retire(false)
    }

    /// Quiesces the host timer producer while preserving the logical deadline.
    ///
    /// Used by `suspend_vcpu`. Already-published pending interrupts live in the
    /// run queue and are deliberately left untouched, and no guest register is
    /// written: the owner task only stops the host producer.
    pub(crate) fn suspend(&mut self) -> LoongArchVcpuResult {
        self.retire(true)
    }

    /// Re-arms a suspended host timer at its preserved absolute deadline.
    ///
    /// A deadline that already elapsed is passed through unchanged: the host
    /// treats an already-due monotonic deadline as immediately executable, so an
    /// interrupt that came due while the vCPU was suspended is delivered on
    /// resume instead of being lost. A timer that already fired is not
    /// re-armed, because its pending interrupt is already published in the run
    /// queue.
    pub(crate) fn resume(
        &mut self,
        vm_id: LoongArchVmId,
        vcpu_id: LoongArchVcpuId,
    ) -> LoongArchVcpuResult {
        let Some(deadline_ns) = self.deadline_ns else {
            return Ok(());
        };
        if self.handle.is_some() {
            return Ok(());
        }
        self.arm(deadline_ns, vm_id, vcpu_id)
    }

    /// Invalidates the live generation and retires the host registration.
    ///
    /// The generation is invalidated first so a callback that has not claimed it
    /// yet cannot publish an obsolete guest interrupt. A callback that did claim
    /// it may still be executing, and a non-blocking host cancellation is not a
    /// completion barrier, so the stable registration is always retired through
    /// the architecture cancel port before its handle is released.
    ///
    /// `keep_deadline` keeps the logical deadline for a later `resume`; it is
    /// dropped for good when the timer already fired (its interrupt is already
    /// queued) or when the caller is cancelling permanently.
    fn retire(&mut self, keep_deadline: bool) -> LoongArchVcpuResult {
        let Some(handle) = self.handle else {
            // Nothing is armed. A suspended timer keeps its deadline so it can
            // still be resumed; a permanent retirement clears it so a later
            // `resume` stays a no-op.
            if !keep_deadline {
                self.deadline_ns = None;
            }
            self.active_generation.store(0, Ordering::Release);
            return Ok(());
        };
        // A live registration exists. Invalidate the generation before retiring
        // it so a callback that has not claimed it yet cannot publish an
        // obsolete guest interrupt. A generation that was already 0 means the
        // callback claimed it and its interrupt is already queued.
        let active = self.active_generation.swap(0, Ordering::AcqRel);
        let already_fired = active == 0;
        if let Err(error) = H::cancel_timer(handle) {
            // The producer is not quiet yet, so the logical timer must stay
            // coherent for the owner's retry: restore the generation and keep the
            // stable handle and deadline untouched.
            self.active_generation.store(active, Ordering::Release);
            return Err(error);
        }
        self.handle = None;
        if !keep_deadline || already_fired {
            self.deadline_ns = None;
        }
        Ok(())
    }

    /// Registers the host callback for one absolute monotonic deadline.
    pub(crate) fn arm(
        &mut self,
        deadline_ns: u64,
        vm_id: LoongArchVmId,
        vcpu_id: LoongArchVcpuId,
    ) -> LoongArchVcpuResult {
        let generation = self.next_generation()?;
        self.active_generation.store(generation, Ordering::Release);
        let active_generation = Arc::clone(&self.active_generation);
        // Capture the exact run capability now, so a timer that outlives its run
        // publishes into that closed run and is rejected instead of resolving a
        // newer run.
        let run = self.run.clone();
        let registration = H::register_timer(
            Duration::from_nanos(deadline_ns),
            Box::new(move |_| {
                if !claim_guest_timer_generation(&active_generation, generation) {
                    return;
                }
                let Some(run) = run.as_ref() else {
                    log::trace!(
                        "LoongArch guest timer for VM[{vm_id}] VCpu[{vcpu_id}] fired without a \
                         bound run"
                    );
                    return;
                };
                match super::super::irq::LoongArchRunPort::virtual_interrupt(INT_TIMER) {
                    Ok(interrupt) => {
                        if let Err(error) = run.publish_virtual_irq(vcpu_id, interrupt) {
                            log::trace!(
                                "LoongArch guest timer interrupt for VM[{vm_id}] VCpu[{vcpu_id}] \
                                 was not delivered: {error:?}"
                            );
                        }
                    }
                    Err(error) => {
                        log::trace!("LoongArch guest timer vector is invalid: {error:?}");
                    }
                }
            }),
        );
        let handle = registration.inspect_err(|_| {
            let _ = self.active_generation.compare_exchange(
                generation,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        })?;
        self.handle = Some(handle);
        self.deadline_ns = Some(deadline_ns);
        Ok(())
    }
}

fn claim_guest_timer_generation(active_generation: &AtomicU64, generation: u64) -> bool {
    active_generation
        .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

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

/// Resolves the guest-visible `cpucfg` word for one index on the pinned CPU.
///
/// This is the only caller of the CPU-local CPUCFG read, and it runs from the
/// pinned capture stage. The task-stage emulator consumes the resolved value
/// through the exit snapshot instead of re-reading CPUCFG on a migrated CPU.
pub(super) fn guest_cpucfg_value(index: usize) -> usize {
    let mut value = if index > 20 { 0 } else { host_cpucfg(index) };
    if index == 2 {
        value &= !CPUCFG2_CRYPTO;
    }
    value
}

/// Retires a `cpucfg` read with the value resolved while the backend was pinned.
pub(crate) fn emulate_cpucfg(
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    pinned_value: usize,
) -> LoongArchVmExit {
    let rd = extract_field(ins, 0, 5);
    ctx.set_gpr(rd, pinned_value);
    advance_guest_pc(ctx);
    LoongArchVmExit::Nothing
}

pub(crate) fn emulate_csrx<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer: &mut GuestTimerRegistration<H>,
) -> LoongArchVcpuResult<LoongArchVmExit> {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let csr = extract_field(ins, 10, 14);

    emulate_guest_csr::<H>(ctx, rd, rj, csr, vm_id, vcpu_id, guest_timer)?;
    advance_guest_pc(ctx);
    Ok(LoongArchVmExit::Nothing)
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
    guest_timer: &mut GuestTimerRegistration<H>,
) -> LoongArchVcpuResult {
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
            write_guest_timer_csr::<H>(ctx, csr, value, vm_id, vcpu_id, guest_timer)?
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
    Ok(())
}

fn guest_timer_periodic(ctx: &LoongArchContextFrame) -> bool {
    guest_tcfg_periodic(ctx.gcsr_tcfg)
}

fn guest_timer_init_ticks(ctx: &LoongArchContextFrame) -> u64 {
    guest_tcfg_initval(ctx.gcsr_tcfg) as u64
}

fn register_guest_timer<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer: &mut GuestTimerRegistration<H>,
) -> LoongArchVcpuResult {
    guest_timer.cancel()?;

    if !guest_tcfg_enabled(ctx.gcsr_tcfg) {
        return Ok(());
    }

    let init_ticks = guest_timer_init_ticks(ctx);
    if init_ticks == 0 {
        ctx.gcsr_tval = 0;
        ctx.gcsr_estat |= TIMER_BIT;
        if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
            log::trace!(
                "LoongArch guest timer immediate: tcfg={:#x}, estat={:#x}",
                ctx.gcsr_tcfg,
                ctx.gcsr_estat
            );
        }
        return Ok(());
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
    guest_timer.arm(deadline_ns, vm_id, vcpu_id)
}

fn write_guest_timer_csr<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    csr: usize,
    value: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer: &mut GuestTimerRegistration<H>,
) -> LoongArchVcpuResult {
    match csr {
        CSR_TCFG => {
            ctx.gcsr_tcfg = value;
            if let Err(error) = register_guest_timer::<H>(ctx, vm_id, vcpu_id, guest_timer) {
                ctx.gcsr_tcfg &= !guest_tcfg_enable_mask();
                ctx.gcsr_tval = 0;
                return Err(error);
            }
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
                    if let Err(error) = register_guest_timer::<H>(ctx, vm_id, vcpu_id, guest_timer)
                    {
                        ctx.gcsr_tcfg &= !guest_tcfg_enable_mask();
                        ctx.gcsr_tval = 0;
                        return Err(error);
                    }
                } else {
                    ctx.gcsr_tcfg &= !guest_tcfg_enable_mask();
                    guest_timer.cancel()?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn emulate_guest_csr<H: LoongArchHostOps>(
    ctx: &mut LoongArchContextFrame,
    rd: usize,
    rj: usize,
    csr: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer: &mut GuestTimerRegistration<H>,
) -> LoongArchVcpuResult {
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
        write_guest_csr::<H>(ctx, csr, new_value, vm_id, vcpu_id, guest_timer)?;
    }

    ctx.set_gpr(rd, return_value);
    Ok(())
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
    use core::{
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
        time::Duration,
    };
    use std::boxed::Box;

    use super::{
        super::{LoongArchHostPhysAddr, LoongArchHostVirtAddr, types::LoongArchVcpuError},
        GuestTimerRegistration, LoongArchContextFrame, LoongArchHostOps, LoongArchVcpuResult,
        claim_guest_timer_generation, guest_tcfg_enable_mask, register_guest_timer,
    };

    const NOW_NS: u64 = 1_000;
    const INIT_TICKS: u64 = 400;
    const ARMED_DEADLINE_NS: u64 = NOW_NS + INIT_TICKS;

    /// Deterministic host timer with no shared mutable state.
    ///
    /// Each recording host below is a distinct type, so parallel tests never
    /// observe each other's timer state.
    struct TestHost;

    impl LoongArchHostOps for TestHost {
        type TimerHandle = u64;

        fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
            LoongArchHostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            NOW_NS
        }

        fn ticks_to_nanos(ticks: u64) -> u64 {
            ticks
        }

        fn register_timer(
            _deadline: Duration,
            _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> LoongArchVcpuResult<Self::TimerHandle> {
            Ok(1)
        }

        fn cancel_timer(_handle: Self::TimerHandle) -> LoongArchVcpuResult {
            Ok(())
        }
    }

    /// Host whose cancellation always fails, so a test can tell whether the
    /// cancel port was invoked at all.
    struct FailingCancelHost;

    impl LoongArchHostOps for FailingCancelHost {
        type TimerHandle = u64;

        fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
            LoongArchHostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            NOW_NS
        }

        fn ticks_to_nanos(ticks: u64) -> u64 {
            ticks
        }

        fn register_timer(
            _deadline: Duration,
            _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> LoongArchVcpuResult<Self::TimerHandle> {
            Ok(1)
        }

        fn cancel_timer(_handle: Self::TimerHandle) -> LoongArchVcpuResult {
            Err(LoongArchVcpuError::TimerUnavailable)
        }
    }

    /// Host whose next `CANCEL_FAILURES` cancellations fail, then succeed.
    struct FailOnceCancelHost;

    static CANCEL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CANCEL_FAILURES: AtomicUsize = AtomicUsize::new(0);

    impl LoongArchHostOps for FailOnceCancelHost {
        type TimerHandle = u64;

        fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
            LoongArchHostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            NOW_NS
        }

        fn ticks_to_nanos(ticks: u64) -> u64 {
            ticks
        }

        fn register_timer(
            _deadline: Duration,
            _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> LoongArchVcpuResult<Self::TimerHandle> {
            Ok(1)
        }

        fn cancel_timer(_handle: Self::TimerHandle) -> LoongArchVcpuResult {
            CANCEL_CALLS.fetch_add(1, Ordering::Relaxed);
            if CANCEL_FAILURES
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |failures| {
                    failures.checked_sub(1)
                })
                .is_ok()
            {
                return Err(LoongArchVcpuError::TimerUnavailable);
            }
            Ok(())
        }
    }

    /// Host with a test-controlled monotonic clock, for the already-elapsed
    /// deadline case.
    struct AdvancingClockHost;

    static HOST_NOW_NS: AtomicU64 = AtomicU64::new(0);

    impl LoongArchHostOps for AdvancingClockHost {
        type TimerHandle = u64;

        fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
            LoongArchHostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            HOST_NOW_NS.load(Ordering::Relaxed)
        }

        fn ticks_to_nanos(ticks: u64) -> u64 {
            ticks
        }

        fn register_timer(
            _deadline: Duration,
            _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> LoongArchVcpuResult<Self::TimerHandle> {
            Ok(1)
        }

        fn cancel_timer(_handle: Self::TimerHandle) -> LoongArchVcpuResult {
            Ok(())
        }
    }

    /// Encodes an enabled guest timer that expires after `init_ticks` ticks.
    fn enabled_tcfg(init_ticks: u64) -> usize {
        (((init_ticks >> 2) as usize) << 2) | guest_tcfg_enable_mask()
    }

    /// Arms the guest timer through the real CSR path, so the deadline math of
    /// `register_guest_timer` is exercised as well.
    fn arm_guest_timer<H: LoongArchHostOps>(
        ctx: &mut LoongArchContextFrame,
        timer: &mut GuestTimerRegistration<H>,
    ) -> LoongArchVcpuResult {
        ctx.gcsr_tcfg = enabled_tcfg(INIT_TICKS);
        register_guest_timer::<H>(ctx, 1, 0, timer)
    }

    #[test]
    fn cancelled_generation_cannot_inject_after_rearm() {
        let mut timer = GuestTimerRegistration::<TestHost>::new();
        let old_generation = timer.next_generation().unwrap();
        timer
            .active_generation
            .store(old_generation, Ordering::Release);
        timer.handle = Some(1);
        timer.cancel().unwrap();

        let new_generation = timer.next_generation().unwrap();
        timer
            .active_generation
            .store(new_generation, Ordering::Release);
        assert!(!claim_guest_timer_generation(
            &timer.active_generation,
            old_generation
        ));
        assert!(claim_guest_timer_generation(
            &timer.active_generation,
            new_generation
        ));
    }

    #[test]
    fn claimed_generation_still_retires_the_host_registration() {
        let mut timer = GuestTimerRegistration::<FailingCancelHost>::new();
        let generation = timer.next_generation().unwrap();
        timer.active_generation.store(generation, Ordering::Release);
        timer.handle = Some(1);
        // The host callback claims the generation. A non-blocking host
        // cancellation is not a barrier, so the callback may still be executing.
        assert!(claim_guest_timer_generation(
            &timer.active_generation,
            generation
        ));

        // The generation is already consumed, yet the stable registration must
        // still be retired through the cancel port, so the always-failing host
        // reports failure and the handle is retained for a retry.
        assert_eq!(timer.cancel(), Err(LoongArchVcpuError::TimerUnavailable));
        assert_eq!(timer.handle, Some(1));
    }

    #[test]
    fn failed_cancel_preserves_registration_for_retry() {
        CANCEL_CALLS.store(0, Ordering::Relaxed);
        CANCEL_FAILURES.store(1, Ordering::Relaxed);
        let mut timer = GuestTimerRegistration::<FailOnceCancelHost>::new();
        let generation = timer.next_generation().unwrap();
        timer.active_generation.store(generation, Ordering::Release);
        timer.handle = Some(1);

        assert_eq!(timer.cancel(), Err(LoongArchVcpuError::TimerUnavailable));
        // The registration is still live, so the owner must retry with the same
        // stable handle instead of dropping it.
        assert_eq!(timer.handle, Some(1));
        // The logical timer stays coherent until the retry: the generation is
        // restored, so a callback that fires before then is still its own.
        assert_eq!(timer.active_generation.load(Ordering::Acquire), generation);
        assert_eq!(CANCEL_CALLS.load(Ordering::Relaxed), 1);

        timer.cancel().unwrap();
        assert_eq!(timer.handle, None);
        assert_eq!(CANCEL_CALLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn suspend_preserves_logical_deadline_and_resume_rearms_same_instant() {
        let mut ctx = LoongArchContextFrame::default();
        let mut timer = GuestTimerRegistration::<TestHost>::new();
        arm_guest_timer(&mut ctx, &mut timer).unwrap();

        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));
        assert!(timer.handle.is_some());

        timer.suspend().unwrap();
        assert_eq!(timer.handle, None);
        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));

        // Re-arming restores the live handle at the identical absolute deadline.
        timer.resume(1, 0).unwrap();
        assert!(timer.handle.is_some());
        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));
    }

    #[test]
    fn repeated_suspend_keeps_the_preserved_deadline() {
        let mut ctx = LoongArchContextFrame::default();
        let mut timer = GuestTimerRegistration::<TestHost>::new();
        arm_guest_timer(&mut ctx, &mut timer).unwrap();

        timer.suspend().unwrap();
        timer.suspend().unwrap();
        assert_eq!(timer.handle, None);
        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));

        timer.resume(1, 0).unwrap();
        assert!(timer.handle.is_some());
        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));
    }

    #[test]
    fn resume_rearms_an_already_elapsed_deadline_as_already_due() {
        HOST_NOW_NS.store(NOW_NS, Ordering::Relaxed);
        let mut ctx = LoongArchContextFrame::default();
        let mut timer = GuestTimerRegistration::<AdvancingClockHost>::new();
        arm_guest_timer(&mut ctx, &mut timer).unwrap();
        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));
        timer.suspend().unwrap();

        // Time passes while the vCPU is suspended, past the preserved deadline.
        HOST_NOW_NS.store(ARMED_DEADLINE_NS + 1, Ordering::Relaxed);
        timer.resume(1, 0).unwrap();

        // The same absolute deadline is re-armed; it is already in the past, so
        // the host runs it immediately instead of losing the interrupt.
        assert_eq!(timer.deadline_ns, Some(ARMED_DEADLINE_NS));
        assert!(timer.deadline_ns.unwrap() <= HOST_NOW_NS.load(Ordering::Relaxed));
        assert!(timer.handle.is_some());
    }

    #[test]
    fn suspend_after_the_timer_fired_does_not_rearm_on_resume() {
        let mut ctx = LoongArchContextFrame::default();
        let mut timer = GuestTimerRegistration::<TestHost>::new();
        arm_guest_timer(&mut ctx, &mut timer).unwrap();

        // Simulate the host callback firing and publishing its pending interrupt.
        let generation = timer.active_generation.load(Ordering::Acquire);
        assert!(claim_guest_timer_generation(
            &timer.active_generation,
            generation
        ));

        timer.suspend().unwrap();
        // The interrupt is already queued, so there is nothing left to resume.
        assert_eq!(timer.deadline_ns, None);
        assert_eq!(timer.handle, None);

        timer.resume(1, 0).unwrap();
        assert_eq!(timer.handle, None);
        assert_eq!(timer.deadline_ns, None);
    }

    #[test]
    fn quiet_permanently_retires_the_logical_timer() {
        let mut ctx = LoongArchContextFrame::default();
        let mut timer = GuestTimerRegistration::<TestHost>::new();
        arm_guest_timer(&mut ctx, &mut timer).unwrap();

        timer.cancel().unwrap();
        assert_eq!(timer.handle, None);
        assert_eq!(timer.deadline_ns, None);

        timer.resume(1, 0).unwrap();
        assert_eq!(timer.handle, None);
        assert_eq!(timer.deadline_ns, None);
    }
}
