use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};

use super::irq_common::{PCH_PIC_VECTOR_COUNT, fdt_first_cell_vector, pch_pic_reg_bit};
use crate::{common::ioremap, setup::MmioRaw};

const DEFAULT_PCH_PIC_SIZE: usize = 0x400;

const PCH_PIC_MASK: usize = 0x20;
const PCH_PIC_EDGE: usize = 0x60;
const PCH_PIC_POL: usize = 0x3e0;
const PCH_INT_HTVEC: usize = 0x200;

module_driver!(
    name: "Loongson PCH-PIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,ls7a-pch-pic",
            "loongson,pch-pic-1.0",
            "loongson,pch-pic",
        ],
        on_probe: probe_pch_pic
    }],
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

fn probe_pch_pic(info: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
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
        .unwrap_or(PCH_PIC_VECTOR_COUNT as u32) as usize;
    let mmio = ioremap(
        reg.address,
        reg.size.unwrap_or(DEFAULT_PCH_PIC_SIZE as u64) as usize,
    )
    .map_err(|err| OnProbeError::other(format!("failed to map PCH-PIC: {err:?}")))?;

    let pic = PchPic::new(mmio, base_vector, vector_count);
    pic.init();
    dev.register(rdif_intc::Intc::new(pic));
    Ok(())
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
