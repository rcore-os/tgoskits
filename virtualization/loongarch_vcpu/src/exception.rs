use core::{
    mem::offset_of,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    context_frame::LoongArchContextFrame,
    guest_addr::{
        direct_map_guest_addr_to_gpa, get_refill_access_flags, should_inject_guest_virtual_fault,
    },
    guest_csr::{
        emulate_cacop, emulate_cpucfg, emulate_csrx, emulate_idle, inject_guest_regular_exception,
        inject_guest_tlb_refill,
    },
    host::LoongArchHostOps,
    host_cpu::ack_host_timer_interrupt,
    iocsr::{LoongArchIocsrState, emulate_iocsr},
    trap::{
        ECODE_ADE, ECODE_GSPR, ECODE_HVC, ECODE_PIF, ECODE_PIL, ECODE_PIS, ECODE_PME, ECODE_PNR,
        ECODE_PNX, ECODE_PPI, ECODE_RSE, ESUBCODE_ADEF, ESUBCODE_ADEM, INT_TIMER, TIMER_BIT,
        advance_guest_pc, decode_interrupt_vector, extract_field, get_badi, get_badv,
        get_exception_code, get_exception_subcode, get_guest_interrupt_status, get_guest_pc,
        is_host_tlb_refill,
    },
    types::{
        LoongArchAccessFlags, LoongArchGuestPhysAddr, LoongArchVcpuId, LoongArchVcpuResult,
        LoongArchVmExit, LoongArchVmId,
    },
};

const LOONGARCH_KSAVE_CSR_BASE: usize = 0x30;

// exception.S requires literal `.equ` values. Bind them to the shared
// allocation so ax-cpu, cpu-local, and the vCPU cannot drift independently.
const _: () = {
    use cpu_local::loongarch64::{
        HOST_PERCPU_KS, HOST_VCPU_KS, HOST_VCPU_TMP_KS, KSAVE_KSP, KSAVE_T0, KSAVE_T1,
    };

    assert!(LOONGARCH_KSAVE_CSR_BASE + KSAVE_KSP == 0x30);
    assert!(LOONGARCH_KSAVE_CSR_BASE + KSAVE_T0 == 0x31);
    assert!(LOONGARCH_KSAVE_CSR_BASE + KSAVE_T1 == 0x32);
    assert!(LOONGARCH_KSAVE_CSR_BASE + HOST_PERCPU_KS == 0x33);
    assert!(LOONGARCH_KSAVE_CSR_BASE + HOST_VCPU_KS == 0x34);
    assert!(LOONGARCH_KSAVE_CSR_BASE + HOST_VCPU_TMP_KS == 0x35);
};

static NESTED_FAULT_LOGS: AtomicUsize = AtomicUsize::new(0);
static SYNC_EXIT_LOGS: AtomicUsize = AtomicUsize::new(0);
static TARGET_GSPR_LOGS: AtomicUsize = AtomicUsize::new(0);

fn emulate_gspr<H: LoongArchHostOps>(
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) -> LoongArchVmExit {
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
    let pc = get_guest_pc(ctx);
    if (0x9000_0000_0159_0000..0x9000_0000_015a_0000).contains(&pc)
        && TARGET_GSPR_LOGS.fetch_add(1, Ordering::Relaxed) < 64
    {
        log::trace!(
            "LoongArch target GSPR: pc={:#x}, ins={:#x}, rd={}, rj={}, csr={:#x}, iocsr_ty={}, \
             a0={:#x}, a1={:#x}, a2={:#x}, estat={:#x}, ecfg={:#x}, tcfg={:#x}, tval={:#x}",
            pc,
            ins,
            extract_field(ins, 0, 5),
            extract_field(ins, 5, 5),
            extract_field(ins, 10, 14),
            extract_field(ins, 10, 3),
            ctx.get_a0(),
            ctx.get_a1(),
            ctx.get_a2(),
            ctx.gcsr_estat,
            ctx.gcsr_ectl,
            ctx.gcsr_tcfg,
            ctx.gcsr_tval,
        );
    }

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
        return emulate_csrx::<H>(ctx, ins, vm_id, vcpu_id, guest_timer_token);
    }
    if matches(OPCODE_IOCSR, OPCODE_IOCSR_LEN) {
        return emulate_iocsr::<H>(state, ctx, ins, vm_id, vcpu_id);
    }

    panic!(
        "Unhandled LoongArch GSPR instruction: pc={:#x}, badi={:#x}",
        get_guest_pc(ctx),
        ins
    );
}

pub fn handle_exception_sync<H: LoongArchHostOps>(
    iocsr_state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    guest_timer_token: &mut Option<usize>,
) -> LoongArchVcpuResult<LoongArchVmExit> {
    let ecode = get_exception_code(ctx);
    let esubcode = get_exception_subcode(ctx);
    if SYNC_EXIT_LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
        log::trace!(
            "LoongArch guest sync exit: ecode={:#x}, esubcode={:#x}, guest_pc={:#x}, \
             host_era={:#x}, gera={:#x}, badv={:#x}, badi={:#x}, guest_is={:#x}, \
             host_estat={:#x}, tlbrbadv={:#x}, tlbrera={:#x}",
            ecode,
            esubcode,
            get_guest_pc(ctx),
            ctx.host_era,
            ctx.gcsr_era,
            get_badv(ctx),
            get_badi(ctx),
            get_guest_interrupt_status(ctx),
            ctx.host_estat,
            ctx.host_tlbrbadv,
            ctx.host_tlbrera,
        );
    }

    log::trace!(
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
            return Ok(LoongArchVmExit::Nothing);
        }
        ctx.sepc = get_guest_pc(ctx);
        if NESTED_FAULT_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
            log::trace!(
                "LoongArch nested fault from host refill: badv={:#x}, gpa={:#x}, pc={:#x}, \
                 crmd={:#x}, eentry={:#x}, tlbrentry={:#x}, tlbrera={:#x}, host_estat={:#x}",
                badv,
                direct_map_guest_addr_to_gpa(badv),
                get_guest_pc(ctx),
                ctx.gcsr_crmd,
                ctx.gcsr_eentry,
                ctx.gcsr_tlbrentry,
                ctx.host_tlbrera,
                ctx.host_estat
            );
        }
        return Ok(LoongArchVmExit::NestedPageFault {
            addr: LoongArchGuestPhysAddr::from(direct_map_guest_addr_to_gpa(badv)),
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
            Ok(LoongArchVmExit::Hypercall { nr, args })
        }
        ECODE_GSPR => Ok(emulate_gspr::<H>(
            iocsr_state,
            ctx,
            vm_id,
            vcpu_id,
            guest_timer_token,
        )),
        ECODE_PIL | ECODE_PIS | ECODE_PIF | ECODE_PME | ECODE_PNR | ECODE_PNX | ECODE_PPI => {
            let badv = get_badv(ctx);
            if should_inject_guest_virtual_fault(ctx, badv, false) {
                inject_guest_regular_exception(ctx, ecode, esubcode, badv);
                return Ok(LoongArchVmExit::Nothing);
            }
            let mut access_flags = LoongArchAccessFlags::empty();
            if matches!(ecode, ECODE_PIS | ECODE_PME) {
                access_flags |= LoongArchAccessFlags::WRITE;
            } else if ecode == ECODE_PIF {
                access_flags |= LoongArchAccessFlags::EXECUTE;
            } else {
                access_flags |= LoongArchAccessFlags::READ;
            }
            if NESTED_FAULT_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
                log::trace!(
                    "LoongArch nested fault from regular exception: ecode={:#x}, badv={:#x}, \
                     gpa={:#x}, pc={:#x}, crmd={:#x}, eentry={:#x}, tlbrentry={:#x}, \
                     host_estat={:#x}",
                    ecode,
                    badv,
                    direct_map_guest_addr_to_gpa(badv),
                    get_guest_pc(ctx),
                    ctx.gcsr_crmd,
                    ctx.gcsr_eentry,
                    ctx.gcsr_tlbrentry,
                    ctx.host_estat
                );
            }
            Ok(LoongArchVmExit::NestedPageFault {
                addr: LoongArchGuestPhysAddr::from(direct_map_guest_addr_to_gpa(badv)),
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
        ECODE_RSE => Ok(LoongArchVmExit::Halt),
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

pub fn handle_exception_irq(
    ctx: &mut LoongArchContextFrame,
) -> LoongArchVcpuResult<LoongArchVmExit> {
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

        return Ok(LoongArchVmExit::ExternalInterrupt {
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
    Ok(LoongArchVmExit::Nothing)
}

core::arch::global_asm!(
    include_str!("exception.S"),
    ctx_size = const core::mem::size_of::<LoongArchContextFrame>(),
    host_pgdl = const offset_of!(LoongArchContextFrame, host_pgdl),
    host_pgdh = const offset_of!(LoongArchContextFrame, host_pgdh),
    host_pwcl = const offset_of!(LoongArchContextFrame, host_pwcl),
    host_pwch = const offset_of!(LoongArchContextFrame, host_pwch),
    host_stlbps = const offset_of!(LoongArchContextFrame, host_stlbps),
    host_tlbrentry = const offset_of!(LoongArchContextFrame, host_tlbrentry),
    host_asid = const offset_of!(LoongArchContextFrame, host_asid),
    host_eentry = const offset_of!(LoongArchContextFrame, host_eentry),
    host_ecfg = const offset_of!(LoongArchContextFrame, host_ecfg),
    guest_tlbrentry = const offset_of!(LoongArchContextFrame, guest_tlbrentry),
    guest_eentry = const offset_of!(LoongArchContextFrame, guest_eentry),
);

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
        // SAVE_GUEST_REGS restored the CPU-owned r21 from the KS3 shadow.
        "addi.d $sp, $sp, 14 * 8",
        "jr $ra",
        ctx_size = const core::mem::size_of::<LoongArchContextFrame>(),
    )
}
