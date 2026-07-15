use kernutil::StaticCell;
use rdif_intc::{AcpiGsiController, AcpiIrqPolarity, AcpiIrqTrigger, Interface};
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver,
    probe::{OnProbeError, acpi::AcpiPchPic},
    register::{ProbeAcpi, ProbeFdt},
};

use super::irq_common::{PCH_PIC_VECTOR_COUNT, fdt_first_cell_vector, pch_pic_reg_bit};
use crate::{
    common::ioremap,
    irq_routing::{
        AcpiControllerRoutes, PchPicCpuInterface, acknowledge_pch_pic_child, pch_pic_ack_registers,
        valid_pch_pic_vector_window,
    },
    setup::MmioRaw,
};

const DEFAULT_PCH_PIC_SIZE: usize = 0x400;

const PCH_PIC_ID_HI: usize = 0x04;
const PCH_PIC_MASK: usize = 0x20;
const PCH_PIC_HTMSI_EN: usize = 0x40;
const PCH_PIC_EDGE: usize = 0x60;
const PCH_PIC_CLEAR: usize = 0x80;
const PCH_PIC_AUTO0: usize = 0xc0;
const PCH_PIC_AUTO1: usize = 0xe0;
const PCH_INT_ROUTE: usize = 0x100;
const PCH_PIC_POL: usize = 0x3e0;
const PCH_INT_HTVEC: usize = 0x200;
const PCH_PIC_REQUIRED_MMIO_SIZE: usize = PCH_PIC_POL + 2 * core::mem::size_of::<u32>();

// Installed as one write-before-release object while every PCH input is masked
// and retained until shutdown. Keeping route lookup and child acknowledgement
// in the same publication prevents a hard IRQ from observing a half-installed
// controller fast path.
static FAST_PATH: StaticCell<PchPicFastPath> = StaticCell::uninit();

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PchPicInput(usize);

pub(super) fn acknowledge_external_vector(vector: usize) -> Option<rdif_intc::IrqId> {
    FAST_PATH
        .get_initialized()?
        .acknowledge_external_vector(vector)
}

fn cpu_interface() -> Option<&'static PchPicCpuInterface> {
    FAST_PATH
        .get_initialized()
        .map(|fast_path| &fast_path.cpu_interface)
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
    if FAST_PATH.is_init() {
        return Err(OnProbeError::other(
            "multiple PCH-PIC instances are not supported by the singleton CPU fast path",
        ));
    }
    if mmio.size() < PCH_PIC_REQUIRED_MMIO_SIZE {
        return Err(OnProbeError::other(format!(
            "PCH-PIC MMIO region is too small: {:#x} < {PCH_PIC_REQUIRED_MMIO_SIZE:#x}",
            mmio.size()
        )));
    }
    let detected_vector_count = detect_vector_count(&mmio).unwrap_or(PCH_PIC_VECTOR_COUNT);
    let vector_count = vector_count.unwrap_or(detected_vector_count);
    if !valid_pch_pic_vector_window(base_vector, vector_count) {
        return Err(OnProbeError::other(format!(
            "invalid PCH-PIC vector window: base={base_vector}, count={vector_count}"
        )));
    }
    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchPchPic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register PCH-PIC domain: {err:?}")))?;
    let controller_address = mmio.phys_addr().as_usize() as u64;
    let pic = PchPic::new(mmio, base_vector, vector_count);
    pic.init();
    FAST_PATH.init(PchPicFastPath {
        cpu_interface: PchPicCpuInterface::new(
            domain,
            AcpiGsiController::PchPic,
            controller_address,
            base_vector,
            vector_count,
        ),
        completion: PchPicCompletionEndpoint::new(pic.mmio.clone(), vector_count),
    });
    dev.register(rdif_intc::Intc::new(domain, pic));
    Ok(())
}

fn detect_vector_count(mmio: &MmioRaw) -> Option<usize> {
    let count = (((mmio.read::<u32>(PCH_PIC_ID_HI) >> 16) & 0xff) as usize).saturating_add(1);
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

struct PchPicCompletionEndpoint {
    mmio: MmioRaw,
    vector_count: usize,
}

struct PchPicFastPath {
    cpu_interface: PchPicCpuInterface,
    completion: PchPicCompletionEndpoint,
}

impl PchPicFastPath {
    fn acknowledge_external_vector(&self, vector: usize) -> Option<rdif_intc::IrqId> {
        let (input, irq) = self.cpu_interface.resolve_external_vector(vector)?;
        self.completion.ack_input(PchPicInput(input));
        Some(irq)
    }
}

impl PchPicCompletionEndpoint {
    fn new(mmio: MmioRaw, vector_count: usize) -> Self {
        Self { mmio, vector_count }
    }

    fn ack_input(&self, input: PchPicInput) {
        let registers = pch_pic_ack_registers(input.0, self.vector_count)
            .expect("resolved PCH-PIC input must remain within the frozen vector window");
        // Match Linux's hierarchical irqchip semantics: only an edge source
        // owns a latched child cause. Clear it before dispatch so a second edge
        // arriving during the action can be latched independently. A level
        // source is deasserted by its device and must not be cleared here.
        acknowledge_pch_pic_child(
            registers,
            self.mmio.read(registers.edge),
            |clear, bit| self.mmio.write(clear, bit),
            super::device_write_barrier,
        );
    }
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
        for input in 0..self.routes.vector_count() {
            let vector = self
                .routes
                .vector_for_input(input)
                .expect("PCH-PIC input must have a hardware vector");
            self.write_b(PCH_INT_ROUTE + input, 1);
            self.write_b(
                PCH_INT_HTVEC + input,
                u8::try_from(vector).expect("validated PCH-PIC vector must fit in a byte"),
            );
        }
        for offset in [0, core::mem::size_of::<u32>()] {
            self.write_w(PCH_PIC_MASK + offset, u32::MAX);
            self.write_w(PCH_PIC_CLEAR + offset, u32::MAX);
            self.write_w(PCH_PIC_AUTO0 + offset, 0);
            self.write_w(PCH_PIC_AUTO1 + offset, 0);
            self.write_w(PCH_PIC_HTMSI_EN + offset, u32::MAX);
            self.write_w(PCH_PIC_EDGE + offset, 0);
            self.write_w(PCH_PIC_POL + offset, 0);
        }
    }

    fn prepare_enable_irq(&mut self, irq: usize) -> bool {
        let Some(input) = self.input_for_vector(irq, "enable") else {
            return false;
        };
        self.write_b(
            PCH_INT_HTVEC + input,
            u8::try_from(irq).expect("validated PCH-PIC vector must fit in a byte"),
        );
        self.clear_input(input);
        true
    }

    fn unmask_irq(&mut self, irq: usize) {
        let Some(input) = self.input_for_vector(irq, "unmask") else {
            return;
        };
        let (offset, bit) = pch_pic_reg_bit(input);
        let addr = PCH_PIC_MASK + offset;
        self.write_w(addr, self.read_w(addr) & !bit);
    }

    fn disable_irq(&mut self, irq: usize) {
        let Some(input) = self.input_for_vector(irq, "disable") else {
            return;
        };
        let (offset, bit) = pch_pic_reg_bit(input);
        let addr = PCH_PIC_MASK + offset;
        self.write_w(addr, self.read_w(addr) | bit);
    }

    fn clear_input(&self, input: usize) {
        let Some(registers) = pch_pic_ack_registers(input, self.routes.vector_count()) else {
            return;
        };
        self.write_w(registers.clear, registers.bit);
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
        let Some(cpu_if) = cpu_interface() else {
            return Err(rdif_intc::IrqError::Controller);
        };
        cpu_if.remember_route(route, translation.id)?;
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
        if enabled {
            if !self.prepare_enable_irq(vector) {
                return Err(rdif_intc::IrqError::InvalidIrq);
            }
            super::eiointc::set_irq_enable(vector, true)?;
            self.unmask_irq(vector);
        } else {
            self.disable_irq(vector);
            super::eiointc::set_irq_enable(vector, false)?;
        }
        Ok(())
    }
}
