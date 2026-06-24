use loongArch64::iocsr::{iocsr_read_d, iocsr_write_d, iocsr_write_w};
use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver,
    probe::OnProbeError,
    register::{ProbeAcpi, ProbeFdt},
};

use super::irq_common::{EIOINTC_VECTOR_COUNT, eiointc_reg_bit, fdt_first_cell_vector};

const EIOINTC_IRQ: usize = 3;

const LOONGARCH_IOCSR_MISC_FUNC: usize = 0x420;
const IOCSR_MISC_FUNC_EXT_IOI_EN: u64 = 1 << 48;

const EIOINTC_REG_NODEMAP: usize = 0x14a0;
const EIOINTC_REG_IPMAP: usize = 0x14c0;
const EIOINTC_REG_ENABLE: usize = 0x1600;
const EIOINTC_REG_BOUNCE: usize = 0x1680;
const EIOINTC_REG_ISR: usize = 0x1800;
const EIOINTC_REG_ROUTE: usize = 0x1c00;

const VEC_COUNT_PER_REG: usize = 64;

module_driver!(
    name: "Loongson EIOINTC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "loongson,ls2k2000-eiointc",
                "loongson,ls3a5000-eiointc",
                "loongson,eiointc",
            ],
            on_probe: probe_eiointc_fdt
        },
        ProbeKind::Acpi {
            ids: &[],
            on_probe: probe_eiointc_acpi
        },
    ],
);

pub fn set_irq_enable(irq: usize, enable: bool) {
    with_eiointc("setting EIOINTC IRQ enable", |intc| {
        if enable {
            intc.enable_irq(irq);
        } else {
            intc.disable_irq(irq);
        }
    });
}

pub fn claim_irq() -> Option<usize> {
    with_eiointc("claiming EIOINTC IRQ", |intc| intc.claim_irq()).flatten()
}

pub fn complete_irq(irq: usize) {
    with_eiointc("completing EIOINTC IRQ", |intc| intc.complete_irq(irq));
}

fn probe_eiointc_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    register_eiointc(probe.into_platform_device())
}

fn probe_eiointc_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    if probe.info().root.routing().pch_pics().is_empty() {
        return Err(OnProbeError::NotMatch);
    }
    register_eiointc(probe.into_platform_device())
}

fn register_eiointc(dev: PlatformDevice) -> Result<(), OnProbeError> {
    let intc = EioIntc::new(EIOINTC_VECTOR_COUNT);
    intc.init();
    someboot::irq::irq_set_enable(someboot::irq::IrqId::new(EIOINTC_IRQ), true);
    dev.register(rdif_intc::Intc::new(intc));
    Ok(())
}

fn with_eiointc<R>(op: &str, f: impl FnOnce(&mut EioIntc) -> R) -> Option<R> {
    if !rdrive::is_initialized() {
        return None;
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(intc) = intc.downcast::<EioIntc>() else {
            continue;
        };
        let Ok(mut intc) = intc.try_lock() else {
            warn!("failed to lock Loongson EIOINTC when {op}");
            return None;
        };
        return Some(f(&mut intc));
    }

    warn!("Loongson EIOINTC is not registered when {op}");
    None
}

struct EioIntc {
    vectors: usize,
}

impl EioIntc {
    const fn new(vectors: usize) -> Self {
        Self { vectors }
    }

    fn init(&self) {
        let misc = iocsr_read_d(LOONGARCH_IOCSR_MISC_FUNC);
        iocsr_write_d(LOONGARCH_IOCSR_MISC_FUNC, misc | IOCSR_MISC_FUNC_EXT_IOI_EN);

        let index = 0;

        for i in 0..(self.vectors / 32) {
            let data = ((1 << (i * 2 + 1)) << 16) | (1 << (i * 2));
            iocsr_write_w(EIOINTC_REG_NODEMAP + i * 4, data);
        }
        for i in 0..(self.vectors / 32 / 4) {
            let bit = 1 << (1 + index);
            let data = bit | (bit << 8) | (bit << 16) | (bit << 24);
            iocsr_write_w(EIOINTC_REG_IPMAP + i * 4, data);
        }
        for i in 0..(self.vectors / 4) {
            let bit = 1;
            let data = bit | (bit << 8) | (bit << 16) | (bit << 24);
            iocsr_write_w(EIOINTC_REG_ROUTE + i * 4, data);
        }
        for i in 0..(self.vectors / 32) {
            iocsr_write_w(EIOINTC_REG_BOUNCE + i * 4, u32::MAX);
        }
    }

    fn enable_irq(&mut self, irq: usize) {
        if !self.contains_irq(irq, "enable") {
            return;
        }

        let (offset, bit) = eiointc_reg_bit(irq);
        for base in [EIOINTC_REG_ENABLE, EIOINTC_REG_BOUNCE] {
            let addr = base + offset;
            iocsr_write_d(addr, iocsr_read_d(addr) | bit);
        }
    }

    fn disable_irq(&mut self, irq: usize) {
        if !self.contains_irq(irq, "disable") {
            return;
        }

        let (offset, bit) = eiointc_reg_bit(irq);
        let addr = EIOINTC_REG_ENABLE + offset;
        iocsr_write_d(addr, iocsr_read_d(addr) & !bit);
    }

    fn claim_irq(&mut self) -> Option<usize> {
        for i in 0..(self.vectors / VEC_COUNT_PER_REG) {
            let flags = iocsr_read_d(EIOINTC_REG_ISR + i * 8);
            if flags != 0 {
                return Some(flags.trailing_zeros() as usize + VEC_COUNT_PER_REG * i);
            }
        }
        None
    }

    fn complete_irq(&mut self, irq: usize) {
        if !self.contains_irq(irq, "complete") {
            return;
        }

        let (offset, bit) = eiointc_reg_bit(irq);
        iocsr_write_d(EIOINTC_REG_ISR + offset, bit);
    }

    fn contains_irq(&self, irq: usize, op: &str) -> bool {
        if irq < self.vectors {
            true
        } else {
            warn!("skip {op} for out-of-range EIOINTC IRQ {irq}");
            false
        }
    }
}

impl DriverGeneric for EioIntc {
    fn name(&self) -> &str {
        "Loongson EIOINTC"
    }
}

impl Interface for EioIntc {
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> rdrive::IrqId {
        let Some(vector) = fdt_first_cell_vector(irq_prop) else {
            warn!("empty EIOINTC interrupt specifier");
            return 0usize.into();
        };
        if vector >= self.vectors {
            warn!(
                "EIOINTC interrupt vector {vector} exceeds vector count {}",
                self.vectors
            );
        }
        vector.into()
    }
}
