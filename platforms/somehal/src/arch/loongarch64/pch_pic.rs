use rdif_intc::{AcpiIrqPolarity, AcpiIrqTrigger, Interface};
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver,
    probe::{
        OnProbeError,
        acpi::{AcpiGsiController, AcpiPchPic},
    },
    register::{ProbeAcpi, ProbeFdt},
};

use super::irq_common::{PCH_PIC_VECTOR_COUNT, fdt_first_cell_vector, pch_pic_reg_bit};
use crate::{common::ioremap, setup::MmioRaw};

const DEFAULT_PCH_PIC_SIZE: usize = 0x400;

const PCH_PIC_ID: usize = 0x00;
const PCH_PIC_MASK: usize = 0x20;
const PCH_PIC_HTMSI_EN: usize = 0x40;
const PCH_PIC_EDGE: usize = 0x60;
const PCH_PIC_CLR: usize = 0x80;
const PCH_INT_ROUTE: usize = 0x100;
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

pub fn set_irq_enable(irq: usize, enable: bool) {
    with_pch_pic("setting PCH-PIC IRQ enable", |pic| {
        if enable {
            pic.enable_irq(irq);
        } else {
            pic.disable_irq(irq);
        }
    });
}

pub fn debug_summary(inputs: &[usize]) {
    with_pch_pic("debugging PCH-PIC state", |pic| {
        let mask0 = pic.read_w(PCH_PIC_MASK);
        let mask1 = pic.read_w(PCH_PIC_MASK + 4);
        let htmsi0 = pic.read_w(PCH_PIC_HTMSI_EN);
        let htmsi1 = pic.read_w(PCH_PIC_HTMSI_EN + 4);
        let edge0 = pic.read_w(PCH_PIC_EDGE);
        let edge1 = pic.read_w(PCH_PIC_EDGE + 4);
        let pol0 = pic.read_w(PCH_PIC_POL);
        let pol1 = pic.read_w(PCH_PIC_POL + 4);
        let clr0 = pic.read_w(PCH_PIC_CLR);
        let clr1 = pic.read_w(PCH_PIC_CLR + 4);
        info!(
            "LoongArch PCH-PIC summary: base_vector={}, vector_count={}, mask=[{:#x},{:#x}], \
             htmsi=[{:#x},{:#x}], edge=[{:#x},{:#x}], pol=[{:#x},{:#x}], clr=[{:#x},{:#x}]",
            pic.base_vector,
            pic.vector_count,
            mask0,
            mask1,
            htmsi0,
            htmsi1,
            edge0,
            edge1,
            pol0,
            pol1,
            clr0,
            clr1
        );
        for &input in inputs {
            if input >= pic.vector_count {
                continue;
            }
            let route = pic.read_b(PCH_INT_ROUTE + input);
            let htvec = pic.read_b(PCH_INT_HTVEC + input);
            info!("LoongArch PCH-PIC input {input}: route={route:#x}, htvec={htvec:#x}");
        }
    });
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
    let pic = PchPic::new(
        mmio,
        base_vector,
        vector_count.unwrap_or(detected_vector_count),
    );
    pic.init();
    dev.register(rdif_intc::Intc::new(pic));
    Ok(())
}

fn detect_vector_count(mmio: &MmioRaw) -> Option<usize> {
    let count = (((mmio.read::<u64>(PCH_PIC_ID) >> 48) & 0xff) as usize).saturating_add(1);
    (count <= PCH_PIC_VECTOR_COUNT).then_some(count)
}

fn with_pch_pic<R>(op: &str, f: impl FnOnce(&mut PchPic) -> R) -> Option<R> {
    if !rdrive::is_initialized() {
        return None;
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(intc) = intc.downcast::<PchPic>() else {
            continue;
        };
        let Ok(mut intc) = intc.try_lock() else {
            warn!("failed to lock Loongson PCH-PIC when {op}");
            return None;
        };
        return Some(f(&mut intc));
    }

    warn!("Loongson PCH-PIC is not registered when {op}");
    None
}

struct PchPic {
    mmio: MmioRaw,
    base_vector: usize,
    vector_count: usize,
}

impl PchPic {
    fn new(mmio: MmioRaw, base_vector: usize, vector_count: usize) -> Self {
        Self {
            mmio,
            base_vector,
            vector_count,
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

    fn input_for_vector(&self, vector: usize, op: &str) -> Option<usize> {
        let Some(input) = vector.checked_sub(self.base_vector) else {
            warn!(
                "skip {op} for PCH-PIC vector {vector} below base {}",
                self.base_vector
            );
            return None;
        };

        if input < self.vector_count {
            Some(input)
        } else {
            warn!(
                "skip {op} for out-of-range PCH-PIC vector {vector}, base {}, count {}",
                self.base_vector, self.vector_count
            );
            None
        }
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

    fn vector_for_acpi_route(&self, route: &rdif_intc::AcpiGsiRoute) -> Option<usize> {
        if route.controller != AcpiGsiController::PchPic {
            return None;
        }
        let input = usize::from(route.controller_input);
        if input < self.vector_count {
            Some(self.base_vector + input)
        } else {
            None
        }
    }

    fn read_w(&self, offset: usize) -> u32 {
        self.mmio.read(offset)
    }

    fn read_b(&self, offset: usize) -> u8 {
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
        route.controller_address == self.mmio.phys_addr().as_usize() as u64
            && self.vector_for_acpi_route(route).is_some()
    }

    fn setup_irq_by_acpi(&mut self, route: &rdif_intc::AcpiGsiRoute) -> rdrive::IrqId {
        let Some(vector) = self.vector_for_acpi_route(route) else {
            warn!(
                "unsupported ACPI PCH-PIC route: controller={:?} address={:#x} input={}",
                route.controller, route.controller_address, route.controller_input
            );
            return self.base_vector.into();
        };
        info!(
            "LoongArch ACPI PCH-PIC route: gsi={}, input={}, vector={}, trigger={:?}, \
             polarity={:?}",
            route.gsi, route.controller_input, vector, route.trigger, route.polarity
        );
        self.configure_input(usize::from(route.controller_input), route);
        vector.into()
    }

    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> rdrive::IrqId {
        let Some(input) = fdt_first_cell_vector(irq_prop) else {
            warn!("empty PCH-PIC interrupt specifier");
            return self.base_vector.into();
        };
        if input >= self.vector_count {
            warn!(
                "PCH-PIC interrupt input {input} exceeds vector count {}",
                self.vector_count
            );
        }
        (self.base_vector + input).into()
    }
}
