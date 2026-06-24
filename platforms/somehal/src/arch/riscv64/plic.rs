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
use riscv::register::{sie, sip};
use sbi_rt::HartMask;

use crate::common::ioremap;

const INTC_IRQ_BASE: usize = 1usize << (usize::BITS as usize - 1);
const S_SOFT: usize = INTC_IRQ_BASE | 1;
const S_TIMER: usize = INTC_IRQ_BASE | 5;
const S_EXT: usize = INTC_IRQ_BASE | 9;
const SUPERVISOR_EXTERNAL_INTERRUPT: u32 = 9;
const DEFAULT_PRIORITY: u32 = 1;
const DEFAULT_PLIC_SIZE: usize = 0x400_0000;

static IRQ_HANDLER: StaticCell<RiscvPlicIrqHandler> = StaticCell::uninit();

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
    S_TIMER.into()
}

pub fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
    let raw: usize = irq.into();
    match raw {
        S_TIMER => unsafe {
            if enable {
                sie::set_stimer();
            } else {
                sie::clear_stimer();
            }
        },
        S_SOFT => unsafe {
            if enable {
                sie::set_ssoft();
            } else {
                sie::clear_ssoft();
            }
        },
        S_EXT => unsafe {
            if enable {
                sie::set_sext();
            } else {
                sie::clear_sext();
            }
        },
        external if external & INTC_IRQ_BASE == 0 => set_external_irq_enable(external, enable),
        other => warn!("unsupported RISC-V local IRQ {other:#x}"),
    }
}

pub fn irq_set_affinity(
    irq: rdrive::IrqId,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), &'static str> {
    let raw: usize = irq.into();
    if raw & INTC_IRQ_BASE != 0 {
        return Err("RISC-V local IRQ affinity cannot be changed");
    }
    let Some(source) = NonZeroU32::new(raw as u32) else {
        return Err("invalid PLIC source 0");
    };
    with_plic("setting PLIC IRQ affinity", |plic| {
        plic.set_source_affinity(source, affinity)
    })
    .flatten()
    .ok_or("RISC-V PLIC is not registered or affinity target is invalid")
}

enum Completion {
    None,
    Plic(NonZeroU32),
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
        if let Completion::Plic(source) = self.completion {
            complete_external_irq_source(source);
        }
    }
}

pub fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    match raw {
        S_TIMER => Some(ActiveIrq {
            irq: S_TIMER.into(),
            completion: Completion::None,
        }),
        S_SOFT => {
            unsafe {
                sip::clear_ssoft();
            }
            Some(ActiveIrq {
                irq: S_SOFT.into(),
                completion: Completion::None,
            })
        }
        S_EXT => begin_external_irq(),
        external if external & INTC_IRQ_BASE == 0 => Some(ActiveIrq {
            irq: external.into(),
            completion: Completion::None,
        }),
        other => {
            warn!("unsupported RISC-V interrupt cause {other:#x}");
            None
        }
    }
}

fn begin_external_irq() -> Option<ActiveIrq> {
    let source = claim_external_irq_source()?;
    Some(ActiveIrq {
        irq: (source.get() as usize).into(),
        completion: Completion::Plic(source),
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

pub fn send_ipi_to_cpu(cpu_id: usize) {
    let Some(hart_id) = someboot::smp::cpu_idx_to_id(cpu_id) else {
        warn!("failed to resolve hart id for logical CPU {cpu_id}");
        return;
    };
    let res = sbi_rt::send_ipi(HartMask::from_mask_base(1, hart_id));
    if !res.is_ok() {
        warn!("send_ipi to hart {hart_id} failed: {res:?}");
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
    let contexts = parse_supervisor_contexts(&info);

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

    dev.register(rdif_intc::Intc::new(plic));
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

fn set_external_irq_enable(irq: usize, enable: bool) {
    let Some(source) = NonZeroU32::new(irq as u32) else {
        return;
    };
    with_plic("setting PLIC IRQ enable", |plic| {
        if enable {
            plic.enable_source(source);
        } else {
            plic.disable_source(source);
        }
    });
}

fn claim_external_irq_source() -> Option<NonZeroU32> {
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
    fn enable_source(&mut self, source: NonZeroU32) {
        if source.get() as usize > self.sources {
            warn!("skip enabling out-of-range PLIC source {}", source.get());
            return;
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
    }

    fn disable_source(&mut self, source: NonZeroU32) {
        if source.get() as usize > self.sources {
            warn!("skip disabling out-of-range PLIC source {}", source.get());
            return;
        }
        self.enabled_by_source[source.get() as usize] = false;
        self.disable_source_contexts(source);
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

impl DriverGeneric for RiscvPlic {
    fn name(&self) -> &str {
        "RISC-V PLIC"
    }
}

impl Interface for RiscvPlic {
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> rdrive::IrqId {
        let Some(source) = irq_prop.first().copied().map(|source| source as usize) else {
            warn!("empty PLIC interrupt specifier");
            return 0usize.into();
        };
        if source > self.sources {
            warn!(
                "PLIC interrupt source {} exceeds riscv,ndev {}",
                source, self.sources
            );
        }
        source.into()
    }
}
