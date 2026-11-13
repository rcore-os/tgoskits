use alloc::{format, sync::Arc, vec, vec::Vec};
use core::{num::NonZeroU32, ptr::NonNull};

use ax_sync::RawSpinLock;
use kernutil::StaticCell;
use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, Phandle, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::ProbeFdt,
};
use riscv_aia_driver::{Aplic, MsiTarget, SourceMode};

use super::imsic::{self, RiscvImsic};
use crate::{
    common::ioremap,
    irq::{IrqAffinity, IrqError},
};

static APLIC_HANDLER: StaticCell<Arc<RiscvAplicIrqHandler>> = StaticCell::uninit();

module_driver!(
    name: "RISC-V APLIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["riscv,aplic", "spacemit,k3-aplic"],
        on_probe: probe_aplic
    }],
);

pub(in crate::arch) fn irq_set_affinity(
    hwirq: rdif_intc::HwIrq,
    affinity: IrqAffinity,
) -> Result<(), IrqError> {
    let source = NonZeroU32::new(hwirq.0).ok_or(IrqError::InvalidIrq)?;
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvAplic)
        .ok_or(IrqError::Controller)?;
    let intc = crate::irq::intc_by_domain(domain)?;
    let mut intc = intc.lock().map_err(|_| IrqError::Controller)?;
    let aplic = intc.typed_mut::<RiscvAplic>().ok_or(IrqError::Controller)?;
    aplic.set_source_affinity(source, affinity)
}

pub(in crate::arch) fn claim_external_irq() -> Option<NonZeroU32> {
    if !APLIC_HANDLER.is_init() {
        return None;
    }
    imsic::claim()
}

pub(in crate::arch) fn complete(source: NonZeroU32) {
    if APLIC_HANDLER.is_init() {
        APLIC_HANDLER.complete(source);
    }
}

fn probe_aplic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let Some(parent) = msi_parent(&info.node)? else {
        return Ok(());
    };
    let parent_node = info
        .get_by_phandle(parent)
        .ok_or_else(|| OnProbeError::other("APLIC MSI parent is missing"))?;
    // Machine APLIC state belongs to SBI, including its MSI address registers.
    // Identify the parent before mapping or touching any APLIC registers.
    if !imsic::is_supervisor(&parent_node)? {
        return Ok(());
    }
    let imsic = imsic::get()
        .ok_or_else(|| OnProbeError::other("APLIC MSI mode requires a probed IMSIC"))?;
    if parent != imsic.phandle {
        return Err(OnProbeError::other(
            "APLIC msi-parent does not match the supervisor IMSIC",
        ));
    }
    if APLIC_HANDLER.is_init() {
        return Err(OnProbeError::other(
            "multiple supervisor APLIC instances are unsupported",
        ));
    }
    let sources = info
        .node
        .as_node()
        .get_property("riscv,num-sources")
        .and_then(|p| p.get_u32())
        .filter(|sources| (1..=1023).contains(sources))
        .ok_or_else(|| OnProbeError::other("APLIC source count must be between 1 and 1023"))?
        as usize;
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("APLIC requires reg"))?;
    let size = reg
        .size
        .ok_or_else(|| OnProbeError::other("APLIC requires a register window size"))?;
    if size < 0x4000 || reg.address & 3 != 0 || reg.address.checked_add(size).is_none() {
        return Err(OnProbeError::other(
            "APLIC requires an aligned 16 KiB register window",
        ));
    }
    let size = usize::try_from(size)
        .map_err(|_| OnProbeError::other("APLIC register window exceeds usize"))?;
    let mmio = ioremap(reg.address, size)
        .map_err(|err| OnProbeError::other(format!("failed to map APLIC: {err:?}")))?;
    let base = NonNull::new(mmio.as_ptr())
        .ok_or_else(|| OnProbeError::other("APLIC MMIO mapping is null"))?;
    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::RiscvAplic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register APLIC domain: {err:?}")))?;
    // SAFETY: ioremap retains the validated, word-aligned register window for
    // the kernel lifetime. Only this driver owns the supervisor domain.
    let aplic = unsafe { RiscvAplic::new(base, sources, imsic) };
    aplic.reset();
    APLIC_HANDLER.init(aplic.irq.clone());
    let irq = aplic.irq.clone();
    dev.register(rdif_intc::Intc::new(domain, aplic));
    // Publish both control and completion endpoints before admitting delivery.
    // S-level domains do not implement SMSICFGADDR/H; firmware owns that setup.
    irq.state.lock_irqsave().registers.enable_msi_domain();
    // SAFETY: this only opens the hart's external-IRQ input, not global SIE.
    unsafe {
        riscv::register::sie::set_sext();
    }
    info!(
        "RISC-V APLIC registered: node={}, base={:#x}, sources={}, usable_ids={}",
        info.node.name(),
        reg.address,
        sources,
        sources.min(imsic.num_ids)
    );
    Ok(())
}

fn msi_parent(node: &NodeType<'_>) -> Result<Option<Phandle>, OnProbeError> {
    let Some(property) = node.as_node().get_property("msi-parent") else {
        // A machine direct-delivery domain belongs to SBI too; it has no
        // MSI parent. Supervisor direct delivery is outside this backend.
        if !imsic::is_supervisor(node)? {
            return Ok(None);
        }
        return Err(OnProbeError::other(
            "supervisor APLIC direct delivery is unsupported",
        ));
    };
    property
        .get_u32()
        .map(Phandle::from)
        .map(Some)
        .ok_or_else(|| OnProbeError::other("invalid APLIC msi-parent"))
}

struct RiscvAplic {
    sources: usize,
    num_ids: usize,
    target_by_cpu: Vec<u32>,
    affinity_by_source: Vec<IrqAffinity>,
    mode_by_source: Vec<SourceMode>,
    irq: Arc<RiscvAplicIrqHandler>,
}

struct RiscvAplicIrqHandler {
    sources: usize,
    // Only fixed-size state and MMIO commands are touched under this IRQ-save
    // lock. No device lock, callback, allocation or notification is entered.
    state: RawSpinLock<AplicIrqState>,
}

#[derive(Clone, Copy, Default)]
struct AplicSourceState {
    enabled: bool,
    deferred: bool,
    guest_owned: bool,
    retired: bool,
}

struct AplicIrqState {
    registers: Aplic<'static>,
    sources: Vec<AplicSourceState>,
}

impl RiscvAplic {
    /// # Safety
    /// `base` must retain an aligned 16 KiB APLIC MMIO window on every CPU for
    /// the kernel lifetime. The platform must transfer sole configuration and
    /// source-transaction control; outside volatile observations or reserved-
    /// source pending injections must follow `Aplic::new`'s MMIO protocol.
    /// `sources` is 1..=1023.
    unsafe fn new(base: NonNull<u8>, sources: usize, imsic: &RiscvImsic) -> Self {
        // SAFETY: the probe transfers its persistent supervisor mapping here.
        // The register capability remains exclusively in the IRQ-save lock.
        let registers =
            unsafe { Aplic::new(base, 0x4000, sources) }.expect("validated APLIC register window");
        Self {
            sources,
            num_ids: imsic.num_ids,
            target_by_cpu: imsic.target_by_cpu.clone(),
            affinity_by_source: vec![IrqAffinity::Any; sources + 1],
            mode_by_source: vec![SourceMode::Inactive; sources + 1],
            irq: Arc::new(RiscvAplicIrqHandler {
                sources: sources.min(imsic.num_ids),
                state: RawSpinLock::new(AplicIrqState {
                    registers,
                    sources: vec![AplicSourceState::default(); sources + 1],
                }),
            }),
        }
    }

    fn reset(&self) {
        self.irq.state.lock_irqsave().registers.reset_msi();
    }

    fn configure_source(
        &mut self,
        source: NonZeroU32,
        trigger: Option<rdif_intc::Trigger>,
    ) -> Result<(), IrqError> {
        let index = self.checked_source_index(source)?;
        let mode = source_mode(trigger)?;
        let target = self.target(source, self.affinity_by_source[index])?;
        let mut state = self.irq.state.lock_irqsave();
        if state.sources[index].retired
            || state.sources[index].guest_owned
            || state.sources[index].deferred
        {
            return Err(IrqError::Busy);
        }
        state
            .registers
            .set_target(source, target)
            .expect("validated source");
        state
            .registers
            .set_source_mode(source, mode)
            .expect("validated source");
        self.mode_by_source[index] = mode;
        if mode.is_level() {
            // AIA 4.9.2: changing source mode need not sample an asserted line.
            state
                .registers
                .set_pending(source)
                .expect("validated source");
        }
        Ok(())
    }

    fn set_source_enabled(&mut self, source: NonZeroU32, enabled: bool) -> Result<(), IrqError> {
        let index = self.checked_source_index(source)?;
        if enabled {
            let target = self.target(source, self.affinity_by_source[index])?;
            if self.mode_by_source[index] == SourceMode::Inactive {
                self.configure_source(source, Some(rdif_intc::Trigger::LevelHigh))?;
            }
            let mut state = self.irq.state.lock_irqsave();
            if state.sources[index].retired {
                return Err(IrqError::Busy);
            }
            state
                .registers
                .set_target(source, target)
                .expect("validated source");
            state.sources[index].enabled = true;
            if !state.sources[index].deferred {
                state
                    .registers
                    .set_enabled(source, true)
                    .expect("validated source");
            }
        } else {
            let mut state = self.irq.state.lock_irqsave();
            state.sources[index].enabled = false;
            state
                .registers
                .set_enabled(source, false)
                .expect("validated source");
        }
        Ok(())
    }

    fn set_source_affinity(
        &mut self,
        source: NonZeroU32,
        affinity: IrqAffinity,
    ) -> Result<(), IrqError> {
        let index = self.checked_source_index(source)?;
        let target = self.target(source, affinity)?;
        let mut state = self.irq.state.lock_irqsave();
        if state.sources[index].retired
            || state.sources[index].guest_owned
            || state.sources[index].deferred
        {
            return Err(IrqError::Busy);
        }
        state
            .registers
            .set_target(source, target)
            .expect("validated source");
        self.affinity_by_source[index] = affinity;
        Ok(())
    }

    fn target(&self, source: NonZeroU32, affinity: IrqAffinity) -> Result<MsiTarget, IrqError> {
        let cpu = match affinity {
            IrqAffinity::Any => 0,
            IrqAffinity::Fixed { cpu_id } => cpu_id,
        };
        let hart_index = self.target_by_cpu.get(cpu).ok_or(IrqError::InvalidCpu)?;
        MsiTarget::new(*hart_index, source).map_err(|_| IrqError::InvalidCpu)
    }

    fn checked_source_index(&self, source: NonZeroU32) -> Result<usize, IrqError> {
        let source = source.get() as usize;
        if source > self.sources || source > self.num_ids {
            Err(IrqError::InvalidIrq)
        } else {
            Ok(source)
        }
    }
}

impl RiscvAplicIrqHandler {
    fn complete(&self, source: NonZeroU32) {
        let index = source.get() as usize;
        if index > self.sources {
            return;
        }
        let mut state = self.state.lock_irqsave();
        // A late/duplicate MSI cannot steal an outstanding guest completion.
        if !state.sources[index].deferred && !state.sources[index].retired {
            state.registers.resample(source).expect("validated source");
        }
    }

    fn defer(&self, source: NonZeroU32) -> bool {
        let index = source.get() as usize;
        if index > self.sources {
            return false;
        }
        let mut state = self.state.lock_irqsave();
        let source_state = &mut state.sources[index];
        if !source_state.enabled
            || !source_state.guest_owned
            || source_state.deferred
            || source_state.retired
        {
            return false;
        }
        source_state.deferred = true;
        state
            .registers
            .set_enabled(source, false)
            .expect("validated source");
        true
    }

    fn finish_deferred(&self, source: NonZeroU32) -> bool {
        let index = source.get() as usize;
        if index > self.sources {
            return false;
        }
        let mut state = self.state.lock_irqsave();
        if !state.sources[index].deferred {
            return false;
        }
        state.sources[index].deferred = false;
        if !state.sources[index].retired {
            state.registers.resample(source).expect("validated source");
            if state.sources[index].enabled {
                state
                    .registers
                    .set_enabled(source, true)
                    .expect("validated source");
            }
        }
        true
    }
}

pub(in crate::arch) fn defer(source: NonZeroU32) -> bool {
    APLIC_HANDLER.is_init() && APLIC_HANDLER.defer(source)
}

pub(in crate::arch) fn finish_deferred(source: NonZeroU32) -> bool {
    APLIC_HANDLER.is_init() && APLIC_HANDLER.finish_deferred(source)
}

pub(in crate::arch) fn activate_guest_source(
    source: NonZeroU32,
    target_cpu: usize,
    trigger: rdif_intc::Trigger,
) -> Result<(), IrqError> {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvAplic)
        .ok_or(IrqError::Unsupported)?;
    let intc = crate::irq::intc_by_domain(domain)?;
    let mut intc = intc.lock().map_err(|_| IrqError::Controller)?;
    let aplic = intc.typed_mut::<RiscvAplic>().ok_or(IrqError::Controller)?;
    let index = aplic.checked_source_index(source)?;
    // Resolve all fallible input before committing ownership or registers.
    let mode = source_mode(Some(trigger))?;
    let affinity = IrqAffinity::Fixed { cpu_id: target_cpu };
    let target = aplic.target(source, affinity)?;
    let mut state = aplic.irq.state.lock_irqsave();
    let source_state = &state.sources[index];
    if source_state.retired
        || source_state.guest_owned
        || source_state.enabled
        || source_state.deferred
    {
        return Err(IrqError::Busy);
    }
    state
        .registers
        .set_enabled(source, false)
        .expect("validated source");
    // An inactive source may ignore target writes. Activate its mode while
    // masked, then install the target before opening delivery (AIA 4.5.3).
    state
        .registers
        .set_source_mode(source, mode)
        .expect("validated source");
    state
        .registers
        .set_target(source, target)
        .expect("validated source");
    aplic.mode_by_source[index] = mode;
    aplic.affinity_by_source[index] = affinity;
    state.sources[index].guest_owned = true;
    state.sources[index].enabled = true;
    if mode.is_level() {
        state.registers.resample(source).expect("validated source");
    }
    state
        .registers
        .set_enabled(source, true)
        .expect("validated source");
    Ok(())
}

pub(in crate::arch) fn retire_guest_source(source: NonZeroU32) -> Result<(), IrqError> {
    if !APLIC_HANDLER.is_init() {
        return Err(IrqError::Unsupported);
    }
    let index = source.get() as usize;
    if index > APLIC_HANDLER.sources {
        return Err(IrqError::InvalidIrq);
    }
    let mut state = APLIC_HANDLER.state.lock_irqsave();
    if state.sources[index].guest_owned {
        state.sources[index].enabled = false;
        state.sources[index].retired = true;
        state
            .registers
            .set_enabled(source, false)
            .expect("validated source");
    }
    // AIA 4.9.3 allows late MSIs. This source cannot be assigned again until
    // platform reset; software reader quiescence is not hardware draining.
    Ok(())
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
    ) -> Result<rdif_intc::ControllerIrqTranslation, IrqError> {
        let [source, flags] = irq_prop else {
            return Err(IrqError::InvalidIrq);
        };
        let source = NonZeroU32::new(*source).ok_or(IrqError::InvalidIrq)?;
        self.checked_source_index(source)?;
        let trigger = match flags {
            1 => rdif_intc::Trigger::EdgeRising,
            2 => rdif_intc::Trigger::EdgeFailling,
            4 => rdif_intc::Trigger::LevelHigh,
            8 => rdif_intc::Trigger::LevelLow,
            _ => return Err(IrqError::InvalidIrq),
        };
        Ok(rdif_intc::ControllerIrqTranslation {
            hwirq: rdif_intc::HwIrq(source.get()),
            trigger: Some(trigger),
        })
    }
    fn configure(&mut self, translation: &rdif_intc::IrqTranslation) -> Result<(), IrqError> {
        let source = NonZeroU32::new(translation.id.hwirq.0).ok_or(IrqError::InvalidIrq)?;
        self.configure_source(source, translation.trigger)
    }
    fn set_enabled(&mut self, hwirq: rdif_intc::HwIrq, enabled: bool) -> Result<(), IrqError> {
        let source = NonZeroU32::new(hwirq.0).ok_or(IrqError::InvalidIrq)?;
        self.set_source_enabled(source, enabled)
    }
}

fn source_mode(trigger: Option<rdif_intc::Trigger>) -> Result<SourceMode, IrqError> {
    match trigger.unwrap_or(rdif_intc::Trigger::LevelHigh) {
        rdif_intc::Trigger::EdgeRising => Ok(SourceMode::EdgeRising),
        rdif_intc::Trigger::EdgeFailling => Ok(SourceMode::EdgeFalling),
        rdif_intc::Trigger::LevelHigh => Ok(SourceMode::LevelHigh),
        rdif_intc::Trigger::LevelLow => Ok(SourceMode::LevelLow),
        rdif_intc::Trigger::EdgeBoth => Err(IrqError::Unsupported),
    }
}
