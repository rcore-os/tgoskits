use alloc::{format, vec::Vec};
use core::{num::NonZeroU32, ptr::NonNull};

use ax_riscv_plic::{PLICRegs, Plic};
use kernutil::StaticCell;
use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, Phandle, PlatformDevice, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::FdtInfo,
};
use riscv::register::{sie, sip};
use sbi_rt::HartMask;
use spin::Mutex;

use crate::{common::ioremap, irq::_handle_irq};

const INTC_IRQ_BASE: usize = 1usize << (usize::BITS as usize - 1);
const S_SOFT: usize = INTC_IRQ_BASE | 1;
const S_TIMER: usize = INTC_IRQ_BASE | 5;
const S_EXT: usize = INTC_IRQ_BASE | 9;
const SUPERVISOR_EXTERNAL_INTERRUPT: u32 = 9;
const DEFAULT_PRIORITY: u32 = 1;
const DEFAULT_PLIC_SIZE: usize = 0x400_0000;

static PLIC: StaticCell<Mutex<RiscvPlic>> = StaticCell::uninit();

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

pub fn irq_handler_with_raw(raw: usize) -> Option<someboot::irq::IrqId> {
    match raw {
        S_TIMER => {
            _handle_irq(S_TIMER.into());
            Some(S_TIMER.into())
        }
        S_SOFT => {
            unsafe {
                sip::clear_ssoft();
            }
            _handle_irq(S_SOFT.into());
            Some(S_SOFT.into())
        }
        S_EXT => handle_external_irq(),
        external if external & INTC_IRQ_BASE == 0 => {
            _handle_irq(external.into());
            Some(external.into())
        }
        other => {
            warn!("unsupported RISC-V interrupt cause {other:#x}");
            None
        }
    }
}

pub fn secondary_init_intc() {
    enable_local_interrupts();
    if PLIC.is_init() {
        unsafe {
            PLIC.update(|plic| plic.lock().init_current_context());
        }
    }
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

fn probe_plic(info: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
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

    let mut plic = RiscvPlic {
        inner: plic,
        context_by_cpu: contexts,
        sources: ndev,
    };
    plic.init_current_context();
    PLIC.init(Mutex::new(plic));
    enable_local_interrupts();

    dev.register(rdif_intc::Intc::new(RiscvPlicIntc));
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
    if !PLIC.is_init() {
        warn!("PLIC is not initialized when setting IRQ {irq}");
        return;
    }
    let Some(source) = NonZeroU32::new(irq as u32) else {
        return;
    };
    unsafe {
        PLIC.update(|plic| {
            let mut plic = plic.lock();
            if enable {
                plic.enable_source(source);
            } else {
                plic.disable_source(source);
            }
        });
    }
}

fn handle_external_irq() -> Option<someboot::irq::IrqId> {
    if !PLIC.is_init() {
        warn!("PLIC is not initialized for external IRQ");
        return None;
    }
    unsafe { PLIC.update(|plic| plic.lock().handle_external_irq()) }
}

struct RiscvPlic {
    inner: Plic,
    context_by_cpu: Vec<Option<usize>>,
    sources: usize,
}

impl RiscvPlic {
    fn current_context(&self) -> Option<usize> {
        let cpu_idx = someboot::smp::cpu_idx();
        self.context_by_cpu.get(cpu_idx).and_then(|ctx| *ctx)
    }

    fn init_current_context(&mut self) {
        if let Some(context) = self.current_context() {
            self.inner.init_by_context(context);
            trace!("PLIC context {context} initialized");
        } else {
            warn!(
                "PLIC supervisor context for logical CPU {} is not found",
                someboot::smp::cpu_idx()
            );
        }
    }

    fn enable_source(&mut self, source: NonZeroU32) {
        if source.get() as usize > self.sources {
            warn!("skip enabling out-of-range PLIC source {}", source.get());
            return;
        }
        self.inner.set_priority(source, DEFAULT_PRIORITY);
        let current = self.current_context();
        for context in self.context_by_cpu.iter().filter_map(|context| *context) {
            self.inner.enable(source, context);
        }
        if current.is_none() {
            warn!(
                "PLIC supervisor context for logical CPU {} is not found",
                someboot::smp::cpu_idx()
            );
        }
    }

    fn disable_source(&mut self, source: NonZeroU32) {
        for context in self.context_by_cpu.iter().filter_map(|context| *context) {
            self.inner.disable(source, context);
        }
    }

    fn handle_external_irq(&mut self) -> Option<someboot::irq::IrqId> {
        let context = self.current_context()?;
        let Some(source) = self.inner.claim(context) else {
            debug!("Spurious external IRQ");
            return None;
        };
        let irq = someboot::irq::IrqId::new(source.get() as usize);
        _handle_irq(irq.raw().into());
        self.inner.complete(context, source);
        Some(irq)
    }
}

struct RiscvPlicIntc;

impl DriverGeneric for RiscvPlicIntc {
    fn name(&self) -> &str {
        "RISC-V PLIC"
    }
}

impl Interface for RiscvPlicIntc {
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> rdrive::IrqId {
        (irq_prop.first().copied().unwrap_or(0) as usize).into()
    }
}
