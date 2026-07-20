use alloc::boxed::Box;

use arm_gic_driver::{IntId, VirtAddr, checked_intid, fdt_parse_irq_config};
use irq_framework::{CpuId, IrqDomainId, IrqId, IrqScope};
use kernutil::StaticCell;
use rdif_intc::{Intc, Interface};
use rdrive::Device;

mod its;
mod v2;
mod v3;

use core::sync::atomic::{AtomicU8, Ordering};

use crate::irq_line::{BoundIrqStatus, IrqChipLine, PreparedIrqChipLine};

#[derive(Clone, Copy, Eq, PartialEq)]
enum GicBackend {
    None = 0,
    V2   = 2,
    V3   = 3,
}

static GIC_BACKEND: AtomicU8 = AtomicU8::new(GicBackend::None as u8);
static GIC_LINE_BACKEND: StaticCell<GicLineBackend> = StaticCell::uninit();

#[derive(Clone, Copy)]
enum GicLineBackend {
    V2 {
        gicd: VirtAddr,
        gicc: VirtAddr,
        max_intid: u32,
    },
    V3 {
        gicd: VirtAddr,
        gicr: VirtAddr,
        max_intid: u32,
    },
}

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

pub(super) fn publish_v2_line_backend(gic: &arm_gic_driver::v2::Gic) {
    GIC_LINE_BACKEND.init(GicLineBackend::V2 {
        gicd: gic.gicd_addr(),
        gicc: gic.gicc_addr(),
        max_intid: gic.max_intid(),
    });
}

pub(super) fn publish_v3_line_backend(gic: &arm_gic_driver::v3::Gic) {
    GIC_LINE_BACKEND.init(GicLineBackend::V3 {
        gicd: gic.gicd_addr(),
        gicr: gic.gicr_addr(),
        max_intid: gic.max_intid(),
    });
}

pub(super) fn prepare_irq_line(
    irq: IrqId,
    scope: IrqScope,
    affinity: crate::irq::IrqAffinity,
) -> Result<PreparedIrqChipLine, crate::irq::IrqError> {
    let line_backend = *GIC_LINE_BACKEND
        .get_initialized()
        .ok_or(crate::irq::IrqError::Unsupported)?;
    if irq.hwirq.0 >= its::LPI_INTID_BASE {
        // An LPI enable transition is not just a distributor bit update: it
        // also requires property-table cache maintenance and invalidation on
        // the exact redistributor collection. Until ITS registration can
        // publish that complete fixed-bound capability, fail during the
        // fallible preparation phase instead of pretending the live path is
        // infallible.
        return Err(crate::irq::IrqError::Unsupported);
    }
    let max_intid = match line_backend {
        GicLineBackend::V2 { max_intid, .. } | GicLineBackend::V3 { max_intid, .. } => max_intid,
    };
    let intid =
        checked_intid(irq.hwirq.0, max_intid).map_err(|_| crate::irq::IrqError::InvalidIrq)?;
    let kind = if intid.is_private() {
        let IrqScope::PerCpu { cpus } = scope else {
            return Err(crate::irq::IrqError::InvalidIrq);
        };
        if cpus.is_empty() {
            return Err(crate::irq::IrqError::InvalidCpu);
        }
        let all_targets_ready = match line_backend {
            GicLineBackend::V2 { .. } => cpus.iter().all(v2::private_line_cpu_ready),
            GicLineBackend::V3 { .. } => cpus.iter().all(v3::private_line_cpu_ready),
        };
        if !all_targets_ready {
            return Err(crate::irq::IrqError::InvalidCpu);
        }
        match line_backend {
            GicLineBackend::V2 { .. } => GicLineKind::V2Private,
            GicLineBackend::V3 { .. } => GicLineKind::V3Private,
        }
    } else {
        if scope != IrqScope::Global {
            return Err(crate::irq::IrqError::InvalidIrq);
        }
        // Global lines are physically masked before the endpoint is published.
        set_shared_line_enabled(line_backend, intid, false);
        irq_set_affinity(irq, affinity)?;
        GicLineKind::Shared(line_backend)
    };
    Ok(PreparedIrqChipLine::maskable(Box::new(GicIrqChipLine {
        irq,
        intid,
        kind,
    })))
}

fn set_shared_line_enabled(backend: GicLineBackend, intid: IntId, enabled: bool) {
    debug_assert!(!intid.is_private());
    match backend {
        GicLineBackend::V2 { gicd, gicc, .. } => {
            let gic = unsafe { arm_gic_driver::v2::Gic::new(gicd, gicc, None) };
            gic.set_irq_enable(intid, enabled);
        }
        GicLineBackend::V3 { gicd, gicr, .. } => {
            let mut gic = unsafe { arm_gic_driver::v3::Gic::new(gicd, gicr) };
            gic.set_irq_enable(intid, enabled);
        }
    }
}

fn shared_line_status(backend: GicLineBackend, intid: IntId) -> BoundIrqStatus {
    debug_assert!(!intid.is_private());
    match backend {
        GicLineBackend::V2 { gicd, gicc, .. } => {
            let gic = unsafe { arm_gic_driver::v2::Gic::new(gicd, gicc, None) };
            BoundIrqStatus {
                enabled: Some(gic.is_irq_enable(intid)),
                pending: Some(gic.is_pending(intid)),
                in_service: Some(gic.is_active(intid)),
            }
        }
        GicLineBackend::V3 { gicd, gicr, .. } => {
            let gic = unsafe { arm_gic_driver::v3::Gic::new(gicd, gicr) };
            BoundIrqStatus {
                enabled: Some(gic.is_irq_enable(intid)),
                pending: Some(gic.is_pending(intid)),
                in_service: Some(gic.is_active(intid)),
            }
        }
    }
}

struct GicIrqChipLine {
    irq: IrqId,
    intid: IntId,
    kind: GicLineKind,
}

#[derive(Clone, Copy)]
enum GicLineKind {
    V2Private,
    V3Private,
    Shared(GicLineBackend),
}

// SAFETY: private endpoints are accepted only after every target CPU has
// published its banked CPU-interface slot, and the framework invokes them on
// that target CPU. Shared endpoints retain immutable distributor addresses.
// All live accesses are bounded MMIO operations with no allocation or driver
// registry lookup.
unsafe impl IrqChipLine for GicIrqChipLine {
    fn set_enabled(&self, cpu: Option<CpuId>, enabled: bool) {
        match self.kind {
            GicLineKind::V2Private => {
                let cpu = cpu.expect("prepared GICv2 private line requires a target CPU");
                v2::set_private_line_enabled(cpu, self.intid, enabled);
            }
            GicLineKind::V3Private => {
                let cpu = cpu.expect("prepared GICv3 private line requires a target CPU");
                v3::set_private_line_enabled(cpu, self.intid, enabled);
            }
            GicLineKind::Shared(backend) => {
                assert!(
                    cpu.is_none(),
                    "prepared GIC shared line {:?} cannot use a per-CPU target",
                    self.irq
                );
                set_shared_line_enabled(backend, self.intid, enabled);
            }
        }
    }

    fn status(&self, cpu: Option<CpuId>) -> BoundIrqStatus {
        match self.kind {
            GicLineKind::V2Private => v2::private_line_status(
                cpu.expect("prepared GICv2 private line requires a target CPU"),
                self.intid,
            ),
            GicLineKind::V3Private => v3::private_line_status(
                cpu.expect("prepared GICv3 private line requires a target CPU"),
                self.intid,
            ),
            GicLineKind::Shared(backend) => {
                assert!(cpu.is_none(), "GIC shared line cannot use a per-CPU target");
                shared_line_status(backend, self.intid)
            }
        }
    }
}

pub fn init_current_cpu() -> Result<(), crate::irq::IrqError> {
    let cpu_idx = crate::cpu::current_cpu_idx().ok_or(crate::irq::IrqError::InvalidCpu)?;
    init_cpu(cpu_idx)
}

pub fn init_cpu(cpu_idx: usize) -> Result<(), crate::irq::IrqError> {
    match backend() {
        GicBackend::V2 => v2::init_cpu(cpu_idx).map_err(|_| crate::irq::IrqError::Controller),
        GicBackend::V3 => v3::init_cpu(cpu_idx).map_err(|_| crate::irq::IrqError::Controller),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::init_cpu(cpu_idx).map_err(|_| crate::irq::IrqError::Controller)
            } else {
                v2::init_cpu(cpu_idx).map_err(|_| crate::irq::IrqError::Controller)
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

pub(crate) fn send_ipi(
    irq: rdrive::IrqId,
    target: crate::irq::CpuIpiTarget,
    current_cpu: irq_framework::CpuId,
) -> crate::irq::IpiSendStatus {
    let raw = irq.into();
    match backend() {
        GicBackend::V2 => v2::send_ipi(raw, target, current_cpu),
        GicBackend::V3 => v3::send_ipi(raw, target, current_cpu),
        GicBackend::None => {
            if v3::is_support_icc() {
                v3::send_ipi(raw, target, current_cpu)
            } else {
                v2::send_ipi(raw, target, current_cpu)
            }
        }
    }
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
