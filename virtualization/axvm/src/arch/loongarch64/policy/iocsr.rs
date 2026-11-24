//! VM-owned IOCSR state and complete instruction interpretation.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{boxed::Box, sync::Arc};

use ax_std::os::arceos::sync::RawSpinLock;

use super::{
    LoongArchContextFrame,
    guest_csr::inject_guest_interrupt_at,
    trap::{
        HWI_MASK, INT_HWI0, INT_HWI7, INT_IPI, IPI_BIT, LOCAL_INTERRUPT_MASK, advance_guest_pc,
        decode_interrupt_vector, extract_field, get_guest_pc,
    },
    types::{
        LoongArchEmulationFault, LoongArchIocsrDirection, LoongArchIocsrFaultReason,
        LoongArchVcpuError, LoongArchVcpuId, LoongArchVcpuResult, LoongArchVmExit, LoongArchVmId,
    },
};
use crate::arch::loongarch64::irq::LoongArchRunPort;

mod access;

pub(crate) const EIOINTC_ISR_BASE: usize = 0x1800;
pub(crate) const EIOINTC_ISR_REG_COUNT: usize = 4;
pub(crate) const EIOINTC_ISR_END: usize = EIOINTC_ISR_BASE + EIOINTC_ISR_REG_COUNT * 8;
const EIOINTC_VECTOR_COUNT: usize = EIOINTC_ISR_REG_COUNT * 64;
const EIOINTC_NODEMAP_BASE: usize = 0x14a0;
const EIOINTC_NODEMAP_END: usize = 0x14c0;
const EIOINTC_IPMAP_BASE: usize = 0x14c0;
const EIOINTC_IPMAP_END: usize = 0x14c8;
const EIOINTC_ENABLE_BASE: usize = 0x1600;
const EIOINTC_ENABLE_END: usize = 0x1620;
const EIOINTC_BOUNCE_BASE: usize = 0x1680;
const EIOINTC_BOUNCE_END: usize = 0x16a0;
const EIOINTC_ISR_COMPAT_BASE: usize = 0x1700;
const EIOINTC_ISR_COMPAT_END: usize = 0x1720;
const EIOINTC_COREMAP_BASE: usize = 0x1c00;
const EIOINTC_COREMAP_END: usize = 0x1d00;
const IOCSR_MISC_FUNC: usize = 0x420;
const MISC_EIO_ENABLE: usize = 1 << 48;
const LOONGARCH_IOCSR_IPI_STATUS: usize = 0x1000;
const LOONGARCH_IOCSR_IPI_EN: usize = 0x1004;
const LOONGARCH_IOCSR_IPI_SET: usize = 0x1008;
const LOONGARCH_IOCSR_IPI_CLEAR: usize = 0x100c;
const LOONGARCH_IOCSR_MAIL_BUF0: usize = 0x1020;
const IOCSR_MAIL_END: usize = 0x1040;
const LOONGARCH_IOCSR_IPI_SEND: usize = 0x1040;
const LOONGARCH_IOCSR_MBUF_SEND: usize = 0x1048;
const LOONGARCH_IOCSR_ANY_SEND: usize = 0x1158;
const EXTIOI_VIRT_FEATURES: usize = 0x4000_0000;
const EXTIOI_VIRT_CONFIG: usize = 0x4000_0004;
const CONFIG_EIO_ENABLE: usize = 2;

pub type LoongArchIocsrStateRef = Arc<LoongArchIocsrState>;

/// Register state owned by one prepared VM execution period.
#[derive(Debug)]
pub struct LoongArchIocsrState {
    vcpus: Box<[LoongArchVcpuIocsrState]>,
    configured_vcpus: Box<[bool]>,
    // Both MISC_FUNC and CONFIG publish through this single atomic.
    eio_enabled: AtomicBool,
    run: RawSpinLock<Option<LoongArchRunPort>>,
}

impl LoongArchIocsrState {
    /// Creates reset state for the validated placement IDs, including sparse IDs.
    pub fn new(ids: &[LoongArchVcpuId]) -> LoongArchVcpuResult<LoongArchIocsrStateRef> {
        let count = ids
            .iter()
            .max()
            .copied()
            .map_or(Some(0), |id| id.checked_add(1))
            .ok_or(LoongArchVcpuError::InvalidInput)?;
        let mut configured_vcpus = std::vec![false; count];
        let mut vcpus = std::vec::Vec::with_capacity(count);
        for _ in 0..count {
            vcpus.push(LoongArchVcpuIocsrState::new());
        }
        for &id in ids {
            if configured_vcpus[id] {
                return Err(LoongArchVcpuError::InvalidInput);
            }
            configured_vcpus[id] = true;
        }
        Ok(Arc::new(Self {
            vcpus: vcpus.into_boxed_slice(),
            configured_vcpus: configured_vcpus.into_boxed_slice(),
            eio_enabled: AtomicBool::new(false),
            run: RawSpinLock::new(None),
        }))
    }

    /// Binds the exact run capability before guest entry is admitted.
    pub(crate) fn set_run_port(&self, port: LoongArchRunPort) {
        *self.run.lock_irqsave() = Some(port);
    }

    fn run_port(&self) -> Option<LoongArchRunPort> {
        self.run.lock_irqsave().as_ref().cloned()
    }

    fn vcpu(&self, id: LoongArchVcpuId) -> Option<&LoongArchVcpuIocsrState> {
        self.configured_vcpus
            .get(id)
            .copied()
            .filter(|valid| *valid)?;
        self.vcpus.get(id)
    }

    fn pending_hwi(&self, id: LoongArchVcpuId) -> Option<usize> {
        if !self.eio_enabled.load(Ordering::Acquire) {
            return None;
        }
        guest_eiointc_pending_hwi(self.vcpu(id)?)
    }

    /// Pending facts survive a retired run's notification failure. Clone under
    /// the run lock, then publish outside it; never resolve a replacement run.
    fn notify(&self, id: LoongArchVcpuId, vector: usize) {
        if let Some(run) = self.run_port()
            && let Ok(interrupt) = LoongArchRunPort::virtual_interrupt(vector)
            && let Err(error) = run.publish_virtual_irq(id, interrupt)
        {
            log::trace!("LoongArch IOCSR notification for VCpu[{id}] was not delivered: {error:?}");
        }
    }

    fn notify_pending(&self, id: LoongArchVcpuId) {
        let Some(vcpu) = self.vcpu(id) else { return };
        if vcpu.ipi_status.load(Ordering::Acquire) & vcpu.ipi_enable.load(Ordering::Acquire) != 0 {
            self.notify(id, INT_IPI);
        }
        if let Some(hwi) = self.pending_hwi(id) {
            self.notify(id, hwi);
        }
    }
}

#[derive(Debug)]
struct LoongArchVcpuIocsrState {
    ipi_status: AtomicUsize,
    ipi_enable: AtomicUsize,
    mail_buf: [AtomicUsize; 4],
    eiointc_isr: [AtomicUsize; EIOINTC_ISR_REG_COUNT],
    eiointc_nodemap: [AtomicUsize; 8],
    eiointc_ipmap: [AtomicUsize; 2],
    eiointc_enable: [AtomicUsize; 8],
    eiointc_bounce: [AtomicUsize; 8],
    eiointc_coremap: [AtomicUsize; 64],
}

impl LoongArchVcpuIocsrState {
    const fn new() -> Self {
        Self {
            ipi_status: AtomicUsize::new(0),
            ipi_enable: AtomicUsize::new(0),
            mail_buf: [const { AtomicUsize::new(0) }; 4],
            eiointc_isr: [const { AtomicUsize::new(0) }; EIOINTC_ISR_REG_COUNT],
            eiointc_nodemap: [const { AtomicUsize::new(0) }; 8],
            eiointc_ipmap: [const { AtomicUsize::new(0) }; 2],
            eiointc_enable: [const { AtomicUsize::new(0) }; 8],
            eiointc_bounce: [const { AtomicUsize::new(0) }; 8],
            eiointc_coremap: [const { AtomicUsize::new(0) }; 64],
        }
    }
}

pub(crate) fn init_guest_iocsr(state: &LoongArchIocsrState, id: LoongArchVcpuId) {
    let Some(vcpu) = state.vcpu(id) else { return };
    // The shared EIO gate is reset only by preparing a new VM state.
    vcpu.ipi_status.store(0, Ordering::Release);
    vcpu.ipi_enable.store(0, Ordering::Release);
    for slot in vcpu.mail_buf.iter().chain(vcpu.eiointc_isr.iter()) {
        slot.store(0, Ordering::Release);
    }
    for (word, slot) in vcpu.eiointc_nodemap.iter().enumerate() {
        slot.store(
            ((1 << (word * 2 + 1)) << 16) | (1 << (word * 2)),
            Ordering::Release,
        );
    }
    for slot in vcpu.eiointc_enable.iter().chain(vcpu.eiointc_bounce.iter()) {
        slot.store(u32::MAX as usize, Ordering::Release);
    }
    for slot in vcpu.eiointc_ipmap.iter().chain(vcpu.eiointc_coremap.iter()) {
        slot.store(0x0101_0101, Ordering::Release);
    }
}

fn eiointc_isr_word(vcpu: &LoongArchVcpuIocsrState, word: usize) -> usize {
    (vcpu.eiointc_isr[word / 2].load(Ordering::Acquire) >> ((word % 2) * 32)) & u32::MAX as usize
}

fn eiointc_cpu_irq_for_source(vcpu: &LoongArchVcpuIocsrState, source: usize) -> Option<usize> {
    let ipmap = vcpu.eiointc_ipmap[source >> 7].load(Ordering::Acquire);
    let pins = (ipmap >> (((source >> 5) & 3) * 8)) & 0xff;
    let route = vcpu.eiointc_coremap[source >> 2].load(Ordering::Acquire);
    // Preserve the existing single-node, traditional bitmap routing view.
    if pins == 0 || (route >> ((source & 3) * 8)) & 1 == 0 {
        return None;
    }
    let hwi = INT_HWI0 + pins.trailing_zeros() as usize;
    (hwi <= INT_HWI7).then_some(hwi)
}

fn guest_eiointc_pending_hwi(vcpu: &LoongArchVcpuIocsrState) -> Option<usize> {
    for word in 0..8 {
        let mut pending =
            eiointc_isr_word(vcpu, word) & vcpu.eiointc_enable[word].load(Ordering::Acquire);
        while pending != 0 {
            let bit = pending.trailing_zeros() as usize;
            if let Some(hwi) = eiointc_cpu_irq_for_source(vcpu, word * 32 + bit) {
                return Some(hwi);
            }
            pending &= pending - 1;
        }
    }
    None
}

/// Derives all IOCSR-owned CPU levels, ignoring stale cached HWI/IPI bits.
pub(crate) fn guest_pending_interrupts(
    state: &LoongArchIocsrState,
    ctx: &LoongArchContextFrame,
    id: LoongArchVcpuId,
) -> usize {
    let mut pending = ctx.gcsr_estat & !(HWI_MASK | IPI_BIT);
    if let Some(vcpu) = state.vcpu(id)
        && vcpu.ipi_status.load(Ordering::Acquire) & vcpu.ipi_enable.load(Ordering::Acquire) != 0
    {
        pending |= IPI_BIT;
    }
    if let Some(hwi) = state.pending_hwi(id) {
        pending |= 1 << hwi;
    }
    pending
}

pub(super) fn update_guest_interrupts(
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    id: usize,
) {
    let pending = guest_pending_interrupts(state, ctx, id);
    ctx.gcsr_estat = (ctx.gcsr_estat & !LOCAL_INTERRUPT_MASK) | (pending & LOCAL_INTERRUPT_MASK);
}

pub(crate) fn inject_guest_eiointc_vector(
    state: &LoongArchIocsrState,
    _vm_id: LoongArchVmId,
    id: LoongArchVcpuId,
    vector: usize,
) -> Option<usize> {
    if vector >= EIOINTC_VECTOR_COUNT {
        return None;
    }
    let vcpu = state.vcpu(id)?;
    vcpu.eiointc_isr[vector / 64].fetch_or(1 << (vector % 64), Ordering::AcqRel);
    state.pending_hwi(id)
}

pub(crate) fn inject_enabled_pending_interrupt(
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    id: LoongArchVcpuId,
) -> bool {
    update_guest_interrupts(state, ctx, id);
    let pending = ctx.gcsr_estat & ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK;
    if ctx.gcsr_eentry != 0
        && ctx.gcsr_crmd & super::registers::crmd_interrupt_enable_value() != 0
        && let Some(vector) = decode_interrupt_vector(pending)
    {
        inject_guest_interrupt_at(ctx, vector, get_guest_pc(ctx));
        true
    } else {
        false
    }
}

/// Interprets one whole instruction after backend unload. Validation precedes
/// every register, GPR, PC and notification effect; rejection is an owned exit.
pub(crate) fn emulate_iocsr(
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    _vm_id: LoongArchVmId,
    id: LoongArchVcpuId,
) -> LoongArchVcpuResult<LoongArchVmExit> {
    let ty = extract_field(ins, 10, 3);
    let width = 1 << (ty & 3);
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let address = if rj == 0 { 0 } else { ctx.x[rj] };
    let write = ty >= 4;
    let value = if rd == 0 { 0 } else { ctx.x[rd] };
    let value = if width == 8 {
        value
    } else {
        value & ((1 << (width * 8)) - 1)
    };
    let result = if !(0x19200..=0x19207).contains(&(ins >> 10)) {
        Err(LoongArchIocsrFaultReason::InvalidInstruction)
    } else {
        access::prepare(state, id, address, width, write, value)
    };
    let operation = match result {
        Ok(operation) => operation,
        Err(reason) => {
            return Ok(LoongArchVmExit::EmulationFault(
                LoongArchEmulationFault::Iocsr {
                    pc: get_guest_pc(ctx),
                    instruction: ins,
                    address,
                    direction: if write {
                        LoongArchIocsrDirection::Write
                    } else {
                        LoongArchIocsrDirection::Read
                    },
                    width,
                    reason,
                },
            ));
        }
    };
    let loaded = operation.commit(state);
    if !write && rd != 0 {
        ctx.set_gpr(
            rd,
            if width == 4 {
                (loaded as i32) as isize as usize
            } else {
                loaded
            },
        );
    }
    update_guest_interrupts(state, ctx, id);
    advance_guest_pc(ctx);
    Ok(LoongArchVmExit::Nothing)
}

#[cfg(any(test, axtest))]
mod tests {
    use super::*;
    use crate::{RunId, VmKey, services::RunSignals};

    fn instruction(ty: usize, rd: usize) -> usize {
        ((0x19200 + ty) << 10) | (5 << 5) | rd
    }

    fn access(
        state: &LoongArchIocsrState,
        ctx: &mut LoongArchContextFrame,
        id: usize,
        ty: usize,
        address: usize,
        value: usize,
    ) -> LoongArchVmExit {
        ctx.x[5] = address;
        ctx.x[4] = value;
        emulate_iocsr(state, ctx, instruction(ty, 4), 1, id).unwrap()
    }

    fn write(
        state: &LoongArchIocsrState,
        ctx: &mut LoongArchContextFrame,
        id: usize,
        width: usize,
        address: usize,
        value: usize,
    ) {
        let ty = if width == 8 { 7 } else { 6 };
        assert!(matches!(
            access(state, ctx, id, ty, address, value),
            LoongArchVmExit::Nothing
        ));
    }

    fn read(
        state: &LoongArchIocsrState,
        ctx: &mut LoongArchContextFrame,
        id: usize,
        width: usize,
        address: usize,
    ) -> usize {
        let ty = if width == 8 { 3 } else { 2 };
        assert!(matches!(
            access(state, ctx, id, ty, address, 0),
            LoongArchVmExit::Nothing
        ));
        ctx.x[4]
    }

    fn model_snapshot(state: &LoongArchIocsrState) -> std::vec::Vec<usize> {
        let mut snapshot = std::vec![state.eio_enabled.load(Ordering::Acquire) as usize];
        for vcpu in &state.vcpus {
            snapshot.extend([
                vcpu.ipi_status.load(Ordering::Acquire),
                vcpu.ipi_enable.load(Ordering::Acquire),
            ]);
            for slot in vcpu
                .mail_buf
                .iter()
                .chain(vcpu.eiointc_isr.iter())
                .chain(vcpu.eiointc_nodemap.iter())
                .chain(vcpu.eiointc_ipmap.iter())
                .chain(vcpu.eiointc_enable.iter())
                .chain(vcpu.eiointc_bounce.iter())
                .chain(vcpu.eiointc_coremap.iter())
            {
                snapshot.push(slot.load(Ordering::Acquire));
            }
        }
        snapshot
    }

    fn context_snapshot(ctx: &LoongArchContextFrame) -> std::vec::Vec<usize> {
        let mut snapshot: std::vec::Vec<_> = (0..32).map(|i| ctx.x[i]).collect();
        snapshot.extend([
            ctx.sepc,
            ctx.gcsr_era,
            ctx.host_era,
            ctx.host_tlbrera,
            ctx.gcsr_estat,
        ]);
        snapshot
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn iocsr_pending_gate_and_vm_lifetime() {
        let state = LoongArchIocsrState::new(&[0, 2]).unwrap();
        init_guest_iocsr(&state, 0);
        let mut ctx = LoongArchContextFrame::default();
        ctx.gcsr_era = 0x1000;
        ctx.gcsr_ectl = LOCAL_INTERRUPT_MASK;
        ctx.gcsr_crmd = super::super::registers::crmd_interrupt_enable_value();
        ctx.gcsr_eentry = 0x8000;
        assert_eq!(inject_guest_eiointc_vector(&state, 1, 0, 7), None);
        assert_eq!(read(&state, &mut ctx, 0, 8, EIOINTC_ISR_BASE), 1 << 7);
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK, 0);
        write(&state, &mut ctx, 0, 8, IOCSR_MISC_FUNC, MISC_EIO_ENABLE);
        init_guest_iocsr(&state, 2);
        assert_eq!(
            read(&state, &mut ctx, 2, 4, EXTIOI_VIRT_CONFIG),
            CONFIG_EIO_ENABLE
        );
        assert_eq!(
            guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK,
            1 << INT_HWI0
        );
        write(&state, &mut ctx, 2, 8, IOCSR_MISC_FUNC, 0);
        assert_eq!(
            read(&state, &mut ctx, 0, 8, IOCSR_MISC_FUNC),
            MISC_EIO_ENABLE
        );
        write(&state, &mut ctx, 2, 4, EXTIOI_VIRT_CONFIG, 0);
        ctx.gcsr_estat |= HWI_MASK | super::super::trap::TIMER_BIT;
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK, 0);
        assert_ne!(
            guest_pending_interrupts(&state, &ctx, 0) & super::super::trap::TIMER_BIT,
            0
        );
        write(&state, &mut ctx, 0, 4, LOONGARCH_IOCSR_IPI_SET, 1 << 3);
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & IPI_BIT, 0);
        write(&state, &mut ctx, 0, 4, LOONGARCH_IOCSR_IPI_EN, 1 << 3);
        assert_ne!(guest_pending_interrupts(&state, &ctx, 0) & IPI_BIT, 0);
        write(&state, &mut ctx, 0, 4, LOONGARCH_IOCSR_IPI_CLEAR, 1 << 3);
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & IPI_BIT, 0);
        write(
            &state,
            &mut ctx,
            0,
            4,
            EXTIOI_VIRT_CONFIG,
            CONFIG_EIO_ENABLE,
        );
        assert_eq!(
            guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK,
            1 << INT_HWI0
        );
        write(&state, &mut ctx, 0, 4, EIOINTC_ENABLE_BASE, 0);
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK, 0);
        write(&state, &mut ctx, 0, 4, EIOINTC_ENABLE_BASE, 1 << 7);
        write(&state, &mut ctx, 0, 4, EIOINTC_COREMAP_BASE + 4, 0);
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK, 0);
        write(
            &state,
            &mut ctx,
            0,
            4,
            EIOINTC_COREMAP_BASE + 4,
            0x0101_0101,
        );
        assert_eq!(
            guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK,
            1 << INT_HWI0
        );
        write(&state, &mut ctx, 0, 4, EIOINTC_ISR_COMPAT_BASE, 1 << 7);
        assert_eq!(read(&state, &mut ctx, 0, 8, EIOINTC_ISR_BASE), 0);
        assert_eq!(guest_pending_interrupts(&state, &ctx, 0) & HWI_MASK, 0);

        // Real send instructions target a configured sparse ID and preserve halves.
        write(
            &state,
            &mut ctx,
            2,
            8,
            LOONGARCH_IOCSR_MAIL_BUF0,
            0x1122_3344_5566_7788,
        );
        write(
            &state,
            &mut ctx,
            0,
            8,
            LOONGARCH_IOCSR_MBUF_SEND,
            (0xaabb_ccddusize << 32) | (2 << 16) | (1 << 31) | 4,
        );
        assert_eq!(
            read(&state, &mut ctx, 2, 8, LOONGARCH_IOCSR_MAIL_BUF0),
            0xaabb_ccdd_5566_7788
        );
        assert_eq!(read(&state, &mut ctx, 2, 4, LOONGARCH_IOCSR_IPI_STATUS), 0);
        write(
            &state,
            &mut ctx,
            0,
            8,
            LOONGARCH_IOCSR_ANY_SEND,
            (0x00ee_00ffusize << 32)
                | (1 << 31)
                | (0b1010 << 27)
                | (2 << 16)
                | LOONGARCH_IOCSR_MAIL_BUF0,
        );
        assert_eq!(
            read(&state, &mut ctx, 2, 8, LOONGARCH_IOCSR_MAIL_BUF0),
            0xaabb_ccdd_55ee_77ff
        );
        write(
            &state,
            &mut ctx,
            0,
            4,
            LOONGARCH_IOCSR_IPI_SEND,
            (1 << 31) | (2 << 16) | 3,
        );
        assert_eq!(
            read(&state, &mut ctx, 2, 4, LOONGARCH_IOCSR_IPI_STATUS),
            1 << 3
        );
        write(&state, &mut ctx, 2, 4, LOONGARCH_IOCSR_IPI_EN, 1 << 3);
        assert_ne!(guest_pending_interrupts(&state, &ctx, 2) & IPI_BIT, 0);
        // Word loads sign-extend, and a load into r0 never corrupts it.
        assert_eq!(
            read(&state, &mut ctx, 2, 4, LOONGARCH_IOCSR_MAIL_BUF0 + 4),
            0xffff_ffff_aabb_ccdd
        );
        ctx.x[5] = LOONGARCH_IOCSR_MAIL_BUF0;
        let pc = get_guest_pc(&ctx);
        assert!(matches!(
            emulate_iocsr(&state, &mut ctx, instruction(3, 0), 1, 2).unwrap(),
            LoongArchVmExit::Nothing
        ));
        assert_eq!(ctx.x[0], 0);
        assert_eq!(get_guest_pc(&ctx), pc + 4);
        for (address, width, expected) in [
            (0x8, 4, 0x18),
            (0x10, 8, u64::from_le_bytes(*b"Axvisor\0") as usize),
            (0x20, 8, u64::from_le_bytes(*b"LA64VM\0\0") as usize),
            (EXTIOI_VIRT_FEATURES, 4, 3),
        ] {
            assert_eq!(read(&state, &mut ctx, 0, width, address), expected);
        }

        let other = LoongArchIocsrState::new(&[0]).unwrap();
        init_guest_iocsr(&other, 0);
        assert_eq!(read(&other, &mut ctx, 0, 4, EXTIOI_VIRT_CONFIG), 0);
        assert_eq!(read(&other, &mut ctx, 0, 8, EIOINTC_ISR_BASE), 0);
        // Preparing the same placement set creates another closed, empty period.
        let next_run = LoongArchIocsrState::new(&[0, 2]).unwrap();
        init_guest_iocsr(&next_run, 0);
        assert_eq!(read(&next_run, &mut ctx, 0, 4, EXTIOI_VIRT_CONFIG), 0);
        assert_eq!(
            read(&next_run, &mut ctx, 2, 4, LOONGARCH_IOCSR_IPI_STATUS),
            0
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn iocsr_rejection_preserves_all_effects() {
        let state = LoongArchIocsrState::new(&[0, 2]).unwrap();
        init_guest_iocsr(&state, 0);
        init_guest_iocsr(&state, 2);
        let signals = RunSignals::new(RunId::new(VmKey::new(1, 1), 1), 3).unwrap();
        state.set_run_port(LoongArchRunPort::new(signals.clone()));
        let mut ctx = LoongArchContextFrame::default();
        ctx.gcsr_era = 0x1234;
        write(
            &state,
            &mut ctx,
            0,
            8,
            LOONGARCH_IOCSR_MAIL_BUF0,
            0x0102_0304_0506_0708,
        );
        // The first input catches the old bank split-write and PC retirement.
        let cases = [
            (
                7,
                EIOINTC_NODEMAP_END - 4,
                0xdead_beef_1234_5678,
                LoongArchIocsrFaultReason::Misaligned,
            ),
            (
                7,
                EIOINTC_NODEMAP_END - 8,
                usize::MAX,
                LoongArchIocsrFaultReason::UnsupportedWidth,
            ),
            (
                4,
                LOONGARCH_IOCSR_IPI_EN,
                1,
                LoongArchIocsrFaultReason::UnsupportedWidth,
            ),
            (
                1,
                LOONGARCH_IOCSR_MAIL_BUF0,
                0,
                LoongArchIocsrFaultReason::UnsupportedWidth,
            ),
            (2, 0x408, 0, LoongArchIocsrFaultReason::UnknownAddress),
            (6, 0x1500, 1, LoongArchIocsrFaultReason::UnknownAddress),
            (
                2,
                LOONGARCH_IOCSR_IPI_SET,
                0,
                LoongArchIocsrFaultReason::Permission,
            ),
            (
                6,
                LOONGARCH_IOCSR_IPI_STATUS,
                1,
                LoongArchIocsrFaultReason::Permission,
            ),
            (
                7,
                IOCSR_MISC_FUNC,
                1 << 49,
                LoongArchIocsrFaultReason::ReservedBits,
            ),
            (
                6,
                EXTIOI_VIRT_CONFIG,
                8,
                LoongArchIocsrFaultReason::ReservedBits,
            ),
            (
                6,
                LOONGARCH_IOCSR_IPI_SEND,
                1 << 16,
                LoongArchIocsrFaultReason::InvalidTarget { target: 1 },
            ),
            (
                7,
                LOONGARCH_IOCSR_MBUF_SEND,
                3 << 16,
                LoongArchIocsrFaultReason::InvalidTarget { target: 3 },
            ),
            (
                6,
                LOONGARCH_IOCSR_IPI_SEND,
                1 << 5,
                LoongArchIocsrFaultReason::ReservedBits,
            ),
            (
                7,
                LOONGARCH_IOCSR_ANY_SEND,
                LOONGARCH_IOCSR_ANY_SEND,
                LoongArchIocsrFaultReason::InvalidSendTarget {
                    address: LOONGARCH_IOCSR_ANY_SEND,
                },
            ),
            (
                7,
                LOONGARCH_IOCSR_ANY_SEND,
                EIOINTC_ISR_BASE,
                LoongArchIocsrFaultReason::InvalidSendTarget {
                    address: EIOINTC_ISR_BASE,
                },
            ),
            (
                2,
                LOONGARCH_IOCSR_ANY_SEND,
                0,
                LoongArchIocsrFaultReason::UnsupportedWidth,
            ),
            (6, 0x8, 0, LoongArchIocsrFaultReason::Permission),
        ];
        for (ty, address, value, reason) in cases {
            ctx.x[5] = address;
            ctx.x[4] = value;
            let model_before = model_snapshot(&state);
            let context_before = context_snapshot(&ctx);
            let pc = get_guest_pc(&ctx);
            let ins = instruction(ty, 4);
            let exit = emulate_iocsr(&state, &mut ctx, ins, 1, 0).unwrap();
            assert_eq!(
                model_snapshot(&state),
                model_before,
                "model changed at {address:#x}"
            );
            assert_eq!(
                context_snapshot(&ctx),
                context_before,
                "context changed at {address:#x}"
            );
            assert!(
                (0..3).all(|id| !signals.has_pending(id)),
                "invalid request notified a target"
            );
            assert!(
                matches!(exit, LoongArchVmExit::EmulationFault(LoongArchEmulationFault::Iocsr {
                pc: fault_pc, instruction: raw, address: fault_address, reason: actual, ..
            }) if fault_pc == pc && raw == ins && fault_address == address && actual == reason),
                "{exit:?}"
            );
        }
        ctx.x[5] = LOONGARCH_IOCSR_IPI_EN;
        let before = context_snapshot(&ctx);
        let malformed = instruction(6, 4) | (1 << 13);
        assert!(matches!(
            emulate_iocsr(&state, &mut ctx, malformed, 1, 0).unwrap(),
            LoongArchVmExit::EmulationFault(LoongArchEmulationFault::Iocsr {
                reason: LoongArchIocsrFaultReason::InvalidInstruction,
                ..
            })
        ));
        assert_eq!(context_snapshot(&ctx), before);
        // Notification retirement cannot undo an already successful register write.
        write(&state, &mut ctx, 2, 4, LOONGARCH_IOCSR_IPI_EN, 1 << 5);
        signals.close_interrupts();
        write(
            &state,
            &mut ctx,
            0,
            4,
            LOONGARCH_IOCSR_IPI_SEND,
            (2 << 16) | 5,
        );
        assert_eq!(
            read(&state, &mut ctx, 2, 4, LOONGARCH_IOCSR_IPI_STATUS),
            1 << 5
        );
        assert!((0..3).all(|id| !signals.has_pending(id)));
    }
}
