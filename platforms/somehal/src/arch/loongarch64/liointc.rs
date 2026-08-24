use core::sync::atomic::{AtomicBool, Ordering};

use kernutil::StaticCell;
use loongarch_intc_driver::{
    CpuIrqLine, LIO_PARENT_COUNT, LIO_PARENT_FIRST_CPU_LINE, LioInput, LioIntcConfig,
    LioIntcCpuInterface, LioIntcParts,
};
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::ProbeFdt};

use crate::{common::ioremap, setup::MmioRaw};

const DEFAULT_LIOINTC_PADDR: usize = 0x1fe0_1400;
const DEFAULT_LIOINTC_SIZE: usize = 0x40;
const DEFAULT_LIOINTC_ISR_PADDR: usize = 0x1fe0_1040;
const DEFAULT_LIOINTC_ISR_SIZE: usize = 0x10;
const DEFAULT_CASCADE_IRQ: usize = 2;

struct LioRuntime {
    domain: crate::irq::IrqDomainId,
    cpu_interface: LioIntcCpuInterface,
}

static RUNTIME: StaticCell<LioRuntime> = StaticCell::uninit();
static REGISTERED: AtomicBool = AtomicBool::new(false);

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

pub fn is_cascade_irq(raw: usize) -> bool {
    let Some(runtime) = runtime() else {
        return false;
    };
    CpuIrqLine::new(raw).is_ok_and(|line| runtime.cpu_interface.is_parent(line))
}

pub fn claim_irq(raw: usize) -> Option<crate::irq::IrqId> {
    let runtime = runtime()?;
    let line = CpuIrqLine::new(raw).ok()?;
    let input = runtime.cpu_interface.claim(line)?;
    Some(crate::irq::IrqId::new(
        runtime.domain,
        crate::irq::HwIrq(input.raw() as u32),
    ))
}

pub fn complete_irq(irq: crate::irq::IrqId) {
    let Some(runtime) = runtime() else {
        return;
    };
    if irq.domain != runtime.domain {
        warn!("ignore completion for foreign LIOINTC domain {irq:?}");
        return;
    }
    let Ok(input) = LioInput::new(irq.hwirq.0 as usize) else {
        warn!("ignore completion for invalid LIOINTC input {irq:?}");
        return;
    };
    runtime.cpu_interface.complete(input);
}

fn runtime() -> Option<&'static LioRuntime> {
    REGISTERED.load(Ordering::Acquire).then(|| &*RUNTIME)
}

fn probe_liointc_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let mut regions = info.node.regs().into_iter();
    let regs = regions.next();
    let isr = regions.next();

    let regs = LioIntcMmioRegion {
        address: regs
            .as_ref()
            .map(|region| region.address as usize)
            .unwrap_or(DEFAULT_LIOINTC_PADDR),
        size: regs
            .as_ref()
            .and_then(|region| region.size)
            .unwrap_or(DEFAULT_LIOINTC_SIZE as u64) as usize,
    };
    let isr = LioIntcMmioRegion {
        address: isr
            .as_ref()
            .map(|region| region.address as usize)
            .unwrap_or(DEFAULT_LIOINTC_ISR_PADDR),
        size: isr
            .as_ref()
            .and_then(|region| region.size)
            .unwrap_or(DEFAULT_LIOINTC_ISR_SIZE as u64) as usize,
    };
    let parent_lines = parent_lines_from_fdt(&info)?;
    let parent_input_maps = parent_input_maps_from_fdt(&info, &parent_lines);
    let config = LioIntcConfig::new(parent_lines, parent_input_maps)
        .map_err(|error| OnProbeError::other(format!("invalid LIOINTC config: {error}")))?;

    register_liointc(dev, info.node.name(), regs, isr, config)
}

#[derive(Clone, Copy)]
struct LioIntcMmioRegion {
    address: usize,
    size: usize,
}

fn register_liointc(
    dev: PlatformDevice,
    node_name: &str,
    regs_region: LioIntcMmioRegion,
    isr_region: LioIntcMmioRegion,
    config: LioIntcConfig,
) -> Result<(), OnProbeError> {
    if REGISTERED.load(Ordering::Acquire) {
        return Err(OnProbeError::other(
            "LS2K1000 LIOINTC is already registered",
        ));
    }

    let regs = map_liointc_mmio(regs_region, "register")?;
    let isr = map_liointc_mmio(isr_region, "ISR")?;
    let regs_pointer = regs.as_ptr() as usize;
    let isr_pointer = isr.as_ptr() as usize;
    let LioIntcParts {
        controller,
        cpu_interface,
    } = LioIntcParts::new(regs, isr, config)
        .map_err(|error| OnProbeError::other(format!("failed to initialize LIOINTC: {error}")))?;

    debug!(
        "probing LS2K1000 LIOINTC: node={}, regs={:#x}->{regs_pointer:#x}, \
         isr={:#x}->{isr_pointer:#x}, parent_lines={:?}, parent_int_map={:#x?}",
        node_name,
        regs_region.address,
        isr_region.address,
        config.parent_lines(),
        config.parent_input_maps(),
    );

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchLioIntc,
    )
    .map_err(|error| {
        OnProbeError::other(format!("failed to register LIOINTC domain: {error:?}"))
    })?;
    let parent_lines = cpu_interface.parent_lines();

    dev.register(rdif_intc::Intc::new(domain, controller));
    RUNTIME.init(LioRuntime {
        domain,
        cpu_interface,
    });
    // Publish the CPU interface and domain only after the RDIF controller is
    // visible. Parent cascades remain disabled until this release completes.
    REGISTERED.store(true, Ordering::Release);
    for line in parent_lines.into_iter().flatten() {
        super::boot_irq_set_enable(someboot::irq::IrqId::new(line.raw()), true);
    }
    Ok(())
}

fn parent_lines_from_fdt(
    info: &rdrive::register::FdtInfo<'_>,
) -> Result<[Option<CpuIrqLine>; LIO_PARENT_COUNT], OnProbeError> {
    let mut parent_lines = [None; LIO_PARENT_COUNT];
    let mut any = false;

    for interrupt in info.interrupts() {
        let Some(raw) = interrupt.specifier.first().copied() else {
            continue;
        };
        set_parent_line(&mut parent_lines, raw as usize)?;
        any = true;
    }

    if !any && let Some(property) = info.node.as_node().get_property("interrupts") {
        for raw in property.get_u32_iter() {
            set_parent_line(&mut parent_lines, raw as usize)?;
            any = true;
        }
    }

    if !any {
        set_parent_line(&mut parent_lines, DEFAULT_CASCADE_IRQ)?;
    }
    Ok(parent_lines)
}

fn set_parent_line(
    parent_lines: &mut [Option<CpuIrqLine>; LIO_PARENT_COUNT],
    raw: usize,
) -> Result<(), OnProbeError> {
    let line = CpuIrqLine::new(raw).map_err(|error| {
        OnProbeError::other(format!("invalid LIOINTC parent CPU line {raw}: {error}"))
    })?;
    let Some(slot) = raw.checked_sub(LIO_PARENT_FIRST_CPU_LINE) else {
        warn!("LIOINTC parent IRQ {raw} is below INT0; using the default INT0 cascade");
        parent_lines[0] = Some(CpuIrqLine::new(DEFAULT_CASCADE_IRQ).map_err(|error| {
            OnProbeError::other(format!("invalid default LIOINTC parent: {error}"))
        })?);
        return Ok(());
    };
    if slot >= LIO_PARENT_COUNT {
        warn!("LIOINTC parent IRQ {raw} is above INT3; using the default INT0 cascade");
        parent_lines[0] = Some(CpuIrqLine::new(DEFAULT_CASCADE_IRQ).map_err(|error| {
            OnProbeError::other(format!("invalid default LIOINTC parent: {error}"))
        })?);
        return Ok(());
    }
    parent_lines[slot] = Some(line);
    Ok(())
}

fn parent_input_maps_from_fdt(
    info: &rdrive::register::FdtInfo<'_>,
    parent_lines: &[Option<CpuIrqLine>; LIO_PARENT_COUNT],
) -> [u32; LIO_PARENT_COUNT] {
    let mut maps = [0; LIO_PARENT_COUNT];
    if let Some(property) = info.node.as_node().get_property("loongson,parent_int_map") {
        for (slot, map) in property.get_u32_iter().take(LIO_PARENT_COUNT).enumerate() {
            if parent_lines[slot].is_some() {
                maps[slot] = map;
            } else if map != 0 {
                warn!("ignore LIOINTC parent_int_map[{slot}]={map:#x} without a parent line");
            }
        }
    }
    if maps.iter().all(|map| *map == 0) {
        let slot = parent_lines.iter().position(Option::is_some).unwrap_or(0);
        maps[slot] = u32::MAX;
    }
    maps
}

fn map_liointc_mmio(region: LioIntcMmioRegion, name: &str) -> Result<MmioRaw, OnProbeError> {
    if region.size == 0 {
        return Err(OnProbeError::other(format!(
            "LS2K1000 LIOINTC {name} region has zero size"
        )));
    }
    ioremap(region.address as u64, region.size).map_err(|error| {
        OnProbeError::other(format!(
            "failed to map LS2K1000 LIOINTC {name} region: {error:?}"
        ))
    })
}
