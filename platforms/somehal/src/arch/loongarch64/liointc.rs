use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::ProbeFdt,
};

use super::irq_common::{LIOINTC_VECTOR_COUNT, fdt_first_cell_vector};
use crate::{common::ioremap, setup::MmioRaw};

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
    with_liointc("claiming LIOINTC IRQ", |domain, intc| {
        if !intc.handles_cascade_irq(raw) {
            return None;
        }
        intc.claim_irq()
            .map(|input| crate::irq::IrqId::new(domain, crate::irq::HwIrq(input as u32)))
    })
}

pub fn complete_irq(irq: crate::irq::IrqId) {
    with_liointc("completing LIOINTC IRQ", |domain, intc| {
        if domain == irq.domain {
            intc.complete_irq(irq.hwirq.0 as usize);
            Some(())
        } else {
            None
        }
    });
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

fn with_liointc<R>(
    op: &str,
    mut f: impl FnMut(crate::irq::IrqDomainId, &mut LioIntc) -> Option<R>,
) -> Option<R> {
    if !REGISTERED.load(Ordering::Acquire) || !rdrive::is_initialized() {
        return None;
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(mut intc) = intc.try_lock() else {
            warn!("failed to lock LS2K1000 LIOINTC when {op}");
            return None;
        };
        let domain = intc.domain();
        let Some(liointc) = intc.typed_mut::<LioIntc>() else {
            continue;
        };
        if let Some(result) = f(domain, liointc) {
            return Some(result);
        }
    }

    debug!("LS2K1000 LIOINTC has no matching controller when {op}");
    None
}

struct LioIntc {
    regs: MmioRaw,
    isr: MmioRaw,
    enabled: u32,
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
            enabled: 0,
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

    fn handles_cascade_irq(&self, irq: usize) -> bool {
        self.parent_irqs
            .into_iter()
            .flatten()
            .any(|parent| parent == irq)
    }

    fn enable_irq(&mut self, input: usize) -> bool {
        if !self.contains_input(input, "enable") {
            return false;
        }

        let mask = 1u32 << input;
        self.enabled |= mask;
        self.write_reg_u32(REG_ENABLE, mask);
        true
    }

    fn disable_irq(&mut self, input: usize) -> bool {
        if !self.contains_input(input, "disable") {
            return false;
        }

        let mask = 1u32 << input;
        self.enabled &= !mask;
        self.write_reg_u32(REG_DISABLE, mask);
        true
    }

    fn claim_irq(&mut self) -> Option<usize> {
        let pending = self.read_isr_u32(0) & self.enabled;
        (pending != 0).then(|| pending.trailing_zeros() as usize)
    }

    fn complete_irq(&mut self, input: usize) {
        let _ = self.contains_input(input, "complete");
        // LIOINTC inputs are level-triggered here; the device-side handler
        // clears the source. There is no controller EOI register to write.
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

    fn read_isr_u32(&self, offset: usize) -> u32 {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.isr.size());
        self.isr.read(offset)
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
        if enabled {
            self.enable_irq(input);
        } else {
            self.disable_irq(input);
        }
        Ok(())
    }
}
