use alloc::{format, vec, vec::Vec};
use core::{num::NonZeroU32, ptr::NonNull};

use ax_riscv_plic::{PLICRegs, Plic, PlicIrqHandler};
use kernutil::StaticCell;
use rdif_intc::Interface;
use rdrive::{
    Device, DriverGeneric, Phandle, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::{FdtInfo, ProbeFdt},
};
use riscv::register::{sie, sip, siselect};
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
const IMSIC_EIDELIVERY: usize = 0x70;
const IMSIC_EITHRESHOLD: usize = 0x72;
const IMSIC_EIE0: usize = 0xc0;
const IMSIC_EIX_BITS: usize = 32;
const IMSIC_ENABLE_EIDELIVERY: usize = 1;
const IMSIC_ENABLE_EITHRESHOLD: usize = 0;
const IMSIC_TOPEI_ID_SHIFT: usize = 16;
const IMSIC_TOPEI_ID_MASK: usize = 0x7ff;
const APLIC_DOMAINCFG: usize = 0x0000;
const APLIC_DOMAINCFG_IE: u32 = 1 << 8;
const APLIC_DOMAINCFG_DM: u32 = 1 << 2;
const APLIC_SOURCECFG_BASE: usize = 0x0004;
const APLIC_SOURCECFG_SM_INACTIVE: u32 = 0;
const APLIC_SOURCECFG_SM_EDGE_RISE: u32 = 4;
const APLIC_SOURCECFG_SM_EDGE_FALL: u32 = 5;
const APLIC_SOURCECFG_SM_LEVEL_HIGH: u32 = 6;
const APLIC_SOURCECFG_SM_LEVEL_LOW: u32 = 7;
const APLIC_SMSICFGADDR: usize = 0x1bc8;
const APLIC_SMSICFGADDRH: usize = 0x1bcc;
const APLIC_SMSICFGADDRH_LHXW_SHIFT: u32 = 12;
const APLIC_SMSICFGADDRH_LHXS_SHIFT: u32 = 20;
const APLIC_CLRIE_BASE: usize = 0x1f00;
const APLIC_SETIENUM: usize = 0x1edc;
const APLIC_CLRIENUM: usize = 0x1fdc;
const APLIC_SETIPNUM_LE: usize = 0x2000;
const APLIC_TARGET_BASE: usize = 0x3004;
const APLIC_TARGET_HART_IDX_SHIFT: u32 = 18;
const APLIC_TARGET_GUEST_IDX_SHIFT: u32 = 12;
const APLIC_TARGET_EIID_MASK: u32 = 0x7ff;
const IRQ_TYPE_EDGE_RISING: u32 = 1;
const IRQ_TYPE_EDGE_FALLING: u32 = 2;
const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
const IRQ_TYPE_LEVEL_LOW: u32 = 8;

static IRQ_HANDLER: StaticCell<RiscvPlicIrqHandler> = StaticCell::uninit();
static IMSIC: StaticCell<RiscvImsic> = StaticCell::uninit();
static APLIC_HANDLER: StaticCell<RiscvAplicIrqHandler> = StaticCell::uninit();

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

module_driver!(
    name: "RISC-V IMSIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["riscv,imsics", "spacemit,k3-imsics"],
        on_probe: probe_imsic
    }],
);

module_driver!(
    name: "RISC-V APLIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["riscv,aplic", "spacemit,k3-aplic"],
        on_probe: probe_aplic
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

pub fn irq_set_affinity(
    hwirq: rdif_intc::HwIrq,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), crate::irq::IrqError> {
    let source = NonZeroU32::new(hwirq.0).ok_or(crate::irq::IrqError::InvalidIrq)?;
    with_plic("setting PLIC IRQ affinity", |plic| {
        plic.set_source_affinity(source, affinity)
    })
    .flatten()
    .ok_or(crate::irq::IrqError::InvalidIrq)
}

pub fn aplic_irq_set_affinity(
    hwirq: rdif_intc::HwIrq,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), crate::irq::IrqError> {
    let source = NonZeroU32::new(hwirq.0).ok_or(crate::irq::IrqError::InvalidIrq)?;
    with_aplic("setting APLIC IRQ affinity", |aplic| {
        aplic.set_source_affinity(source, affinity)
    })
    .flatten()
    .ok_or(crate::irq::IrqError::InvalidIrq)
}

enum Completion {
    None,
    Plic(PlicClaim),
    Aplic(AplicClaim),
}

enum ActiveIrqSource {
    Legacy(rdrive::IrqId),
    Framework(crate::irq::IrqId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PlicClaim {
    context: usize,
    source: NonZeroU32,
}

impl PlicClaim {
    const fn new(context: usize, source: NonZeroU32) -> Self {
        Self { context, source }
    }

    pub(super) const fn into_parts(self) -> (usize, NonZeroU32) {
        (self.context, self.source)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AplicClaim {
    source: NonZeroU32,
}

impl AplicClaim {
    const fn new(source: NonZeroU32) -> Self {
        Self { source }
    }
}

pub struct ActiveIrq {
    irq: ActiveIrqSource,
    completion: Completion,
}

impl ActiveIrq {
    pub fn id(&self) -> rdrive::IrqId {
        match self.irq {
            ActiveIrqSource::Legacy(irq) => irq,
            ActiveIrqSource::Framework(irq) => (irq.hwirq.0 as usize).into(),
        }
    }

    pub(super) const fn framework_id(&self) -> Option<crate::irq::IrqId> {
        match self.irq {
            ActiveIrqSource::Legacy(_) => None,
            ActiveIrqSource::Framework(irq) => Some(irq),
        }
    }

    pub(super) fn take_plic_claim(&mut self) -> Option<PlicClaim> {
        match core::mem::replace(&mut self.completion, Completion::None) {
            Completion::None => None,
            Completion::Plic(claim) => Some(claim),
            Completion::Aplic(claim) => {
                self.completion = Completion::Aplic(claim);
                None
            }
        }
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        if let Some(claim) = self.take_plic_claim() {
            complete_external_irq_claim(claim);
        } else if let Completion::Aplic(claim) =
            core::mem::replace(&mut self.completion, Completion::None)
        {
            complete_aplic_irq(claim);
        }
    }
}

pub fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    match classify_riscv_trap(raw) {
        RiscvTrapIrq::Timer => Some(ActiveIrq {
            irq: ActiveIrqSource::Legacy(RISCV_S_TIMER_IRQ.into()),
            completion: Completion::None,
        }),
        RiscvTrapIrq::Ipi => {
            unsafe {
                sip::clear_ssoft();
            }
            Some(ActiveIrq {
                irq: ActiveIrqSource::Legacy(RISCV_S_SOFT_IRQ.into()),
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
    if get_irq_handler().is_some()
        && let Some(claim) = claim_external_irq()
    {
        let (_, source) = claim.into_parts();
        return Some(ActiveIrq {
            irq: ActiveIrqSource::Legacy((source.get() as usize).into()),
            completion: Completion::Plic(claim),
        });
    }

    let claim = claim_aplic_external_irq()?;
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvAplic)?;
    Some(ActiveIrq {
        irq: ActiveIrqSource::Framework(crate::irq::IrqId::new(
            domain,
            crate::irq::HwIrq(claim.source.get()),
        )),
        completion: Completion::Aplic(claim),
    })
}

fn complete_external_irq_claim(claim: PlicClaim) {
    if let Some(handler) = get_irq_handler() {
        handler.complete_claim(claim);
    } else {
        warn!("RISC-V PLIC IRQ handler is not registered when completing external IRQ");
    }
}

pub fn secondary_init_intc(cpu_idx: usize) {
    if let Some(handler) = get_irq_handler() {
        handler.init_context(cpu_idx);
    }
    if IMSIC.is_init() {
        init_current_imsic();
    }
    enable_local_interrupts();
}

pub fn send_ipi_to_cpu(cpu_id: usize) -> Result<(), crate::irq::IrqError> {
    let hart_id = someboot::smp::cpu_idx_to_id(cpu_id).ok_or(crate::irq::IrqError::InvalidCpu)?;
    // An SBI IPI is only a doorbell. Complete the shared-memory publication
    // before entering firmware, whose later MMIO/IMSIC operation may otherwise
    // become visible to the target hart first under RVWMO.
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack, preserves_flags));
    }
    let res = sbi_rt::send_ipi(HartMask::from_mask_base(1, hart_id));
    if res.is_ok() {
        Ok(())
    } else {
        Err(crate::irq::IrqError::Controller)
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

fn probe_imsic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    if IMSIC.is_init() {
        info!("skip additional IMSIC node {}", info.node.name());
        return Ok(());
    }
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let hart_index_bits = fdt_u32(info, "riscv,hart-index-bits").unwrap_or(0);
    let guest_index_bits = fdt_u32(info, "riscv,guest-index-bits").unwrap_or(0);
    let num_ids = fdt_u32(info, "riscv,num-ids").unwrap_or(256) as usize;
    IMSIC.init(RiscvImsic {
        base: reg.address,
        hart_index_bits,
        guest_index_bits,
        num_ids,
    });
    init_current_imsic();
    enable_local_interrupts();

    info!(
        "RISC-V IMSIC registered: base={:#x}, hart_index_bits={}, guest_index_bits={}, num_ids={}",
        reg.address, hart_index_bits, guest_index_bits, num_ids
    );
    Ok(())
}

fn probe_aplic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let mmio = ioremap(reg.address, reg.size.unwrap_or(0x4000) as usize)
        .map_err(|err| OnProbeError::other(format!("failed to map APLIC: {err:?}")))?;
    let base = NonNull::new(mmio.as_ptr())
        .ok_or_else(|| OnProbeError::other("APLIC MMIO mapping is null"))?;
    let sources = fdt_u32(&info, "riscv,num-sources").unwrap_or(1024) as usize;
    let imsic =
        get_imsic().ok_or_else(|| OnProbeError::other("APLIC MSI mode requires a probed IMSIC"))?;
    let aplic = unsafe { RiscvAplic::new(base, sources, imsic) };
    aplic.init_global();
    aplic.disable_all_sources();
    APLIC_HANDLER.init(RiscvAplicIrqHandler { base });

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::RiscvAplic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register APLIC domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, aplic));
    info!(
        "RISC-V APLIC registered: node={}, base={:#x}, sources={}",
        info.node.name(),
        reg.address,
        sources
    );
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

fn fdt_u32(info: &FdtInfo<'_>, name: &str) -> Option<u32> {
    info.node.as_node().get_property(name)?.get_u32()
}

fn get_imsic() -> Option<&'static RiscvImsic> {
    if IMSIC.is_init() { Some(&IMSIC) } else { None }
}

fn enable_local_interrupts() {
    unsafe {
        sie::set_ssoft();
        sie::set_stimer();
        sie::set_sext();
    }
}

fn claim_external_irq() -> Option<PlicClaim> {
    let Some(handler) = get_irq_handler() else {
        warn!("RISC-V PLIC IRQ handler is not registered for external IRQ");
        return None;
    };
    handler.claim_current()
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

    fn claim_current(&self) -> Option<PlicClaim> {
        let Some(context) = self.current_context() else {
            warn_missing_current_context();
            return None;
        };
        let Some(source) = self.inner.claim(context) else {
            debug!("Spurious external IRQ");
            return None;
        };
        Some(PlicClaim::new(context, source))
    }

    fn complete_claim(&self, claim: PlicClaim) {
        self.inner.complete(claim.context, claim.source);
    }
}

pub(super) fn complete_deferred_claim(context: usize, source: NonZeroU32) -> bool {
    let Some(handler) = get_irq_handler() else {
        warn!("RISC-V PLIC IRQ handler is not registered when completing a deferred claim");
        return false;
    };
    handler.complete_claim(PlicClaim::new(context, source));
    true
}

fn init_current_imsic() {
    unsafe {
        imsic_write(IMSIC_EITHRESHOLD, IMSIC_ENABLE_EITHRESHOLD);
        imsic_write(IMSIC_EIDELIVERY, IMSIC_ENABLE_EIDELIVERY);
    }
}

fn claim_aplic_external_irq() -> Option<AplicClaim> {
    if !APLIC_HANDLER.is_init() {
        return None;
    }
    let source = unsafe { imsic_claim() }?;
    Some(AplicClaim::new(source))
}

fn complete_aplic_irq(claim: AplicClaim) {
    if APLIC_HANDLER.is_init() {
        APLIC_HANDLER.complete(claim);
    } else {
        warn!("RISC-V APLIC IRQ handler is not registered when completing external IRQ");
    }
}

fn with_aplic<R>(op: &str, f: impl FnOnce(&mut RiscvAplic) -> R) -> Option<R> {
    let Some(domain) = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvAplic)
    else {
        warn!("RISC-V APLIC is not registered when {op}");
        return None;
    };
    let Ok(intc) = crate::irq::intc_by_domain(domain) else {
        warn!("failed to find RISC-V APLIC controller when {op}");
        return None;
    };
    let Ok(mut intc) = intc.lock() else {
        warn!("failed to lock RISC-V APLIC when {op}");
        return None;
    };
    let Some(aplic) = intc.typed_mut::<RiscvAplic>() else {
        warn!("registered interrupt controller is not RISC-V APLIC when {op}");
        return None;
    };
    Some(f(aplic))
}

struct RiscvImsic {
    base: u64,
    hart_index_bits: u32,
    guest_index_bits: u32,
    num_ids: usize,
}

struct RiscvAplic {
    base: NonNull<u8>,
    sources: usize,
    imsic_base: u64,
    hart_index_bits: u32,
    guest_index_bits: u32,
    affinity_by_source: Vec<crate::irq::IrqAffinity>,
    enabled_by_source: Vec<bool>,
    mode_by_source: Vec<u32>,
}

struct RiscvAplicIrqHandler {
    base: NonNull<u8>,
}

// SAFETY: the mapped APLIC register base is a stable MMIO capability owned by
// the platform interrupt controller. Register access is synchronized either by
// the rdrive controller lock or by the architecture hard-IRQ transaction.
unsafe impl Send for RiscvAplic {}

// SAFETY: the handler only performs one volatile MMIO write to the immutable
// APLIC base while completing an interrupt claim.
unsafe impl Send for RiscvAplicIrqHandler {}

impl RiscvAplic {
    unsafe fn new(base: NonNull<u8>, sources: usize, imsic: &RiscvImsic) -> Self {
        Self {
            base,
            sources,
            imsic_base: imsic.base,
            hart_index_bits: imsic.hart_index_bits,
            guest_index_bits: imsic.guest_index_bits,
            affinity_by_source: vec![crate::irq::IrqAffinity::Any; sources.saturating_add(1)],
            enabled_by_source: vec![false; sources.saturating_add(1)],
            mode_by_source: vec![APLIC_SOURCECFG_SM_INACTIVE; sources.saturating_add(1)],
        }
    }

    fn init_global(&self) {
        let base_ppn = self.imsic_base >> 12;
        unsafe {
            write32(self.base, APLIC_SMSICFGADDR, base_ppn as u32);
            write32(
                self.base,
                APLIC_SMSICFGADDRH,
                ((base_ppn >> 32) as u32)
                    | (self.hart_index_bits << APLIC_SMSICFGADDRH_LHXW_SHIFT)
                    | (self.guest_index_bits << APLIC_SMSICFGADDRH_LHXS_SHIFT),
            );
            write32(
                self.base,
                APLIC_DOMAINCFG,
                APLIC_DOMAINCFG_IE | APLIC_DOMAINCFG_DM,
            );
        }
    }

    fn disable_all_sources(&self) {
        for source in (0..=self.sources).step_by(32) {
            unsafe {
                write32(self.base, APLIC_CLRIE_BASE + (source / 32) * 4, u32::MAX);
            }
        }
        for source in 1..=self.sources {
            unsafe {
                write32(
                    self.base,
                    APLIC_SOURCECFG_BASE + (source - 1) * 4,
                    APLIC_SOURCECFG_SM_INACTIVE,
                );
            }
        }
    }

    fn configure_source(
        &mut self,
        source: NonZeroU32,
        trigger: Option<rdif_intc::Trigger>,
    ) -> Result<(), crate::irq::IrqError> {
        let source_index = self.checked_source_index(source)?;
        let mode = aplic_source_mode(trigger)?;
        self.mode_by_source[source_index] = mode;
        unsafe {
            write32(
                self.base,
                APLIC_SOURCECFG_BASE + (source_index - 1) * 4,
                mode,
            );
            write32(
                self.base,
                APLIC_TARGET_BASE + (source_index - 1) * 4,
                source.get(),
            );
        }
        Ok(())
    }

    fn set_source_enabled(
        &mut self,
        source: NonZeroU32,
        enabled: bool,
    ) -> Result<(), crate::irq::IrqError> {
        let source_index = self.checked_source_index(source)?;
        if self.mode_by_source[source_index] == APLIC_SOURCECFG_SM_INACTIVE {
            self.configure_source(source, Some(rdif_intc::Trigger::LevelHigh))?;
        }
        self.enabled_by_source[source_index] = enabled;
        if enabled {
            self.write_target(source)?;
            unsafe {
                imsic_enable_id(source);
                write32(self.base, APLIC_SETIENUM, source.get());
            }
        } else {
            unsafe {
                write32(self.base, APLIC_CLRIENUM, source.get());
            }
        }
        Ok(())
    }

    fn set_source_affinity(
        &mut self,
        source: NonZeroU32,
        affinity: crate::irq::IrqAffinity,
    ) -> Option<()> {
        let source_index = self.checked_source_index(source).ok()?;
        self.affinity_by_source[source_index] = affinity;
        if self.enabled_by_source[source_index] {
            self.write_target(source).ok()?;
        }
        Some(())
    }

    fn write_target(&self, source: NonZeroU32) -> Result<(), crate::irq::IrqError> {
        let source_index = self.checked_source_index(source)?;
        let cpu_id = match self.affinity_by_source[source_index] {
            crate::irq::IrqAffinity::Any => 0,
            crate::irq::IrqAffinity::Fixed { cpu_id } => cpu_id,
        };
        let hart_index = u32::try_from(cpu_id).map_err(|_| crate::irq::IrqError::InvalidCpu)?;
        let target = (hart_index << APLIC_TARGET_HART_IDX_SHIFT)
            | (0 << APLIC_TARGET_GUEST_IDX_SHIFT)
            | (source.get() & APLIC_TARGET_EIID_MASK);
        unsafe {
            write32(
                self.base,
                APLIC_TARGET_BASE + (source_index - 1) * 4,
                target,
            );
        }
        Ok(())
    }

    fn checked_source_index(&self, source: NonZeroU32) -> Result<usize, crate::irq::IrqError> {
        let source = source.get() as usize;
        if source == 0 || source > self.sources {
            Err(crate::irq::IrqError::InvalidIrq)
        } else {
            Ok(source)
        }
    }
}

impl RiscvAplicIrqHandler {
    fn complete(&self, claim: AplicClaim) {
        unsafe {
            write32(self.base, APLIC_SETIPNUM_LE, claim.source.get());
        }
    }
}

impl DriverGeneric for RiscvAplic {
    fn name(&self) -> &str {
        "RISC-V APLIC"
    }
}

impl Interface for RiscvAplic {
    fn translate_fdt(
        &self,
        irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        let Some(source) = irq_prop.first().copied().and_then(NonZeroU32::new) else {
            warn!("empty APLIC interrupt specifier");
            return Err(rdif_intc::IrqError::InvalidIrq);
        };
        self.checked_source_index(source)?;
        let trigger = irq_prop.get(1).and_then(|raw| trigger_from_fdt(*raw));
        Ok(rdif_intc::ControllerIrqTranslation {
            hwirq: rdif_intc::HwIrq(source.get()),
            trigger,
        })
    }

    fn configure(
        &mut self,
        translation: &rdif_intc::IrqTranslation,
    ) -> Result<(), rdif_intc::IrqError> {
        let source =
            NonZeroU32::new(translation.id.hwirq.0).ok_or(rdif_intc::IrqError::InvalidIrq)?;
        self.configure_source(source, translation.trigger)
    }

    fn set_enabled(
        &mut self,
        hwirq: rdif_intc::HwIrq,
        enabled: bool,
    ) -> Result<(), rdif_intc::IrqError> {
        let source = NonZeroU32::new(hwirq.0).ok_or(rdif_intc::IrqError::InvalidIrq)?;
        self.set_source_enabled(source, enabled)
    }
}

fn trigger_from_fdt(raw: u32) -> Option<rdif_intc::Trigger> {
    match raw {
        IRQ_TYPE_EDGE_RISING => Some(rdif_intc::Trigger::EdgeRising),
        IRQ_TYPE_EDGE_FALLING => Some(rdif_intc::Trigger::EdgeFailling),
        IRQ_TYPE_LEVEL_HIGH => Some(rdif_intc::Trigger::LevelHigh),
        IRQ_TYPE_LEVEL_LOW => Some(rdif_intc::Trigger::LevelLow),
        _ => None,
    }
}

fn aplic_source_mode(trigger: Option<rdif_intc::Trigger>) -> Result<u32, crate::irq::IrqError> {
    match trigger.unwrap_or(rdif_intc::Trigger::LevelHigh) {
        rdif_intc::Trigger::EdgeRising => Ok(APLIC_SOURCECFG_SM_EDGE_RISE),
        rdif_intc::Trigger::EdgeFailling => Ok(APLIC_SOURCECFG_SM_EDGE_FALL),
        rdif_intc::Trigger::LevelHigh => Ok(APLIC_SOURCECFG_SM_LEVEL_HIGH),
        rdif_intc::Trigger::LevelLow => Ok(APLIC_SOURCECFG_SM_LEVEL_LOW),
        rdif_intc::Trigger::EdgeBoth => Err(crate::irq::IrqError::Unsupported),
    }
}

unsafe fn imsic_write(reg: usize, value: usize) {
    unsafe {
        siselect::write(siselect::Siselect::from_bits(reg));
        core::arch::asm!("csrw 0x151, {value}", value = in(reg) value);
    }
}

unsafe fn imsic_set(reg: usize, value: usize) {
    unsafe {
        siselect::write(siselect::Siselect::from_bits(reg));
        core::arch::asm!("csrs 0x151, {value}", value = in(reg) value);
    }
}

unsafe fn imsic_enable_id(source: NonZeroU32) {
    let source = source.get() as usize;
    if get_imsic().is_some_and(|imsic| source <= imsic.num_ids) {
        let reg = imsic_eix_selector(IMSIC_EIE0, source);
        let bit = 1usize << (source % usize::BITS as usize);
        unsafe {
            imsic_set(reg, bit);
        }
    }
}

fn imsic_eix_selector(base: usize, interrupt_id: usize) -> usize {
    base + interrupt_id / usize::BITS as usize * (usize::BITS as usize / IMSIC_EIX_BITS)
}

unsafe fn imsic_claim() -> Option<NonZeroU32> {
    let value: usize;
    unsafe {
        core::arch::asm!("csrrw {value}, 0x15c, zero", value = out(reg) value);
    }
    NonZeroU32::new(((value >> IMSIC_TOPEI_ID_SHIFT) & IMSIC_TOPEI_ID_MASK) as u32)
}

unsafe fn write32(base: NonNull<u8>, offset: usize, value: u32) {
    unsafe {
        (base.as_ptr().add(offset) as *mut u32).write_volatile(value);
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

    fn set_source_affinity(
        &mut self,
        source: NonZeroU32,
        affinity: crate::irq::IrqAffinity,
    ) -> Option<()> {
        if source.get() as usize > self.sources {
            warn!(
                "skip setting affinity for out-of-range PLIC source {}",
                source.get()
            );
            return None;
        }
        if let crate::irq::IrqAffinity::Fixed { cpu_id } = affinity
            && self
                .context_by_cpu
                .get(cpu_id)
                .and_then(|ctx| *ctx)
                .is_none()
        {
            warn!("PLIC supervisor context for affinity CPU {cpu_id} is not found");
            return None;
        }

        let was_enabled = self.enabled_by_source[source.get() as usize];
        self.disable_source_contexts(source);
        self.affinity_by_source[source.get() as usize] = affinity;
        if was_enabled {
            for context in self.contexts_for_source(source) {
                self.inner.enable(source, context);
            }
        }
        Some(())
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
        if enabled {
            self.enable_source(source)
        } else {
            self.disable_source(source)
        }
    }
}
