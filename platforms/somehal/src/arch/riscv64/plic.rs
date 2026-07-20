use alloc::{format, vec, vec::Vec};
use core::{
    num::NonZeroU32,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_riscv_plic::{PLICRegs, Plic, PlicIrqHandler};
use kernutil::StaticCell;
use rdif_intc::Interface;
use rdrive::{
    Device, DriverGeneric, Phandle, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::{FdtInfo, ProbeFdt},
};
use riscv::register::{sie, sip};
use sbi_rt::HartMask;

use crate::{
    common::ioremap,
    irq_routing::{
        RISCV_S_EXT_IRQ, RISCV_S_SOFT_IRQ, RISCV_S_TIMER_IRQ, RiscvTrapIrq, classify_riscv_trap,
        riscv_plic_hwirq_from_source, riscv_source_from_plic_hwirq,
    },
};

const SUPERVISOR_EXTERNAL_INTERRUPT: u32 = 9;
const DEFAULT_PRIORITY: u32 = 1;
const DEFAULT_PLIC_SIZE: usize = 0x400_0000;

static IRQ_HANDLER: StaticCell<RiscvPlicIrqHandler> = StaticCell::uninit();
const GUEST_FORWARDABLE_PLIC_SOURCES: usize = 1024;
static PLIC_CLAIM_READERS: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_PLIC_CLAIMS: [AtomicUsize; GUEST_FORWARDABLE_PLIC_SOURCES] =
    [const { AtomicUsize::new(0) }; GUEST_FORWARDABLE_PLIC_SOURCES];

struct PlicClaimReader;

impl PlicClaimReader {
    fn enter() -> Self {
        PLIC_CLAIM_READERS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for PlicClaimReader {
    fn drop(&mut self) {
        PLIC_CLAIM_READERS.fetch_sub(1, Ordering::Release);
    }
}

module_driver!(
    name: "RISC-V PLIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "riscv,plic0",
            "sifive,plic-1.0.0",
            "starfive,jh7110-plic",
        ],
        on_probe: probe_plic
    }],
);

pub fn systick_irq() -> rdrive::IrqId {
    RISCV_S_TIMER_IRQ.into()
}

pub fn local_irq_set_enable(irq: rdrive::IrqId, enable: bool) -> Result<(), crate::irq::IrqError> {
    let raw: usize = irq.into();
    match raw {
        RISCV_S_TIMER_IRQ => unsafe {
            if enable {
                sie::set_stimer();
            } else {
                sie::clear_stimer();
            }
            Ok(())
        },
        RISCV_S_SOFT_IRQ => unsafe {
            if enable {
                sie::set_ssoft();
            } else {
                sie::clear_ssoft();
            }
            Ok(())
        },
        RISCV_S_EXT_IRQ => unsafe {
            if enable {
                sie::set_sext();
            } else {
                sie::clear_sext();
            }
            Ok(())
        },
        other => {
            warn!("unsupported RISC-V local IRQ {other:#x}");
            Err(crate::irq::IrqError::InvalidIrq)
        }
    }
}

/// Immutable IRQ-side capability for one leased physical PLIC source.
///
/// The control plane validates and enables the source before constructing this
/// value. Its data-plane methods touch only the source-owned priority register:
/// they do not perform a domain lookup, acquire a driver lock, or log.
pub struct RiscvPlicIrqEndpoint {
    handler: PlicIrqHandler,
    source: NonZeroU32,
    restore_priority: u32,
    lease_generation: u64,
}

impl RiscvPlicIrqEndpoint {
    /// Returns the leased physical PLIC source ID.
    pub const fn source(&self) -> u32 {
        self.source.get()
    }

    /// Returns the generation-bearing controller lease identity.
    pub const fn lease_id(&self) -> RiscvPlicLeaseId {
        RiscvPlicLeaseId {
            source: self.source.get(),
            generation: self.lease_generation,
        }
    }

    /// Masks the source through its independent priority register.
    #[inline]
    pub fn mask(&self) {
        self.handler.set_priority(self.source, 0);
        plic_fence_output_to_output();
    }

    /// Restores the spec-required first nonzero PLIC priority.
    #[inline]
    pub fn unmask(&self) {
        // The endpoint, wake target, and ingress route are normal-memory
        // publications. Order them before making the PLIC source observable.
        plic_fence_memory_to_output();
        self.handler
            .set_priority(self.source, self.restore_priority);
        // The caller clears its normal-memory `masked` generation after this
        // returns. Ensure the MMIO restore reaches the PLIC first.
        plic_fence_output_to_memory();
    }
}

/// Value-only identity of one physical PLIC source lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RiscvPlicLeaseId {
    source: u32,
    generation: u64,
}

impl RiscvPlicLeaseId {
    /// Returns the physical PLIC source ID.
    pub const fn source(self) -> u32 {
        self.source
    }

    /// Returns the controller lease generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

#[inline]
fn plic_fence_memory_to_output() {
    // SAFETY: `fence` changes only architectural ordering; it does not access
    // memory or depend on register operands.
    unsafe {
        core::arch::asm!("fence rw, ow", options(nostack, preserves_flags));
    }
}

#[inline]
fn plic_fence_output_to_output() {
    // SAFETY: `fence` changes only architectural ordering; it does not access
    // memory or depend on register operands.
    unsafe {
        core::arch::asm!("fence ow, ow", options(nostack, preserves_flags));
    }
}

#[inline]
fn plic_fence_output_to_memory() {
    // SAFETY: `fence` changes only architectural ordering; it does not access
    // memory or depend on register operands.
    unsafe {
        core::arch::asm!("fence ow, rw", options(nostack, preserves_flags));
    }
}

/// Leases one physical PLIC source for IRQ-side ownership transfer.
///
/// This is a control-plane operation. It resolves the driver, fixes affinity,
/// enables the source's context bits while priority remains zero, records the
/// first nonzero architectural priority, and then rejects subsequent generic
/// affinity/enable changes for the leased source. The caller must publish the
/// endpoint and its consumer before calling [`RiscvPlicIrqEndpoint::unmask`].
pub fn lease_irq_endpoint(
    hwirq: rdif_intc::HwIrq,
    affinity: crate::irq::IrqAffinity,
) -> Result<RiscvPlicIrqEndpoint, crate::irq::IrqError> {
    let mut endpoints = lease_irq_endpoints(core::slice::from_ref(&hwirq), affinity)?;
    Ok(endpoints
        .pop()
        .expect("one validated PLIC source must produce one endpoint"))
}

/// Atomically leases a set of physical PLIC sources for one fixed owner.
///
/// The controller lock is held across validation and commit. Every source,
/// duplicate, target context, and existing lease is validated before affinity,
/// enable, or lease ownership is changed for any member. Live sources are not
/// probed with a transient nonzero priority during validation.
pub fn lease_irq_endpoints(
    hwirqs: &[rdif_intc::HwIrq],
    affinity: crate::irq::IrqAffinity,
) -> Result<Vec<RiscvPlicIrqEndpoint>, crate::irq::IrqError> {
    with_plic("leasing PLIC IRQ endpoint batch", |plic| {
        plic.lease_irq_endpoints(hwirqs, affinity)
    })
    .ok_or(crate::irq::IrqError::Controller)?
}

/// Atomically masks, detaches, and releases a complete PLIC lease batch.
///
/// Validation happens before any controller state changes. A stale generation,
/// duplicate source, or partial batch therefore cannot release a newer owner.
pub fn release_irq_endpoints(leases: &[RiscvPlicLeaseId]) -> Result<(), crate::irq::IrqError> {
    let intc = get_plic().ok_or(crate::irq::IrqError::Controller)?;
    let mut intc = intc.try_lock().map_err(|_| crate::irq::IrqError::Busy)?;
    let plic = intc
        .typed_mut::<RiscvPlic>()
        .ok_or(crate::irq::IrqError::Controller)?;
    plic.release_irq_endpoints(leases)
}

enum Completion {
    None,
    Plic { source: NonZeroU32, tracked: bool },
}

pub struct ActiveIrq {
    irq: rdrive::IrqId,
    completion: Completion,
}

impl ActiveIrq {
    pub fn id(&self) -> rdrive::IrqId {
        self.irq
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        if let Completion::Plic { source, tracked } = self.completion {
            complete_external_irq_source(source);
            if tracked {
                ACTIVE_PLIC_CLAIMS[source.get() as usize].fetch_sub(1, Ordering::Release);
            }
        }
    }
}

pub fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    match classify_riscv_trap(raw) {
        RiscvTrapIrq::Timer => Some(ActiveIrq {
            irq: RISCV_S_TIMER_IRQ.into(),
            completion: Completion::None,
        }),
        RiscvTrapIrq::Ipi => {
            unsafe {
                sip::clear_ssoft();
            }
            Some(ActiveIrq {
                irq: RISCV_S_SOFT_IRQ.into(),
                completion: Completion::None,
            })
        }
        RiscvTrapIrq::External => begin_external_irq(),
        RiscvTrapIrq::UnknownInterrupt { cause } => {
            warn!("unsupported RISC-V interrupt cause {cause}");
            None
        }
        RiscvTrapIrq::BareSource(source) => {
            warn!("ignore bare RISC-V PLIC source {source} outside external interrupt claim path");
            None
        }
    }
}

fn begin_external_irq() -> Option<ActiveIrq> {
    let (source, tracked) = claim_external_irq_source()?;
    Some(ActiveIrq {
        irq: (source.get() as usize).into(),
        completion: Completion::Plic { source, tracked },
    })
}

fn complete_external_irq_source(source: NonZeroU32) {
    if let Some(handler) = get_irq_handler() {
        handler.complete_current(source);
    } else {
        warn!("RISC-V PLIC IRQ handler is not registered when completing external IRQ");
    }
}

pub fn secondary_init_intc(cpu_idx: usize) {
    if let Some(handler) = get_irq_handler() {
        handler.init_context(cpu_idx);
    }
    enable_local_interrupts();
}

pub(super) fn send_ipi_to_cpu(cpu: irq_framework::CpuId) -> crate::irq::IpiSendStatus {
    let Some(hart_id) = checked_ipi_hart_id(cpu) else {
        return crate::irq::IpiSendStatus::Invalid;
    };
    send_ipi_to_hart(hart_id)
}

pub(super) fn checked_ipi_hart_id(cpu: irq_framework::CpuId) -> Option<usize> {
    let hart_id = crate::cpu::runtime_cpu_target(cpu)?.as_usize();
    // `usize::MAX` is SBI's special all-harts base and cannot represent one
    // physical target in a single-bit HartMask.
    (hart_id != HartMask::IGNORE_MASK).then_some(hart_id)
}

fn send_ipi_to_hart(hart_id: usize) -> crate::irq::IpiSendStatus {
    publish_before_sbi_ipi();
    let response = sbi_rt::send_ipi(HartMask::from_mask_base(1, hart_id));
    ipi_status_from_sbi_error(response.error)
}

#[inline]
fn publish_before_sbi_ipi() {
    // Make normal-memory inbox publication globally observable before the SBI
    // implementation asserts the remote software interrupt.
    // SAFETY: `fence` changes ordering only and has no register operands.
    unsafe { core::arch::asm!("fence rw, rw", options(nostack, preserves_flags)) }
}

const SBI_SUCCESS: usize = 0;
const SBI_ERR_FAILED: usize = (-1isize) as usize;
const SBI_ERR_TIMEOUT: usize = (-12isize) as usize;
const SBI_ERR_IO: usize = (-13isize) as usize;

const fn ipi_status_from_sbi_error(error: usize) -> crate::irq::IpiSendStatus {
    match error {
        SBI_SUCCESS => crate::irq::IpiSendStatus::Success,
        // These failures may clear on a later bounded retry. Every validation,
        // policy, state, unsupported, and custom error is permanent for this
        // request and must converge to Invalid instead of spinning forever.
        SBI_ERR_FAILED | SBI_ERR_TIMEOUT | SBI_ERR_IO => crate::irq::IpiSendStatus::Retry,
        _ => crate::irq::IpiSendStatus::Invalid,
    }
}

#[cfg(test)]
mod ipi_tests {
    use super::*;

    #[test]
    fn classifies_transient_and_permanent_sbi_errors() {
        assert_eq!(
            ipi_status_from_sbi_error(SBI_SUCCESS),
            crate::irq::IpiSendStatus::Success
        );
        assert_eq!(
            ipi_status_from_sbi_error(SBI_ERR_FAILED),
            crate::irq::IpiSendStatus::Retry
        );
        assert_eq!(
            ipi_status_from_sbi_error(SBI_ERR_TIMEOUT),
            crate::irq::IpiSendStatus::Retry
        );
        assert_eq!(
            ipi_status_from_sbi_error(SBI_ERR_IO),
            crate::irq::IpiSendStatus::Retry
        );
        for permanent in -2isize..=-11 {
            assert_eq!(
                ipi_status_from_sbi_error(permanent as usize),
                crate::irq::IpiSendStatus::Invalid,
            );
        }
        assert_eq!(
            ipi_status_from_sbi_error((-14isize) as usize),
            crate::irq::IpiSendStatus::Invalid
        );
        assert_eq!(
            ipi_status_from_sbi_error(usize::MAX / 2),
            crate::irq::IpiSendStatus::Invalid
        );
    }
}

fn probe_plic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let mmio = ioremap(
        reg.address,
        reg.size.unwrap_or(DEFAULT_PLIC_SIZE as u64) as usize,
    )
    .map_err(|err| OnProbeError::other(format!("failed to map PLIC: {err:?}")))?;
    let plic = unsafe {
        Plic::new(
            NonNull::new(mmio.as_ptr() as *mut PLICRegs)
                .ok_or_else(|| OnProbeError::other("PLIC MMIO mapping is null"))?,
        )
    };
    let ndev = info
        .node
        .as_node()
        .get_property("riscv,ndev")
        .and_then(|prop| prop.get_u32())
        .unwrap_or(1024) as usize;
    let mut plic = plic;
    plic.disable_all_sources(ndev);
    let contexts = parse_supervisor_contexts(&info);
    for context in contexts.iter().filter_map(|context| *context) {
        plic.disable_context_sources(context);
    }

    let irq_handler = RiscvPlicIrqHandler {
        inner: plic.irq_handler(),
        context_by_cpu: contexts.clone(),
    };
    IRQ_HANDLER.init(irq_handler);
    if let Some(handler) = get_irq_handler() {
        handler.reset_all_contexts();
    }
    let plic = RiscvPlic {
        inner: plic,
        context_by_cpu: contexts,
        affinity_by_source: vec![crate::irq::IrqAffinity::Any; ndev.saturating_add(1)],
        enabled_by_source: vec![false; ndev.saturating_add(1)],
        leased_by_source: vec![false; ndev.saturating_add(1)],
        lease_generation_by_source: vec![0; ndev.saturating_add(1)],
        sources: ndev,
    };
    enable_local_interrupts();

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::RiscvPlic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register PLIC domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, plic));
    Ok(())
}

fn parse_supervisor_contexts(info: &FdtInfo<'_>) -> Vec<Option<usize>> {
    let mut contexts = Vec::new();
    let Some(prop) = info.node.as_node().get_property("interrupts-extended") else {
        return contexts;
    };

    let mut reader = prop.as_reader();
    let mut context = 0;
    while let (Some(phandle), Some(interrupt)) = (reader.read_u32(), reader.read_u32()) {
        if interrupt == SUPERVISOR_EXTERNAL_INTERRUPT
            && let Some(cpu_idx) = cpu_idx_from_intc_phandle(info, Phandle::from(phandle))
        {
            if contexts.len() <= cpu_idx {
                contexts.resize(cpu_idx + 1, None);
            }
            contexts[cpu_idx] = Some(context);
        }
        context += 1;
    }
    contexts
}

fn cpu_idx_from_intc_phandle(info: &FdtInfo<'_>, phandle: Phandle) -> Option<usize> {
    let intc = info.get_by_phandle(phandle)?;
    if let Some(cpu_idx) = intc.parent().and_then(|cpu| cpu_idx_from_cpu_node(&cpu)) {
        return Some(cpu_idx);
    }
    let cpu = info.get_by_phandle(intc.as_node().interrupt_parent()?)?;
    cpu_idx_from_cpu_node(&cpu)
}

fn cpu_idx_from_cpu_node(cpu: &NodeType<'_>) -> Option<usize> {
    let hart_id = cpu.regs().first()?.address as usize;
    someboot::smp::cpu_id_to_idx(hart_id)
}

fn enable_local_interrupts() {
    unsafe {
        sie::set_ssoft();
        sie::set_stimer();
        sie::set_sext();
    }
}

fn claim_external_irq_source() -> Option<(NonZeroU32, bool)> {
    // The controller release path first masks its sources, then observes this
    // reader count. A claim that could have read an old eligible source is
    // therefore visible before the lease can become reusable.
    let _reader = PlicClaimReader::enter();
    let Some(handler) = get_irq_handler() else {
        warn!("RISC-V PLIC IRQ handler is not registered for external IRQ");
        return None;
    };
    let source = handler.claim_current()?;
    let tracked = (source.get() as usize) < GUEST_FORWARDABLE_PLIC_SOURCES;
    if tracked {
        ACTIVE_PLIC_CLAIMS[source.get() as usize].fetch_add(1, Ordering::AcqRel);
    }
    Some((source, tracked))
}

fn with_plic<R>(op: &str, f: impl FnOnce(&mut RiscvPlic) -> R) -> Option<R> {
    let Some(intc) = get_plic() else {
        warn!("RISC-V PLIC is not registered when {op}");
        return None;
    };
    let Ok(mut intc) = intc.lock() else {
        warn!("failed to lock RISC-V PLIC when {op}");
        return None;
    };
    let Some(plic) = intc.typed_mut::<RiscvPlic>() else {
        warn!("registered interrupt controller is not RISC-V PLIC when {op}");
        return None;
    };
    Some(f(plic))
}

fn get_plic() -> Option<Device<rdif_intc::Intc>> {
    if !rdrive::is_initialized() {
        return None;
    }
    rdrive::get_one()
}

fn get_irq_handler() -> Option<&'static RiscvPlicIrqHandler> {
    if IRQ_HANDLER.is_init() {
        Some(&IRQ_HANDLER)
    } else {
        None
    }
}

struct RiscvPlic {
    inner: Plic,
    context_by_cpu: Vec<Option<usize>>,
    affinity_by_source: Vec<crate::irq::IrqAffinity>,
    enabled_by_source: Vec<bool>,
    leased_by_source: Vec<bool>,
    lease_generation_by_source: Vec<u64>,
    sources: usize,
}

struct RiscvPlicIrqHandler {
    inner: PlicIrqHandler,
    context_by_cpu: Vec<Option<usize>>,
}

impl RiscvPlicIrqHandler {
    fn current_context(&self) -> Option<usize> {
        current_context(&self.context_by_cpu)
    }

    fn init_context(&self, cpu_idx: usize) {
        if let Some(context) = self.context_by_cpu.get(cpu_idx).and_then(|ctx| *ctx) {
            self.init_context_by_context_id(context);
        } else {
            warn!("PLIC supervisor context for logical CPU {cpu_idx} is not found");
        }
    }

    fn init_context_by_context_id(&self, context: usize) {
        self.inner.init_by_context(context);
        trace!("PLIC context {context} initialized");
    }

    fn reset_all_contexts(&self) {
        for context in self.context_by_cpu.iter().filter_map(|context| *context) {
            self.reset_context_by_context_id(context);
        }
    }

    fn reset_context_by_context_id(&self, context: usize) {
        self.inner.reset_context(context);
        trace!("PLIC context {context} reset");
    }

    fn claim_current(&self) -> Option<NonZeroU32> {
        let Some(context) = self.current_context() else {
            warn_missing_current_context();
            return None;
        };
        let Some(source) = self.inner.claim(context) else {
            debug!("Spurious external IRQ");
            return None;
        };
        Some(source)
    }

    fn complete_current(&self, source: NonZeroU32) {
        let Some(context) = self.current_context() else {
            warn_missing_current_context();
            return;
        };
        self.inner.complete(context, source);
    }
}

impl RiscvPlic {
    fn hwirq_from_source(&self, source: usize) -> Result<rdif_intc::HwIrq, crate::irq::IrqError> {
        riscv_plic_hwirq_from_source(source, self.sources)
    }

    fn source_from_hwirq(&self, hwirq: rdif_intc::HwIrq) -> Result<usize, crate::irq::IrqError> {
        riscv_source_from_plic_hwirq(hwirq, self.sources)
    }

    fn enable_source(&mut self, source: NonZeroU32) -> Result<(), crate::irq::IrqError> {
        if source.get() as usize > self.sources {
            warn!("skip enabling out-of-range PLIC source {}", source.get());
            return Err(crate::irq::IrqError::InvalidIrq);
        }
        self.enabled_by_source[source.get() as usize] = true;
        self.inner.set_priority(source, DEFAULT_PRIORITY);
        let current = current_context(&self.context_by_cpu);
        for context in self.contexts_for_source(source) {
            self.inner.enable(source, context);
        }
        if current.is_none() {
            warn_missing_current_context();
        }
        Ok(())
    }

    fn disable_source(&mut self, source: NonZeroU32) -> Result<(), crate::irq::IrqError> {
        if source.get() as usize > self.sources {
            warn!("skip disabling out-of-range PLIC source {}", source.get());
            return Err(crate::irq::IrqError::InvalidIrq);
        }
        self.enabled_by_source[source.get() as usize] = false;
        self.disable_source_contexts(source);
        Ok(())
    }

    fn disable_source_contexts(&mut self, source: NonZeroU32) {
        for context in self.context_by_cpu.iter().filter_map(|context| *context) {
            self.inner.disable(source, context);
        }
    }

    fn lease_irq_endpoints(
        &mut self,
        hwirqs: &[rdif_intc::HwIrq],
        affinity: crate::irq::IrqAffinity,
    ) -> Result<Vec<RiscvPlicIrqEndpoint>, crate::irq::IrqError> {
        if let crate::irq::IrqAffinity::Fixed { cpu_id } = affinity
            && self
                .context_by_cpu
                .get(cpu_id)
                .and_then(|context| *context)
                .is_none()
        {
            return Err(crate::irq::IrqError::InvalidIrq);
        }

        let handler = self.inner.irq_handler();
        let mut prepared = Vec::with_capacity(hwirqs.len());
        let mut endpoints = Vec::with_capacity(hwirqs.len());
        for &hwirq in hwirqs {
            let source = NonZeroU32::new(self.source_from_hwirq(hwirq)? as u32)
                .ok_or(crate::irq::IrqError::InvalidIrq)?;
            if self.leased_by_source[source.get() as usize] || prepared.contains(&source) {
                return Err(crate::irq::IrqError::Busy);
            }
            prepared.push(source);
        }

        // PLIC priority zero is architecturally reserved for "never interrupt"
        // and every implemented source supports the first nonzero priority.
        // Do not probe a live host source by transiently writing a nonzero
        // priority before its context enables are disabled.
        for &source in &prepared {
            handler.set_priority(source, 0);
        }
        plic_fence_output_to_output();

        // All remaining operations are infallible controller writes under the
        // same lock. Keep priority zero throughout the commit, publish every
        // immutable endpoint, and let the caller activate only after its
        // software route and wake target are visible.
        for source in prepared {
            self.disable_source_contexts(source);
            self.affinity_by_source[source.get() as usize] = affinity;
            self.enabled_by_source[source.get() as usize] = true;
            match affinity {
                crate::irq::IrqAffinity::Any => {
                    for context in self.context_by_cpu.iter().filter_map(|context| *context) {
                        self.inner.enable(source, context);
                    }
                }
                crate::irq::IrqAffinity::Fixed { cpu_id } => {
                    let context = self.context_by_cpu[cpu_id]
                        .expect("fixed PLIC affinity was validated before commit");
                    self.inner.enable(source, context);
                }
            }
            self.leased_by_source[source.get() as usize] = true;
            let lease_generation =
                next_lease_generation(self.lease_generation_by_source[source.get() as usize]);
            self.lease_generation_by_source[source.get() as usize] = lease_generation;
            endpoints.push(RiscvPlicIrqEndpoint {
                handler,
                source,
                restore_priority: DEFAULT_PRIORITY,
                lease_generation,
            });
        }
        plic_fence_output_to_output();
        Ok(endpoints)
    }

    fn release_irq_endpoints(
        &mut self,
        leases: &[RiscvPlicLeaseId],
    ) -> Result<(), crate::irq::IrqError> {
        let mut prepared = Vec::with_capacity(leases.len());
        for lease in leases {
            let source = NonZeroU32::new(lease.source).ok_or(crate::irq::IrqError::InvalidIrq)?;
            let source_index = source.get() as usize;
            if source_index > self.sources
                || source_index >= GUEST_FORWARDABLE_PLIC_SOURCES
                || prepared.contains(&source)
                || !self.leased_by_source[source_index]
                || self.lease_generation_by_source[source_index] != lease.generation
            {
                return Err(crate::irq::IrqError::Busy);
            }
            prepared.push(source);
        }

        let handler = self.inner.irq_handler();
        for &source in &prepared {
            handler.set_priority(source, 0);
        }
        plic_fence_output_to_output();

        if PLIC_CLAIM_READERS.load(Ordering::Acquire) != 0
            || prepared.iter().any(|source| {
                ACTIVE_PLIC_CLAIMS[source.get() as usize].load(Ordering::Acquire) != 0
            })
        {
            // Priority zero remains as a fail-closed quarantine. The caller
            // retries from task context after every old claim has completed.
            return Err(crate::irq::IrqError::Busy);
        }

        for source in prepared {
            let source_index = source.get() as usize;
            self.disable_source_contexts(source);
            self.enabled_by_source[source_index] = false;
            self.affinity_by_source[source_index] = crate::irq::IrqAffinity::Any;
            self.leased_by_source[source_index] = false;
        }
        plic_fence_output_to_output();
        Ok(())
    }

    fn contexts_for_source(&self, source: NonZeroU32) -> Vec<usize> {
        match self.affinity_by_source[source.get() as usize] {
            crate::irq::IrqAffinity::Any => {
                self.context_by_cpu.iter().filter_map(|ctx| *ctx).collect()
            }
            crate::irq::IrqAffinity::Fixed { cpu_id } => self
                .context_by_cpu
                .get(cpu_id)
                .and_then(|ctx| *ctx)
                .into_iter()
                .collect(),
        }
    }
}

const fn next_lease_generation(current: u64) -> u64 {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

fn current_context(context_by_cpu: &[Option<usize>]) -> Option<usize> {
    let cpu_idx = crate::cpu::current_cpu_idx()?;
    context_by_cpu.get(cpu_idx).and_then(|ctx| *ctx)
}

fn warn_missing_current_context() {
    if let Some(cpu_idx) = crate::cpu::current_cpu_idx() {
        warn!("PLIC supervisor context for logical CPU {cpu_idx} is not found");
    } else {
        warn!("PLIC supervisor context for current logical CPU is not found");
    }
}

pub fn source_from_hwirq(hwirq: rdif_intc::HwIrq) -> Result<usize, crate::irq::IrqError> {
    with_plic("validating PLIC hardware IRQ", |plic| {
        plic.source_from_hwirq(hwirq)
    })
    .ok_or(crate::irq::IrqError::Controller)?
}

impl DriverGeneric for RiscvPlic {
    fn name(&self) -> &str {
        "RISC-V PLIC"
    }
}

impl Interface for RiscvPlic {
    fn translate_fdt(
        &self,
        irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        let Some(source) = irq_prop.first().copied() else {
            warn!("empty PLIC interrupt specifier");
            return Err(rdif_intc::IrqError::InvalidIrq);
        };
        Ok(rdif_intc::ControllerIrqTranslation::new(
            self.hwirq_from_source(source as usize)?,
        ))
    }

    fn set_enabled(
        &mut self,
        hwirq: rdif_intc::HwIrq,
        enabled: bool,
    ) -> Result<(), rdif_intc::IrqError> {
        let source = NonZeroU32::new(self.source_from_hwirq(hwirq)? as u32)
            .ok_or(rdif_intc::IrqError::InvalidIrq)?;
        if self.leased_by_source[source.get() as usize] {
            return Err(rdif_intc::IrqError::Busy);
        }
        if enabled {
            self.enable_source(source)
        } else {
            self.disable_source(source)
        }
    }
}
