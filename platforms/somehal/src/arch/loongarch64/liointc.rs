use core::sync::atomic::{AtomicBool, Ordering};

use kernutil::StaticCell;
use loongarch_intc_driver::{
    CpuIrqLine, LIO_PARENT_COUNT, LIO_PARENT_FIRST_CPU_LINE, LioInput, LioIntcConfig,
    LioIntcCpuInterface, LioIntcParts,
};
use rdrive::{
    DeviceId, PlatformDevice, module_driver,
    probe::{
        OnProbeError,
        acpi::{AcpiEioPic, AcpiLioPic, AcpiResourceAddress},
    },
    register::{ProbeAcpi, ProbeFdt},
};

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
    name: "Loongson LIOINTC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "loongson,2k1000-icu",
                "loongson,ls2k1000-icu",
                "loongson,liointc",
            ],
            on_probe: probe_liointc_fdt,
        },
        ProbeKind::Acpi {
            ids: &[],
            on_probe: probe_liointc_acpi,
        },
    ],
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

pub fn resolve_acpi_route(
    route: &rdif_intc::AcpiGsiRoute,
) -> Result<rdif_intc::IrqId, rdif_intc::IrqError> {
    if route.controller != rdif_intc::AcpiGsiController::LioPic {
        return Err(rdif_intc::IrqError::Unsupported);
    }
    if !rdrive::is_initialized() {
        return Err(rdif_intc::IrqError::Controller);
    }

    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::LoongArchLioIntc)
        .ok_or(rdif_intc::IrqError::Unsupported)?;
    let intc = crate::irq::intc_by_domain(domain)?;
    let mut intc = intc.try_lock().map_err(|_| rdif_intc::IrqError::Busy)?;
    if !intc.supports_acpi_gsi(route) {
        return Err(rdif_intc::IrqError::Unsupported);
    }
    let translation = intc.translate_acpi(route)?;
    intc.configure_acpi(&translation, route)?;
    Ok(translation.id)
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

fn probe_liointc_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let routing = probe.info().root.routing();
    let lio_pics = routing.lio_pics();
    if lio_pics.is_empty() {
        return Err(OnProbeError::NotMatch);
    }
    if lio_pics.len() != 1 {
        return Err(OnProbeError::other(format!(
            "expected one ACPI LIOPIC, found {}",
            lio_pics.len()
        )));
    }

    let lio_pic = lio_pics[0];
    let config = acpi_liointc_config(lio_pic, routing.eio_pics())?;
    let physical_core_id = someboot::smp::early_current_hart_id();
    if physical_core_id != 0 {
        return Err(OnProbeError::other(format!(
            "ACPI LIOINTC startup is only supported on physical core 0, current core is \
             {physical_core_id}"
        )));
    }
    let (regs, isr) = acpi_liointc_regions(lio_pic, physical_core_id)?;
    let owner = probe.device_id();
    let resource_address = AcpiResourceAddress::memory(lio_pic.address);
    let domain = register_liointc_with_owner(
        owner,
        move |intc| probe.register_root_resource_device(resource_address, intc),
        "ACPI LIOPIC",
        regs,
        isr,
        config,
    )?;

    info!(
        "registered Loongson LIOINTC: physical_core_id={physical_core_id}, registers={:#x}+{:#x}, \
         isr={:#x}+{:#x}, parent_lines={:?}, parent_input_maps={:#x?}, domain={domain:?}",
        regs.address,
        regs.size,
        isr.address,
        isr.size,
        config.parent_lines(),
        config.parent_input_maps(),
    );
    Ok(())
}

fn acpi_liointc_config(
    lio_pic: AcpiLioPic,
    eio_pics: &[AcpiEioPic],
) -> Result<LioIntcConfig, OnProbeError> {
    if lio_pic.version != 1 {
        return Err(OnProbeError::other(format!(
            "unsupported ACPI LIOPIC version {}",
            lio_pic.version
        )));
    }
    if eio_pics.is_empty() {
        return Err(OnProbeError::other(
            "ACPI LIOPIC has no EIOPIC cascade metadata",
        ));
    }

    for eio_pic in eio_pics {
        if eio_pic.version != 1 {
            return Err(OnProbeError::other(format!(
                "unsupported ACPI EIOPIC version {}",
                eio_pic.version
            )));
        }
        if usize::from(eio_pic.cascade) != super::EIOINTC_IRQ {
            return Err(OnProbeError::other(format!(
                "ACPI EIOPIC cascade line {} does not match the configured CPU line {}",
                eio_pic.cascade,
                super::EIOINTC_IRQ
            )));
        }
        CpuIrqLine::new(usize::from(eio_pic.cascade)).map_err(|error| {
            OnProbeError::other(format!(
                "invalid ACPI EIOPIC cascade CPU line {}: {error}",
                eio_pic.cascade
            ))
        })?;
    }

    let mut parent_lines = [None; LIO_PARENT_COUNT];
    let mut parent_input_maps = [0; LIO_PARENT_COUNT];
    for slot in 0..lio_pic.cascade_map.len() {
        let input_map = lio_pic.cascade_map[slot];
        if input_map == 0 {
            continue;
        }

        let raw_line = usize::from(lio_pic.cascade[slot]);
        let last_parent_line = LIO_PARENT_FIRST_CPU_LINE + LIO_PARENT_COUNT - 1;
        if !(LIO_PARENT_FIRST_CPU_LINE..=last_parent_line).contains(&raw_line) {
            return Err(OnProbeError::other(format!(
                "ACPI LIOPIC parent slot {slot} has unsupported CPU line {raw_line}"
            )));
        }
        if eio_pics
            .iter()
            .any(|eio_pic| usize::from(eio_pic.cascade) == raw_line)
        {
            return Err(OnProbeError::other(format!(
                "ACPI LIOPIC parent CPU line {raw_line} conflicts with an EIOPIC cascade"
            )));
        }
        parent_lines[slot] = Some(CpuIrqLine::new(raw_line).map_err(|error| {
            OnProbeError::other(format!(
                "invalid ACPI LIOPIC parent CPU line {raw_line}: {error}"
            ))
        })?);
        parent_input_maps[slot] = input_map;
    }

    LioIntcConfig::from_firmware_routes(parent_lines, parent_input_maps)
        .map_err(|error| OnProbeError::other(format!("invalid ACPI LIOPIC routes: {error}")))
}

fn acpi_liointc_regions(
    lio_pic: AcpiLioPic,
    physical_core_id: usize,
) -> Result<(LioIntcMmioRegion, LioIntcMmioRegion), OnProbeError> {
    if physical_core_id != 0 {
        return Err(OnProbeError::other(format!(
            "ACPI LIOINTC ISR layout is unsupported on physical core {physical_core_id}"
        )));
    }

    let mmio_size = u64::from(lio_pic.mmio_size);
    if lio_pic.address == 0 || mmio_size == 0 {
        return Err(OnProbeError::other(format!(
            "invalid ACPI LIOPIC MMIO range: address={:#x}, size={mmio_size:#x}",
            lio_pic.address
        )));
    }
    let region_end = lio_pic
        .address
        .checked_add(mmio_size)
        .ok_or_else(|| OnProbeError::other("ACPI LIOPIC MMIO range overflows"))?;
    let physical_core_id = u64::try_from(physical_core_id)
        .map_err(|_| OnProbeError::other("physical LIOPIC core ID does not fit u64"))?;
    let isr_offset = physical_core_id
        .checked_mul(8)
        .and_then(|offset| 0x40u64.checked_add(offset))
        .ok_or_else(|| OnProbeError::other("ACPI LIOPIC ISR offset overflows"))?;
    let isr_end_offset = isr_offset
        .checked_add(4)
        .ok_or_else(|| OnProbeError::other("ACPI LIOPIC ISR range overflows"))?;
    if isr_end_offset > mmio_size {
        return Err(OnProbeError::other(format!(
            "ACPI LIOPIC ISR range {isr_offset:#x}..{isr_end_offset:#x} exceeds MMIO size \
             {mmio_size:#x}"
        )));
    }
    let isr_address = lio_pic
        .address
        .checked_add(isr_offset)
        .ok_or_else(|| OnProbeError::other("ACPI LIOPIC ISR address overflows"))?;
    let isr_end = isr_address
        .checked_add(4)
        .ok_or_else(|| OnProbeError::other("ACPI LIOPIC ISR end address overflows"))?;
    if isr_end > region_end {
        return Err(OnProbeError::other(format!(
            "ACPI LIOPIC ISR range {isr_address:#x}..{isr_end:#x} exceeds MMIO range \
             {:#x}..{region_end:#x}",
            lio_pic.address
        )));
    }

    Ok((
        LioIntcMmioRegion {
            address: usize::try_from(lio_pic.address)
                .map_err(|_| OnProbeError::other("ACPI LIOPIC base does not fit usize"))?,
            size: usize::try_from(mmio_size)
                .map_err(|_| OnProbeError::other("ACPI LIOPIC size does not fit usize"))?,
        },
        LioIntcMmioRegion {
            address: usize::try_from(isr_address)
                .map_err(|_| OnProbeError::other("ACPI LIOPIC ISR address does not fit usize"))?,
            size: 4,
        },
    ))
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
    let owner = dev.descriptor.device_id();
    register_liointc_with_owner(
        owner,
        move |intc| dev.register(intc),
        node_name,
        regs_region,
        isr_region,
        config,
    )
    .map(|_| ())
}

fn register_liointc_with_owner(
    owner: DeviceId,
    register_controller: impl FnOnce(rdif_intc::Intc),
    node_name: &str,
    regs_region: LioIntcMmioRegion,
    isr_region: LioIntcMmioRegion,
    config: LioIntcConfig,
) -> Result<crate::irq::IrqDomainId, OnProbeError> {
    if REGISTERED.load(Ordering::Acquire) {
        return Err(OnProbeError::other(
            "Loongson LIOINTC is already registered",
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
        "probing Loongson LIOINTC: node={}, regs={:#x}->{regs_pointer:#x}, \
         isr={:#x}->{isr_pointer:#x}, parent_lines={:?}, parent_int_map={:#x?}",
        node_name,
        regs_region.address,
        isr_region.address,
        config.parent_lines(),
        config.parent_input_maps(),
    );

    let domain = crate::irq::alloc_irq_domain(owner, crate::irq::IrqDomainKind::LoongArchLioIntc)
        .map_err(|error| {
        OnProbeError::other(format!("failed to register LIOINTC domain: {error:?}"))
    })?;
    let parent_lines = cpu_interface.parent_lines();

    register_controller(rdif_intc::Intc::new(domain, controller));
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
    Ok(domain)
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
            "Loongson LIOINTC {name} region has zero size"
        )));
    }
    ioremap(region.address as u64, region.size).map_err(|error| {
        OnProbeError::other(format!(
            "failed to map Loongson LIOINTC {name} region: {error:?}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_lio_pic() -> AcpiLioPic {
        AcpiLioPic {
            version: 1,
            address: 0x1fe0_1400,
            mmio_size: 0x80,
            cascade: [2, 0],
            cascade_map: [0x00ff_ffff, 0],
        }
    }

    fn test_eio_pic(cascade: u8) -> AcpiEioPic {
        AcpiEioPic {
            version: 1,
            cascade,
            node: 0,
            node_map: 0x3,
        }
    }

    #[test]
    fn acpi_lio_config_uses_only_the_firmware_mapped_parent() {
        let config = acpi_liointc_config(test_lio_pic(), &[test_eio_pic(3)]).unwrap();

        assert_eq!(
            config.parent_lines().map(|line| line.map(CpuIrqLine::raw)),
            [Some(2), None, None, None]
        );
        assert_eq!(config.parent_input_maps(), [0x00ff_ffff, 0, 0, 0]);
        assert!(config.is_input_routed(LioInput::new(10).unwrap()));
        assert!(!config.is_input_routed(LioInput::new(24).unwrap()));
    }

    #[test]
    fn acpi_lio_config_rejects_missing_unsupported_and_conflicting_metadata() {
        let lio_pic = test_lio_pic();
        assert!(acpi_liointc_config(lio_pic, &[]).is_err());
        assert!(
            acpi_liointc_config(
                AcpiLioPic {
                    version: 2,
                    ..lio_pic
                },
                &[test_eio_pic(3)]
            )
            .is_err()
        );
        assert!(
            acpi_liointc_config(
                AcpiLioPic {
                    cascade: [2, 3],
                    cascade_map: [1, 1],
                    ..lio_pic
                },
                &[test_eio_pic(3)]
            )
            .is_err()
        );
        assert!(
            acpi_liointc_config(
                lio_pic,
                &[AcpiEioPic {
                    version: 2,
                    ..test_eio_pic(3)
                }]
            )
            .is_err()
        );
        assert!(acpi_liointc_config(lio_pic, &[test_eio_pic(2)]).is_err());
    }

    #[test]
    fn acpi_lio_isr_uses_physical_core_zero_and_stays_in_declared_mmio() {
        let (regs, isr) = acpi_liointc_regions(test_lio_pic(), 0).unwrap();
        assert_eq!((regs.address, regs.size), (0x1fe0_1400, 0x80));
        assert_eq!((isr.address, isr.size), (0x1fe0_1440, 4));

        assert!(acpi_liointc_regions(test_lio_pic(), 1).is_err());
        assert!(
            acpi_liointc_regions(
                AcpiLioPic {
                    mmio_size: 0x43,
                    ..test_lio_pic()
                },
                0
            )
            .is_err()
        );
        assert!(
            acpi_liointc_regions(
                AcpiLioPic {
                    address: u64::MAX - 0x3f,
                    ..test_lio_pic()
                },
                0
            )
            .is_err()
        );
    }
}
