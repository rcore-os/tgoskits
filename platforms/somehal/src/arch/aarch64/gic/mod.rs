use arm_gic_driver::fdt_parse_irq_config;
use irq_framework::{IrqDomainId, IrqId};
use rdif_intc::{Intc, Interface};
use rdrive::Device;

mod its;
mod v2;
mod v3;

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Eq, PartialEq)]
enum GicBackend {
    None = 0,
    V2   = 2,
    V3   = 3,
}

static GIC_BACKEND: AtomicU8 = AtomicU8::new(GicBackend::None as u8);

fn set_backend(backend: GicBackend) {
    GIC_BACKEND.store(backend as u8, Ordering::Release);
}

fn backend() -> GicBackend {
    match GIC_BACKEND.load(Ordering::Acquire) {
        2 => GicBackend::V2,
        3 => GicBackend::V3,
        _ => GicBackend::None,
    }
}

pub fn init_current_cpu() {
    let cpu_idx = crate::cpu::current_cpu_idx()
        .unwrap_or_else(|| panic!("current logical CPU index is not available for GIC init"));
    init_cpu(cpu_idx);
}

pub fn init_cpu(cpu_idx: usize) {
    match backend() {
        GicBackend::V2 => v2::init_cpu(cpu_idx),
        GicBackend::V3 => v3::init_cpu(cpu_idx),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::init_cpu(cpu_idx);
            } else {
                v2::init_cpu(cpu_idx);
            }
        }
    }
}

fn get_primary_gicd() -> Result<Device<Intc>, crate::irq::IrqError> {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::AArch64Gic)
        .ok_or(crate::irq::IrqError::Unsupported)?;
    crate::irq::intc_by_domain(domain)
}

pub fn with_gic_domain<T: Interface, R>(
    domain: IrqDomainId,
    f: impl FnOnce(&mut T) -> R,
) -> Result<R, crate::irq::IrqError> {
    let mut intc = crate::irq::intc_by_domain(domain)?
        .try_lock()
        .map_err(|_| crate::irq::IrqError::Busy)?;
    let gic = intc
        .typed_mut::<T>()
        .ok_or(crate::irq::IrqError::Unsupported)?;
    Ok(f(gic))
}

pub fn with_primary_gic<T: Interface, R>(
    f: impl FnOnce(&mut T) -> R,
) -> Result<R, crate::irq::IrqError> {
    let mut intc = get_primary_gicd()?
        .try_lock()
        .map_err(|_| crate::irq::IrqError::Busy)?;
    let gic = intc
        .typed_mut::<T>()
        .ok_or(crate::irq::IrqError::Unsupported)?;
    Ok(f(gic))
}

pub fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), crate::irq::IrqError> {
    match backend() {
        GicBackend::V2 => v2::irq_set_enable(irq, enable),
        GicBackend::V3 => v3::irq_set_enable(irq, enable),
        GicBackend::None => Err(crate::irq::IrqError::Unsupported),
    }
}

pub fn irq_set_affinity(
    irq: IrqId,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), crate::irq::IrqError> {
    match backend() {
        GicBackend::V2 => v2::irq_set_affinity(irq, affinity),
        GicBackend::V3 => v3::irq_set_affinity(irq, affinity),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::irq_set_affinity(irq, affinity)
            } else {
                v2::irq_set_affinity(irq, affinity)
            }
        }
    }
}

pub fn setup_irq_by_fdt(cells: &[u32]) -> Result<rdif_intc::IrqTranslation, crate::irq::IrqError> {
    fdt_parse_irq_config(cells).map_err(|_| crate::irq::IrqError::InvalidIrq)?;
    let mut gic = rdrive::get_one::<Intc>()
        .ok_or(crate::irq::IrqError::Unsupported)?
        .lock()
        .map_err(|_| crate::irq::IrqError::Controller)?;
    let translation = gic.translate_fdt(cells)?;
    gic.configure(&translation)?;
    Ok(translation)
}

pub fn send_ipi(irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
    let raw = irq.into();
    match backend() {
        GicBackend::V2 => v2::send_ipi(raw, target),
        GicBackend::V3 => v3::send_ipi(raw, target),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::send_ipi(raw, target);
            } else {
                v2::send_ipi(raw, target);
            }
        }
    }
}

fn hardware_cpu_id(cpu_idx: usize) -> usize {
    someboot::smp::cpu_idx_to_id(cpu_idx).unwrap_or(cpu_idx)
}

pub enum ActiveIrq {
    V2(v2::ActiveIrq),
    V3(v3::ActiveIrq),
}

impl ActiveIrq {
    pub fn id(&self) -> rdrive::IrqId {
        match self {
            Self::V2(active) => active.id(),
            Self::V3(active) => active.id(),
        }
    }
}

pub fn begin_irq() -> Option<ActiveIrq> {
    match backend() {
        GicBackend::V2 => v2::begin_irq().map(ActiveIrq::V2),
        GicBackend::V3 => v3::begin_irq().map(ActiveIrq::V3),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::begin_irq().map(ActiveIrq::V3)
            } else {
                v2::begin_irq().map(ActiveIrq::V2)
            }
        }
    }
}
