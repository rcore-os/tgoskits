use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use ax_kspin::SpinIrqSave;
use irq_framework::{CpuId, IrqId, IrqScope};
use kernutil::StaticCell;
use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::ProbeFdt,
};

use super::irq_common::{LIOINTC_VECTOR_COUNT, fdt_first_cell_vector};
use crate::{
    common::ioremap,
    irq_line::{BoundIrqStatus, IrqChipLine, PreparedIrqChipLine},
    setup::MmioRaw,
};

const DEFAULT_LIOINTC_PADDR: usize = 0x1fe0_1400;
const DEFAULT_LIOINTC_SIZE: usize = 0x40;
const DEFAULT_LIOINTC_ISR_PADDR: usize = 0x1fe0_1040;
const DEFAULT_LIOINTC_ISR_SIZE: usize = 0x10;
const DEFAULT_CASCADE_IRQ: usize = 2;
const PARENT_INT_COUNT: usize = 4;

const ROUTE_BASE: usize = 0x00;
const REG_ENABLE: usize = 0x28;
const REG_DISABLE: usize = 0x2c;
const REG_POLARITY: usize = 0x30;
const REG_EDGE: usize = 0x34;

const ROUTE_CPU0: u8 = 1 << 0;
const ROUTE_INT_SHIFT: usize = 4;
const CPU_HWI_BASE_IRQ: usize = 2;
const ROUTE_INT_COUNT: usize = 4;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static CASCADE_IRQ_MASK: AtomicUsize = AtomicUsize::new(0);
static FAST_PATH: StaticCell<LioIntcFastPath> = StaticCell::uninit();

module_driver!(
    name: "Loongson LS2K1000 LIOINTC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,2k1000-icu",
            "loongson,ls2k1000-icu",
            "loongson,liointc",
        ],
        on_probe: probe_liointc_fdt,
    }],
);

pub fn is_cascade_irq(irq: usize) -> bool {
    REGISTERED.load(Ordering::Acquire)
        && irq < usize::BITS as usize
        && (CASCADE_IRQ_MASK.load(Ordering::Acquire) & (1usize << irq)) != 0
}

pub fn claim_irq(raw: usize) -> Option<crate::irq::IrqId> {
    FAST_PATH.get_initialized()?.claim_irq(raw)
}

pub fn complete_irq(irq: crate::irq::IrqId) {
    if let Some(fast_path) = FAST_PATH.get_initialized() {
        fast_path.complete_irq(irq);
    }
}

pub(super) fn prepare_irq_line(
    irq: IrqId,
    scope: IrqScope,
) -> Result<PreparedIrqChipLine, crate::irq::IrqError> {
    if scope != IrqScope::Global {
        return Err(crate::irq::IrqError::InvalidIrq);
    }
    let fast_path = FAST_PATH
        .get_initialized()
        .ok_or(crate::irq::IrqError::Unsupported)?;
    fast_path.validate_irq(irq)?;
    fast_path.set_enabled(irq.hwirq.0 as usize, false);
    Ok(PreparedIrqChipLine::maskable(Box::new(LioIrqChipLine {
        irq,
        fast_path,
    })))
}

fn probe_liointc_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let mut regs = info.node.regs().into_iter();
    let reg = regs.next();
    let isr = regs.next();

    let reg_addr = reg
        .as_ref()
        .map(|reg| reg.address as usize)
        .unwrap_or(DEFAULT_LIOINTC_PADDR);
    let reg_size = reg
        .as_ref()
        .and_then(|reg| reg.size)
        .unwrap_or(DEFAULT_LIOINTC_SIZE as u64) as usize;
    let isr_addr = isr
        .as_ref()
        .map(|reg| reg.address as usize)
        .unwrap_or(DEFAULT_LIOINTC_ISR_PADDR);
    let isr_size = isr
        .as_ref()
        .and_then(|reg| reg.size)
        .unwrap_or(DEFAULT_LIOINTC_ISR_SIZE as u64) as usize;
    let parent_irqs = parent_irqs_from_fdt(&info);
    let parent_int_map = parent_int_map_from_fdt(&info, &parent_irqs);
    let mmio = LioIntcMmioRegions {
        regs: LioIntcMmioRegion {
            addr: reg_addr,
            size: reg_size,
        },
        isr: LioIntcMmioRegion {
            addr: isr_addr,
            size: isr_size,
        },
    };

    register_liointc(dev, info.node.name(), mmio, parent_irqs, parent_int_map)
}

#[derive(Clone, Copy)]
struct LioIntcMmioRegion {
    addr: usize,
    size: usize,
}

#[derive(Clone, Copy)]
struct LioIntcMmioRegions {
    regs: LioIntcMmioRegion,
    isr: LioIntcMmioRegion,
}

fn register_liointc(
    dev: PlatformDevice,
    node_name: &str,
    mmio: LioIntcMmioRegions,
    parent_irqs: [Option<usize>; PARENT_INT_COUNT],
    parent_int_map: [u32; PARENT_INT_COUNT],
) -> Result<(), OnProbeError> {
    let regs = map_liointc_mmio(mmio.regs.addr, mmio.regs.size, "register")?;
    let isr = map_liointc_mmio(mmio.isr.addr, mmio.isr.size, "ISR")?;
    let intc = LioIntc::new(regs, isr, parent_irqs, parent_int_map);
    intc.init();
    let reg_addr = mmio.regs.addr;
    let isr_addr = mmio.isr.addr;

    debug!(
        "probing LS2K1000 LIOINTC: node={}, regs={reg_addr:#x}->{:#x}, isr={isr_addr:#x}->{:#x}, \
         parent_irqs={:?}, parent_int_map={:#x?}, inputs={}",
        node_name,
        intc.regs.as_ptr() as usize,
        intc.isr.as_ptr() as usize,
        intc.parent_irqs,
        intc.parent_int_map,
        LIOINTC_VECTOR_COUNT,
    );

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchLioIntc,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register LIOINTC domain: {err:?}")))?;
    let cascade_mask = intc.cascade_irq_mask();
    for cascade_irq in intc.parent_irqs.into_iter().flatten() {
        someboot::irq::irq_set_enable(someboot::irq::IrqId::new(cascade_irq), true);
    }
    FAST_PATH.init(LioIntcFastPath::new(
        domain,
        intc.regs.clone(),
        intc.isr.clone(),
        intc.parent_irqs,
    ));
    dev.register(rdif_intc::Intc::new(domain, intc));
    CASCADE_IRQ_MASK.fetch_or(cascade_mask, Ordering::AcqRel);
    REGISTERED.store(true, Ordering::Release);
    Ok(())
}

fn parent_irqs_from_fdt(info: &rdrive::register::FdtInfo<'_>) -> [Option<usize>; PARENT_INT_COUNT] {
    let mut parent_irqs = [None; PARENT_INT_COUNT];
    let mut any = false;

    for interrupt in info.interrupts() {
        let Some(irq) = fdt_first_cell_vector(&interrupt.specifier) else {
            continue;
        };
        set_parent_irq(&mut parent_irqs, irq);
        any = true;
    }

    if !any && let Some(prop) = info.node.as_node().get_property("interrupts") {
        for irq in prop.get_u32_iter() {
            set_parent_irq(&mut parent_irqs, irq as usize);
            any = true;
        }
    }

    if !any {
        set_parent_irq(&mut parent_irqs, DEFAULT_CASCADE_IRQ);
    }
    parent_irqs
}

fn set_parent_irq(parent_irqs: &mut [Option<usize>; PARENT_INT_COUNT], irq: usize) {
    let index = parent_index_from_cpu_irq(irq).unwrap_or_else(|| {
        warn!("LIOINTC parent IRQ {irq} is outside CPU HWI range; treating it as parent INT0");
        0
    });
    parent_irqs[index] = Some(irq);
}

fn parent_index_from_cpu_irq(irq: usize) -> Option<usize> {
    irq.checked_sub(CPU_HWI_BASE_IRQ)
        .filter(|index| *index < PARENT_INT_COUNT)
}

fn parent_int_map_from_fdt(
    info: &rdrive::register::FdtInfo<'_>,
    parent_irqs: &[Option<usize>; PARENT_INT_COUNT],
) -> [u32; PARENT_INT_COUNT] {
    let mut parent_int_map = [0; PARENT_INT_COUNT];
    if let Some(prop) = info.node.as_node().get_property("loongson,parent_int_map") {
        for (index, map) in prop.get_u32_iter().take(PARENT_INT_COUNT).enumerate() {
            parent_int_map[index] = map;
        }
    }
    if parent_int_map.iter().all(|map| *map == 0) {
        let parent_index = parent_irqs.iter().position(Option::is_some).unwrap_or(0);
        parent_int_map[parent_index] = u32::MAX;
    }
    parent_int_map
}

fn map_liointc_mmio(addr: usize, size: usize, name: &str) -> Result<MmioRaw, OnProbeError> {
    if size == 0 {
        return Err(OnProbeError::other(format!(
            "LS2K1000 LIOINTC {name} region has zero size"
        )));
    }
    ioremap(addr as u64, size).map_err(|err| {
        OnProbeError::other(format!(
            "failed to map LS2K1000 LIOINTC {name} region: {err:?}"
        ))
    })
}

fn route_int_bit(parent_index: usize) -> u8 {
    debug_assert!(parent_index < ROUTE_INT_COUNT);
    1 << (ROUTE_INT_SHIFT + parent_index)
}

struct LioIntc {
    regs: MmioRaw,
    isr: MmioRaw,
    parent_irqs: [Option<usize>; PARENT_INT_COUNT],
    parent_int_map: [u32; PARENT_INT_COUNT],
}

impl LioIntc {
    fn new(
        regs: MmioRaw,
        isr: MmioRaw,
        parent_irqs: [Option<usize>; PARENT_INT_COUNT],
        parent_int_map: [u32; PARENT_INT_COUNT],
    ) -> Self {
        Self {
            regs,
            isr,
            parent_irqs,
            parent_int_map,
        }
    }

    fn init(&self) {
        for irq in 0..LIOINTC_VECTOR_COUNT {
            self.write_route(irq, self.route_value_for_input(irq));
        }

        self.write_reg_u32(REG_DISABLE, u32::MAX);
        self.write_reg_u32(REG_EDGE, 0);
        // LIOINTC POL=0 selects active-high level interrupts.
        self.write_reg_u32(REG_POLARITY, 0);
    }

    fn route_value_for_input(&self, input: usize) -> u8 {
        let parent_index = self.parent_index_for_input(input).unwrap_or_else(|| {
            warn!("LIOINTC input {input} has no usable parent INT route; routing through INT0");
            0
        });
        ROUTE_CPU0 | route_int_bit(parent_index)
    }

    fn parent_index_for_input(&self, input: usize) -> Option<usize> {
        let bit = 1u32.checked_shl(input as u32)?;
        self.parent_int_map
            .iter()
            .enumerate()
            .find(|(index, map)| self.parent_irqs[*index].is_some() && (*map & bit) != 0)
            .map(|(index, _)| index)
            .or_else(|| self.parent_irqs.iter().position(Option::is_some))
    }

    fn cascade_irq_mask(&self) -> usize {
        self.parent_irqs
            .into_iter()
            .flatten()
            .filter(|irq| *irq < usize::BITS as usize)
            .fold(0usize, |mask, irq| mask | (1usize << irq))
    }

    fn contains_input(&self, input: usize, op: &str) -> bool {
        if input < LIOINTC_VECTOR_COUNT {
            true
        } else {
            warn!("skip {op} for out-of-range LIOINTC input {input}");
            false
        }
    }

    fn write_reg_u32(&self, offset: usize, value: u32) {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.regs.size());
        self.regs.write(offset, value);
    }

    fn write_route(&self, irq: usize, value: u8) {
        debug_assert!(ROUTE_BASE + irq < self.regs.size());
        self.regs.write(ROUTE_BASE + irq, value);
    }
}

impl DriverGeneric for LioIntc {
    fn name(&self) -> &str {
        "Loongson LS2K1000 LIOINTC"
    }
}

impl Interface for LioIntc {
    fn translate_fdt(
        &self,
        irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        let Some(input) = fdt_first_cell_vector(irq_prop) else {
            warn!("empty LIOINTC interrupt specifier");
            return Err(rdif_intc::IrqError::InvalidIrq);
        };
        if input >= LIOINTC_VECTOR_COUNT {
            warn!(
                "LIOINTC interrupt input {input} exceeds input count {}",
                LIOINTC_VECTOR_COUNT
            );
            return Err(rdif_intc::IrqError::InvalidIrq);
        }
        Ok(rdif_intc::ControllerIrqTranslation::new(rdif_intc::HwIrq(
            input as u32,
        )))
    }

    fn set_enabled(
        &mut self,
        hwirq: rdif_intc::HwIrq,
        enabled: bool,
    ) -> Result<(), rdif_intc::IrqError> {
        let input = hwirq.0 as usize;
        if !self.contains_input(input, if enabled { "enable" } else { "disable" }) {
            return Err(rdif_intc::IrqError::InvalidIrq);
        }
        FAST_PATH
            .get_initialized()
            .ok_or(rdif_intc::IrqError::Controller)?
            .set_enabled(input, enabled);
        Ok(())
    }
}

struct LioIntcFastPath {
    domain: crate::irq::IrqDomainId,
    regs: MmioRaw,
    isr: MmioRaw,
    parent_irqs: [Option<usize>; PARENT_INT_COUNT],
    enabled: AtomicU32,
    control: SpinIrqSave<()>,
}

impl LioIntcFastPath {
    fn new(
        domain: crate::irq::IrqDomainId,
        regs: MmioRaw,
        isr: MmioRaw,
        parent_irqs: [Option<usize>; PARENT_INT_COUNT],
    ) -> Self {
        Self {
            domain,
            regs,
            isr,
            parent_irqs,
            enabled: AtomicU32::new(0),
            control: SpinIrqSave::new(()),
        }
    }

    fn validate_irq(&self, irq: IrqId) -> Result<(), crate::irq::IrqError> {
        if irq.domain != self.domain || irq.hwirq.0 as usize >= LIOINTC_VECTOR_COUNT {
            Err(crate::irq::IrqError::InvalidIrq)
        } else {
            Ok(())
        }
    }

    fn set_enabled(&self, input: usize, enabled: bool) {
        assert!(
            input < LIOINTC_VECTOR_COUNT,
            "prepared LIOINTC input left the frozen controller range"
        );
        let _guard = self.control.lock();
        let mask = 1u32 << input;
        if enabled {
            self.regs.write(REG_ENABLE, mask);
            self.enabled.fetch_or(mask, Ordering::Release);
        } else {
            self.regs.write(REG_DISABLE, mask);
            self.enabled.fetch_and(!mask, Ordering::Release);
        }
    }

    fn claim_irq(&self, raw: usize) -> Option<IrqId> {
        if !self.parent_irqs.into_iter().flatten().any(|irq| irq == raw) {
            return None;
        }
        let pending: u32 = self.isr.read(0);
        let pending = pending & self.enabled.load(Ordering::Acquire);
        (pending != 0).then(|| IrqId::new(self.domain, crate::irq::HwIrq(pending.trailing_zeros())))
    }

    fn complete_irq(&self, irq: IrqId) {
        assert_eq!(irq.domain, self.domain, "LIOINTC completion domain changed");
        assert!(
            (irq.hwirq.0 as usize) < LIOINTC_VECTOR_COUNT,
            "LIOINTC completion input is outside the frozen range"
        );
        // Inputs are level-triggered; the device-side handler deasserts them.
    }

    fn status(&self, input: usize) -> BoundIrqStatus {
        let bit = 1u32 << input;
        BoundIrqStatus {
            enabled: Some(self.enabled.load(Ordering::Acquire) & bit != 0),
            pending: Some(self.isr.read::<u32>(0) & bit != 0),
            in_service: None,
        }
    }
}

struct LioIrqChipLine {
    irq: IrqId,
    fast_path: &'static LioIntcFastPath,
}

// SAFETY: the fixed fast path owns cloned shutdown-lifetime MMIO mappings and
// is published only after controller initialization. Live writes use W1
// registers under a bounded IRQ-safe lock and cannot allocate or block.
unsafe impl IrqChipLine for LioIrqChipLine {
    fn set_enabled(&self, cpu: Option<CpuId>, enabled: bool) {
        assert!(
            cpu.is_none(),
            "prepared LIOINTC line {:?} cannot use a per-CPU target",
            self.irq
        );
        self.fast_path
            .set_enabled(self.irq.hwirq.0 as usize, enabled);
    }

    fn status(&self, cpu: Option<CpuId>) -> BoundIrqStatus {
        assert!(cpu.is_none(), "LIOINTC status cannot use a per-CPU target");
        self.fast_path.status(self.irq.hwirq.0 as usize)
    }
}
