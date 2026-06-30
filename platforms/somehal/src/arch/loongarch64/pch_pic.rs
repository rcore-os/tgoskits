use rdif_intc::{AcpiGsiController, AcpiIrqPolarity, AcpiIrqTrigger, Interface};
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver,
    probe::{OnProbeError, acpi::AcpiPchPic},
    register::{ProbeAcpi, ProbeFdt},
};

use super::irq_common::{PCH_PIC_VECTOR_COUNT, fdt_first_cell_vector, pch_pic_reg_bit};
use crate::{common::ioremap, irq_routing::AcpiControllerRoutes, setup::MmioRaw};

const DEFAULT_PCH_PIC_SIZE: usize = 0x400;

const PCH_PIC_ID: usize = 0x00;
const PCH_PIC_MASK: usize = 0x20;
const PCH_PIC_EDGE: usize = 0x60;
const PCH_PIC_POL: usize = 0x3e0;
const PCH_INT_HTVEC: usize = 0x200;

module_driver!(
    name: "Loongson PCH-PIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "loongson,ls7a-pch-pic",
                "loongson,pch-pic-1.0",
                "loongson,pch-pic",
            ],
            on_probe: probe_pch_pic_fdt
        },
        ProbeKind::Acpi {
            ids: &[],
            on_probe: probe_pch_pic_acpi
        },
    ],
);

pub fn irq_for_external_vector(
    vector: usize,
) -> Result<Option<rdif_intc::IrqId>, rdif_intc::IrqError> {
    if !rdrive::is_initialized() {
        return Ok(None);
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(pic) = intc.downcast::<PchPic>() else {
            continue;
        };
        let domain = crate::irq::domain_by_owner(intc.descriptor().device_id())
            .map(|domain| domain.id)
            .ok_or(rdif_intc::IrqError::Unsupported)?;
        let Ok(pic) = pic.try_lock() else {
            warn!("failed to lock Loongson PCH-PIC when resolving vector {vector}");
            return Err(rdif_intc::IrqError::Busy);
        };
        if let Some(irq) = pic.irq_for_external_vector(vector) {
            return Ok(Some(irq));
        }
        if let Some(input) = pic.input_for_external_vector(vector) {
            return Ok(Some(rdif_intc::IrqId::new(
                domain,
                rdif_intc::HwIrq(input as u32),
            )));
        }
    }

    Ok(None)
}

pub fn resolve_acpi_route(
    route: &rdif_intc::AcpiGsiRoute,
) -> Result<rdif_intc::IrqId, rdif_intc::IrqError> {
    let intc = pch_pic_controller_for_route(route)?;
    let mut intc = intc.try_lock().map_err(|_| rdif_intc::IrqError::Busy)?;
    if !intc.supports_acpi_gsi(route) {
        return Err(rdif_intc::IrqError::Unsupported);
    }
    let translation = intc.translate_acpi(route)?;
    intc.configure_acpi(&translation, route)?;
    Ok(translation.id)
}

fn probe_pch_pic_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let base_vector = info
        .node
        .as_node()
        .get_property("loongson,pic-base-vec")
        .and_then(|prop| prop.get_u32())
        .unwrap_or(0) as usize;
    let vector_count = info
        .node
        .as_node()
        .get_property("loongson,pic-num-vecs")
        .and_then(|prop| prop.get_u32())
        .map(|count| count as usize);
    let mmio = ioremap(
        reg.address,
        reg.size.unwrap_or(DEFAULT_PCH_PIC_SIZE as u64) as usize,
    )
    .map_err(|err| OnProbeError::other(format!("failed to map PCH-PIC: {err:?}")))?;

    register_pch_pic(dev, mmio, base_vector, vector_count)
}

fn probe_pch_pic_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let mut registered = false;

    for pch_pic in info.root.routing().pch_pics() {
        register_acpi_pch_pic(
            PlatformDevice {
                descriptor: dev.descriptor.clone(),
            },
            *pch_pic,
        )?;
        registered = true;
    }

    if registered {
        Ok(())
    } else {
        Err(OnProbeError::NotMatch)
    }
}

fn register_acpi_pch_pic(dev: PlatformDevice, info: AcpiPchPic) -> Result<(), OnProbeError> {
    let size = if info.mmio_size == 0 {
        DEFAULT_PCH_PIC_SIZE
    } else {
        usize::from(info.mmio_size)
    };
    let mmio = ioremap(info.address, size)
        .map_err(|err| OnProbeError::other(format!("failed to map ACPI PCH-PIC: {err:?}")))?;
    register_pch_pic(dev, mmio, 0, None)
}

fn register_pch_pic(
    dev: PlatformDevice,
    mmio: MmioRaw,
    base_vector: usize,
    vector_count: Option<usize>,
) -> Result<(), OnProbeError> {
    let detected_vector_count = detect_vector_count(&mmio).unwrap_or(PCH_PIC_VECTOR_COUNT);
    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchPchPic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register PCH-PIC domain: {err:?}")))?;
    let pic = PchPic::new(
        mmio,
        base_vector,
        vector_count.unwrap_or(detected_vector_count),
    );
    pic.init();
    dev.register(rdif_intc::Intc::new(domain, pic));
    Ok(())
}

fn detect_vector_count(mmio: &MmioRaw) -> Option<usize> {
    let count = (((mmio.read::<u64>(PCH_PIC_ID) >> 48) & 0xff) as usize).saturating_add(1);
    (count <= PCH_PIC_VECTOR_COUNT).then_some(count)
}

fn pch_pic_controller_for_route(
    route: &rdif_intc::AcpiGsiRoute,
) -> Result<rdrive::Device<rdif_intc::Intc>, rdif_intc::IrqError> {
    if !rdrive::is_initialized() {
        return Err(rdif_intc::IrqError::Controller);
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(pic) = intc.downcast::<PchPic>() else {
            continue;
        };
        let Ok(guard) = pic.try_lock() else {
            warn!("failed to lock Loongson PCH-PIC when resolving ACPI route");
            return Err(rdif_intc::IrqError::Busy);
        };
        let supported = guard.supports_acpi_gsi(route);
        drop(guard);
        if supported {
            return Ok(intc);
        }
    }

    warn!(
        "Loongson PCH-PIC is not registered for ACPI route controller={:?} address={:#x} input={}",
        route.controller, route.controller_address, route.controller_input
    );
    Err(rdif_intc::IrqError::Unsupported)
}

struct PchPic {
    mmio: MmioRaw,
    routes: AcpiControllerRoutes,
}

impl PchPic {
    fn new(mmio: MmioRaw, base_vector: usize, vector_count: usize) -> Self {
        let controller_address = mmio.phys_addr().as_usize() as u64;
        Self {
            mmio,
            routes: AcpiControllerRoutes::new(
                AcpiGsiController::PchPic,
                controller_address,
                base_vector,
                vector_count,
            ),
        }
    }

    fn init(&self) {
        self.write_w(PCH_PIC_EDGE, 0);
        self.write_w(PCH_PIC_EDGE + 4, 0);
        self.write_w(PCH_PIC_POL, 0);
        self.write_w(PCH_PIC_POL + 4, 0);
    }

    fn enable_irq(&mut self, irq: usize) {
        let Some(input) = self.input_for_vector(irq, "enable") else {
            return;
        };
        let (offset, bit) = pch_pic_reg_bit(input);

        let addr = PCH_PIC_MASK + offset;
        self.write_w(addr, self.read_w(addr) & !bit);
        self.write_b(PCH_INT_HTVEC + input, irq as u8);
    }

    fn disable_irq(&mut self, irq: usize) {
        let Some(input) = self.input_for_vector(irq, "disable") else {
            return;
        };
        let (offset, bit) = pch_pic_reg_bit(input);
        let addr = PCH_PIC_MASK + offset;
        self.write_w(addr, self.read_w(addr) | bit);
    }

    fn vector_for_input(&self, input: usize) -> Option<usize> {
        self.routes.vector_for_input(input)
    }

    fn input_for_vector(&self, vector: usize, op: &str) -> Option<usize> {
        let input = self.routes.input_for_vector(vector);
        if input.is_none() {
            warn!(
                "skip {op} for out-of-range PCH-PIC vector {vector}, vector count {}",
                self.routes.vector_count()
            );
        }
        input
    }

    fn configure_input(&mut self, input: usize, route: &rdif_intc::AcpiGsiRoute) {
        let (offset, bit) = pch_pic_reg_bit(input);
        let edge_addr = PCH_PIC_EDGE + offset;
        let pol_addr = PCH_PIC_POL + offset;

        let edge = self.read_w(edge_addr);
        let edge = match route.trigger {
            AcpiIrqTrigger::Edge => edge | bit,
            AcpiIrqTrigger::Level => edge & !bit,
        };
        self.write_w(edge_addr, edge);

        let pol = self.read_w(pol_addr);
        let pol = match route.polarity {
            AcpiIrqPolarity::ActiveHigh => pol & !bit,
            AcpiIrqPolarity::ActiveLow => pol | bit,
        };
        self.write_w(pol_addr, pol);
    }

    fn irq_for_external_vector(&self, vector: usize) -> Option<rdif_intc::IrqId> {
        self.routes.irq_for_external_vector(vector)
    }

    fn input_for_external_vector(&self, vector: usize) -> Option<usize> {
        self.routes.input_for_vector(vector)
    }

    fn read_w(&self, offset: usize) -> u32 {
        self.mmio.read(offset)
    }

    fn write_w(&self, offset: usize, value: u32) {
        self.mmio.write(offset, value);
    }

    fn write_b(&self, offset: usize, value: u8) {
        self.mmio.write(offset, value);
    }
}

impl DriverGeneric for PchPic {
    fn name(&self) -> &str {
        "Loongson PCH-PIC"
    }
}

impl Interface for PchPic {
    fn supports_acpi_gsi(&self, route: &rdif_intc::AcpiGsiRoute) -> bool {
        self.routes.supports_acpi_gsi(route)
    }

    fn translate_acpi(
        &self,
        route: &rdif_intc::AcpiGsiRoute,
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        if !self.supports_acpi_gsi(route) {
            warn!(
                "unsupported ACPI PCH-PIC route: controller={:?} address={:#x} input={}",
                route.controller, route.controller_address, route.controller_input
            );
            return Err(rdif_intc::IrqError::Unsupported);
        }
        Ok(rdif_intc::ControllerIrqTranslation::new(rdif_intc::HwIrq(
            u32::from(route.controller_input),
        )))
    }

    fn configure_acpi(
        &mut self,
        translation: &rdif_intc::IrqTranslation,
        route: &rdif_intc::AcpiGsiRoute,
    ) -> Result<(), rdif_intc::IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(rdif_intc::IrqError::Unsupported);
        }
        if translation.id.hwirq != rdif_intc::HwIrq(u32::from(route.controller_input)) {
            return Err(rdif_intc::IrqError::InvalidIrq);
        }
        self.routes.remember_route(route, translation.id)?;
        self.configure_input(usize::from(route.controller_input), route);
        Ok(())
    }

    fn translate_fdt(
        &self,
        irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        let Some(input) = fdt_first_cell_vector(irq_prop) else {
            warn!("empty PCH-PIC interrupt specifier");
            return Err(rdif_intc::IrqError::InvalidIrq);
        };
        if input >= self.routes.vector_count() {
            warn!(
                "PCH-PIC interrupt input {input} exceeds vector count {}",
                self.routes.vector_count()
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
        let Some(vector) = self.vector_for_input(input) else {
            warn!("skip {enabled} for out-of-range PCH-PIC input {input}");
            return Err(rdif_intc::IrqError::InvalidIrq);
        };
        super::eiointc::set_irq_enable(vector, enabled)?;
        if enabled {
            self.enable_irq(vector);
        } else {
            self.disable_irq(vector);
        }
        Ok(())
    }
}
