use alloc::{boxed::Box, format};
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use ax_kspin::SpinIrqSave;
use irq_framework::{CpuId, IrqId, IrqScope};
use loongArch64::iocsr::{iocsr_read_d, iocsr_write_d, iocsr_write_w};
use rdif_intc::Interface;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver,
    probe::OnProbeError,
    register::{ProbeAcpi, ProbeFdt},
};

use super::irq_common::{EIOINTC_VECTOR_COUNT, eiointc_reg_bit, fdt_first_cell_vector};
use crate::irq_line::{BoundIrqStatus, IrqChipLine, PreparedIrqChipLine};

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

static EIOINTC_RUNTIME_VECTOR_COUNT: AtomicUsize = AtomicUsize::new(0);
static EIOINTC_CONTROL: SpinIrqSave<()> = SpinIrqSave::new(());
static EIOINTC_LINE_OWNERS: [AtomicU8; EIOINTC_VECTOR_COUNT] =
    [const { AtomicU8::new(0) }; EIOINTC_VECTOR_COUNT];

const LINE_OWNER_DIRECT: u8 = 1;
const LINE_OWNER_PCH_PIC: u8 = 2;

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

pub(super) fn prepare_irq_line(
    irq: IrqId,
    scope: IrqScope,
) -> Result<PreparedIrqChipLine, crate::irq::IrqError> {
    if scope != IrqScope::Global {
        return Err(crate::irq::IrqError::InvalidIrq);
    }
    let vector = checked_vector(irq.hwirq.0 as usize)?;
    reserve_line(vector, LINE_OWNER_DIRECT)?;
    set_prepared_irq_enabled(vector, false);
    Ok(PreparedIrqChipLine::maskable(Box::new(EioIrqChipLine {
        irq,
        vector,
    })))
}

pub(super) fn reserve_pch_pic_vector(vector: usize) -> Result<(), crate::irq::IrqError> {
    let vector = checked_vector(vector)?;
    reserve_line(vector, LINE_OWNER_PCH_PIC)
}

pub(super) fn set_prepared_irq_enabled(vector: usize, enabled: bool) {
    assert!(
        vector < EIOINTC_RUNTIME_VECTOR_COUNT.load(Ordering::Acquire),
        "prepared EIOINTC vector left the frozen controller range"
    );
    let _guard = EIOINTC_CONTROL.lock();
    let (offset, bit) = eiointc_reg_bit(vector);
    let enable_addr = EIOINTC_REG_ENABLE + offset;
    let current = iocsr_read_d(enable_addr);
    iocsr_write_d(
        enable_addr,
        if enabled {
            current | bit
        } else {
            current & !bit
        },
    );
    if enabled {
        let bounce_addr = EIOINTC_REG_BOUNCE + offset;
        iocsr_write_d(bounce_addr, iocsr_read_d(bounce_addr) | bit);
    }
}

fn checked_vector(vector: usize) -> Result<usize, crate::irq::IrqError> {
    let count = EIOINTC_RUNTIME_VECTOR_COUNT.load(Ordering::Acquire);
    if count == 0 {
        return Err(crate::irq::IrqError::Unsupported);
    }
    (vector < count)
        .then_some(vector)
        .ok_or(crate::irq::IrqError::InvalidIrq)
}

fn reserve_line(vector: usize, owner: u8) -> Result<(), crate::irq::IrqError> {
    EIOINTC_LINE_OWNERS[vector]
        .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| crate::irq::IrqError::Busy)
}

struct EioIrqChipLine {
    irq: IrqId,
    vector: usize,
}

// SAFETY: registration permanently reserves the physical vector and the IOCSR
// register block is architectural shutdown-lifetime state. Live RMW sequences
// use one IRQ-safe bounded lock and never allocate or enter rdrive.
unsafe impl IrqChipLine for EioIrqChipLine {
    fn set_enabled(&self, cpu: Option<CpuId>, enabled: bool) {
        assert!(
            cpu.is_none(),
            "prepared EIOINTC line {:?} cannot use a per-CPU target",
            self.irq
        );
        assert_eq!(
            EIOINTC_LINE_OWNERS[self.vector].load(Ordering::Acquire),
            LINE_OWNER_DIRECT,
            "prepared EIOINTC line lost its physical source lease"
        );
        set_prepared_irq_enabled(self.vector, enabled);
    }

    fn status(&self, cpu: Option<CpuId>) -> BoundIrqStatus {
        assert!(cpu.is_none(), "EIOINTC status cannot use a per-CPU target");
        let _guard = EIOINTC_CONTROL.lock();
        let (offset, bit) = eiointc_reg_bit(self.vector);
        BoundIrqStatus {
            enabled: Some(iocsr_read_d(EIOINTC_REG_ENABLE + offset) & bit != 0),
            pending: Some(iocsr_read_d(EIOINTC_REG_ISR + offset) & bit != 0),
            in_service: None,
        }
    }
}

pub fn claim_irq() -> Option<usize> {
    let vectors = EIOINTC_RUNTIME_VECTOR_COUNT.load(Ordering::Acquire);
    if vectors == 0 {
        return None;
    }

    for i in 0..vectors.div_ceil(VEC_COUNT_PER_REG) {
        let flags = iocsr_read_d(EIOINTC_REG_ISR + i * 8);
        if flags == 0 {
            continue;
        }
        let irq = flags.trailing_zeros() as usize + VEC_COUNT_PER_REG * i;
        if irq < vectors {
            return Some(irq);
        }
    }
    None
}

pub fn complete_irq(irq: usize) {
    if irq >= EIOINTC_RUNTIME_VECTOR_COUNT.load(Ordering::Acquire) {
        return;
    }

    let (offset, bit) = eiointc_reg_bit(irq);
    iocsr_write_d(EIOINTC_REG_ISR + offset, bit);
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
    EIOINTC_RUNTIME_VECTOR_COUNT.store(intc.vectors, Ordering::Release);
    someboot::irq::irq_set_enable(someboot::irq::IrqId::new(EIOINTC_IRQ), true);
    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchEioIntc,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register EIOINTC domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, intc));
    Ok(())
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

        set_prepared_irq_enabled(irq, true);
    }

    fn disable_irq(&mut self, irq: usize) {
        if !self.contains_irq(irq, "disable") {
            return;
        }

        set_prepared_irq_enabled(irq, false);
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
    fn translate_fdt(
        &self,
        irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        let Some(vector) = fdt_first_cell_vector(irq_prop) else {
            warn!("empty EIOINTC interrupt specifier");
            return Err(rdif_intc::IrqError::InvalidIrq);
        };
        if vector >= self.vectors {
            warn!(
                "EIOINTC interrupt vector {vector} exceeds vector count {}",
                self.vectors
            );
            return Err(rdif_intc::IrqError::InvalidIrq);
        }
        Ok(rdif_intc::ControllerIrqTranslation::new(rdif_intc::HwIrq(
            vector as u32,
        )))
    }

    fn set_enabled(
        &mut self,
        hwirq: rdif_intc::HwIrq,
        enabled: bool,
    ) -> Result<(), rdif_intc::IrqError> {
        let irq = hwirq.0 as usize;
        if !self.contains_irq(irq, if enabled { "enable" } else { "disable" }) {
            return Err(rdif_intc::IrqError::InvalidIrq);
        }
        if enabled {
            self.enable_irq(irq);
        } else {
            self.disable_irq(irq);
        }
        Ok(())
    }
}
