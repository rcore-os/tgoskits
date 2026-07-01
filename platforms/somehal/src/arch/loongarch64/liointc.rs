use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::ProbeFdt,
};

use super::irq_common::{LIOINTC_VECTOR_COUNT, fdt_first_cell_vector};

const DEFAULT_LIOINTC_PADDR: usize = 0x1fe0_1400;
const DEFAULT_LIOINTC_SIZE: usize = 0x40;
const DEFAULT_LIOINTC_ISR_PADDR: usize = 0x1fe0_1040;
const DEFAULT_LIOINTC_ISR_SIZE: usize = 0x10;
const DEFAULT_CASCADE_IRQ: usize = 2;

const LOONGARCH_PADDR_MASK: usize = (1usize << 48) - 1;
const LOONGARCH_UNCACHED_DMW_BASE: usize = 0x8000_0000_0000_0000;

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
static CASCADE_IRQ: AtomicUsize = AtomicUsize::new(usize::MAX);

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
    REGISTERED.load(Ordering::Acquire) && CASCADE_IRQ.load(Ordering::Acquire) == irq
}

pub fn claim_irq() -> Option<crate::irq::IrqId> {
    let input = with_liointc("claiming LIOINTC IRQ", |intc| intc.claim_irq()).flatten()?;
    liointc_irq(input)
}

pub fn complete_irq(input: usize) {
    with_liointc("completing LIOINTC IRQ", |intc| intc.complete_irq(input));
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
    let cascade_irq = cascade_irq_from_fdt(&info).unwrap_or(DEFAULT_CASCADE_IRQ);

    register_liointc(
        dev,
        info.node.name(),
        reg_addr,
        reg_size,
        isr_addr,
        isr_size,
        cascade_irq,
    )
}

fn register_liointc(
    dev: PlatformDevice,
    node_name: &str,
    reg_addr: usize,
    reg_size: usize,
    isr_addr: usize,
    isr_size: usize,
    cascade_irq: usize,
) -> Result<(), OnProbeError> {
    let reg_paddr = firmware_addr_to_phys(reg_addr);
    let isr_paddr = firmware_addr_to_phys(isr_addr);
    let intc = LioIntc::new(reg_paddr, reg_size, isr_paddr, isr_size, cascade_irq)?;
    intc.init();

    info!(
        "probing LS2K1000 LIOINTC: node={}, regs={reg_addr:#x}->{:#x}, isr={isr_addr:#x}->{:#x}, \
         cascade_irq={}, route={:#04x}, inputs={}",
        node_name,
        intc.regs.as_ptr() as usize,
        intc.isr.as_ptr() as usize,
        intc.cascade_irq,
        intc.route_value(),
        LIOINTC_VECTOR_COUNT,
    );

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchLioIntc,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register LIOINTC domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, intc));
    CASCADE_IRQ.store(cascade_irq, Ordering::Release);
    REGISTERED.store(true, Ordering::Release);
    someboot::irq::irq_set_enable(someboot::irq::IrqId::new(cascade_irq), true);
    Ok(())
}

fn liointc_irq(input: usize) -> Option<crate::irq::IrqId> {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::LoongArchLioIntc)?;
    Some(crate::irq::IrqId::new(
        domain,
        crate::irq::HwIrq(input as u32),
    ))
}

fn cascade_irq_from_fdt(info: &rdrive::register::FdtInfo<'_>) -> Option<usize> {
    info.interrupts()
        .into_iter()
        .next()
        .and_then(|interrupt| fdt_first_cell_vector(&interrupt.specifier))
        .or_else(|| {
            info.node
                .as_node()
                .get_property("interrupts")
                .and_then(|prop| prop.get_u32_iter().next())
                .map(|irq| irq as usize)
        })
}

fn firmware_addr_to_phys(addr: usize) -> usize {
    addr & LOONGARCH_PADDR_MASK
}

fn uncached_ptr(paddr: usize, size: usize, name: &str) -> Result<NonNull<u8>, OnProbeError> {
    if size == 0 {
        return Err(OnProbeError::other(format!(
            "LS2K1000 LIOINTC {name} region has zero size"
        )));
    }
    NonNull::new((LOONGARCH_UNCACHED_DMW_BASE | firmware_addr_to_phys(paddr)) as *mut u8)
        .ok_or_else(|| OnProbeError::other(format!("LS2K1000 LIOINTC {name} pointer is null")))
}

fn route_int_bit(cascade_irq: usize) -> u8 {
    let Some(input) = cascade_irq.checked_sub(CPU_HWI_BASE_IRQ) else {
        warn!(
            "LIOINTC cascade IRQ {cascade_irq} is below CPU HWI base {CPU_HWI_BASE_IRQ}; routing \
             through INT0"
        );
        return 1 << ROUTE_INT_SHIFT;
    };
    if input >= ROUTE_INT_COUNT {
        warn!(
            "LIOINTC cascade IRQ {cascade_irq} exceeds routeable HWI range; routing through INT0"
        );
        return 1 << ROUTE_INT_SHIFT;
    }
    1 << (ROUTE_INT_SHIFT + input)
}

fn with_liointc<R>(op: &str, f: impl FnOnce(&mut LioIntc) -> R) -> Option<R> {
    if !REGISTERED.load(Ordering::Acquire) || !rdrive::is_initialized() {
        return None;
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(intc) = intc.downcast::<LioIntc>() else {
            continue;
        };
        let Ok(mut intc) = intc.try_lock() else {
            warn!("failed to lock LS2K1000 LIOINTC when {op}");
            return None;
        };
        return Some(f(&mut intc));
    }

    warn!("LS2K1000 LIOINTC is not registered when {op}");
    None
}

struct LioIntc {
    regs: NonNull<u8>,
    isr: NonNull<u8>,
    reg_size: usize,
    isr_size: usize,
    enabled: u32,
    cascade_irq: usize,
}

unsafe impl Send for LioIntc {}

impl LioIntc {
    fn new(
        reg_paddr: usize,
        reg_size: usize,
        isr_paddr: usize,
        isr_size: usize,
        cascade_irq: usize,
    ) -> Result<Self, OnProbeError> {
        Ok(Self {
            regs: uncached_ptr(reg_paddr, reg_size, "register")?,
            isr: uncached_ptr(isr_paddr, isr_size, "ISR")?,
            reg_size,
            isr_size,
            enabled: 0,
            cascade_irq,
        })
    }

    fn init(&self) {
        for irq in 0..LIOINTC_VECTOR_COUNT {
            self.write_route(irq, self.route_value());
        }

        self.write_reg_u32(REG_DISABLE, u32::MAX);
        self.write_reg_u32(REG_EDGE, 0);
        // LIOINTC POL=0 selects active-high level interrupts.
        self.write_reg_u32(REG_POLARITY, 0);
    }

    fn route_value(&self) -> u8 {
        ROUTE_CPU0 | route_int_bit(self.cascade_irq)
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
        if !self.contains_input(input, "complete") {
            return;
        }
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
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.reg_size);
        unsafe {
            (self.regs.as_ptr().add(offset) as *mut u32).write_volatile(value);
        }
    }

    fn read_isr_u32(&self, offset: usize) -> u32 {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.isr_size);
        unsafe { (self.isr.as_ptr().add(offset) as *const u32).read_volatile() }
    }

    fn write_route(&self, irq: usize, value: u8) {
        debug_assert!(ROUTE_BASE + irq < self.reg_size);
        unsafe {
            self.regs
                .as_ptr()
                .add(ROUTE_BASE + irq)
                .write_volatile(value);
        }
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
