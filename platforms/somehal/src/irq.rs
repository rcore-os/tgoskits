use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

#[cfg(not(test))]
use ax_kspin::SpinNoIrq as IrqRouteMutex;
#[cfg(test)]
use ax_kspin::SpinRaw as IrqRouteMutex;
use ax_kspin::{IrqGuard, SpinRaw as Mutex};
pub use rdif_intc;
use rdif_intc::Intc;
pub type ControllerIrqId = irq_framework::IrqId;
pub use irq_framework::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, CpuIpiTarget, HwIrq,
    IpiSendStatus, IrqDomainId, IrqError, IrqId, IrqSource,
};
use rdrive::{Device, DeviceId};

#[cfg(target_arch = "riscv64")]
pub use crate::arch::{RiscvPlicIrqEndpoint, RiscvPlicLeaseId};
use crate::{arch::Plat, common::PlatOp};

/// CPU-local interrupt domain for architecture trap causes such as timers/IPIs.
pub const CPU_LOCAL_IRQ_DOMAIN: IrqDomainId = IrqDomainId(u16::MAX);

/// x86 local APIC interrupt domain.
pub const X86_LAPIC_DOMAIN: IrqDomainId = IrqDomainId(1);

/// x86 I/O APIC interrupt domain.
pub const X86_IOAPIC_DOMAIN: IrqDomainId = IrqDomainId(2);

/// AArch64 GIC interrupt domain.
pub const AARCH64_GIC_DOMAIN: IrqDomainId = IrqDomainId(3);

/// RISC-V PLIC interrupt domain.
pub const RISCV_PLIC_DOMAIN: IrqDomainId = IrqDomainId(4);

/// LoongArch EIOINTC interrupt domain.
pub const LOONGARCH_EIOINTC_DOMAIN: IrqDomainId = IrqDomainId(5);

/// LoongArch PCH-PIC interrupt domain.
pub const LOONGARCH_PCH_PIC_DOMAIN: IrqDomainId = IrqDomainId(6);

const DYNAMIC_IRQ_DOMAIN_BASE: u16 = 7;
const INVALID_IRQ_DOMAIN: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqDomainKind {
    X86IoApic,
    AArch64Gic,
    RiscvPlic,
    LoongArchEioIntc,
    LoongArchPchPic,
    LoongArchLioIntc,
    MsiParent,
    PciMsix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqDomain {
    pub id: IrqDomainId,
    pub owner: DeviceId,
    pub parent: Option<IrqDomainId>,
    pub kind: IrqDomainKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IrqRoute {
    parent: IrqId,
    leaf: IrqId,
}

static IRQ_DOMAINS: Mutex<Vec<IrqDomain>> = Mutex::new(Vec::new());
static IRQ_ROUTES: IrqRouteMutex<Vec<IrqRoute>> = IrqRouteMutex::new(Vec::new());

#[cfg(not(test))]
const _: fn(&IrqRouteMutex<Vec<IrqRoute>>) = |lock| {
    let _: &ax_kspin::SpinNoIrq<Vec<IrqRoute>> = lock;
};
static X86_IOAPIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static AARCH64_GIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static RISCV_PLIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static LOONGARCH_EIOINTC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static LOONGARCH_PCH_PIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
static LOONGARCH_LIOINTC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);

pub fn alloc_irq_domain(owner: DeviceId, kind: IrqDomainKind) -> Result<IrqDomainId, IrqError> {
    register_irq_domain(owner, None, kind)
}

pub fn register_irq_domain(
    owner: DeviceId,
    preferred: Option<IrqDomainId>,
    kind: IrqDomainKind,
) -> Result<IrqDomainId, IrqError> {
    register_domain(owner, preferred, None, kind)
}

pub fn alloc_child_irq_domain(
    owner: DeviceId,
    parent: IrqDomainId,
    kind: IrqDomainKind,
) -> Result<IrqDomainId, IrqError> {
    register_child_irq_domain(owner, None, parent, kind)
}

pub fn register_child_irq_domain(
    owner: DeviceId,
    preferred: Option<IrqDomainId>,
    parent: IrqDomainId,
    kind: IrqDomainKind,
) -> Result<IrqDomainId, IrqError> {
    register_domain(owner, preferred, Some(parent), kind)
}

fn register_domain(
    owner: DeviceId,
    preferred: Option<IrqDomainId>,
    parent: Option<IrqDomainId>,
    kind: IrqDomainKind,
) -> Result<IrqDomainId, IrqError> {
    let mut domains = IRQ_DOMAINS.lock();

    match parent {
        Some(parent) => {
            if domain_slot(kind).is_some() {
                return Err(IrqError::Unsupported);
            }
            if !domains.iter().any(|domain| domain.id == parent) {
                return Err(IrqError::InvalidIrq);
            }
            if let Some(domain) = domains.iter().find(|domain| {
                domain.owner == owner && domain.parent == Some(parent) && domain.kind == kind
            }) {
                return match preferred {
                    Some(preferred) if preferred != domain.id => Err(IrqError::Busy),
                    _ => Ok(domain.id),
                };
            }
        }
        None => {
            if domain_slot(kind).is_none() {
                return Err(IrqError::Unsupported);
            }
            if let Some(domain) = domains
                .iter()
                .find(|domain| domain.owner == owner && domain.parent.is_none())
            {
                return if domain.kind == kind {
                    Ok(domain.id)
                } else {
                    Err(IrqError::Busy)
                };
            }

            if domains
                .iter()
                .any(|domain| domain.parent.is_none() && domain.kind == kind)
            {
                return Err(IrqError::Unsupported);
            }
        }
    }

    let id = match preferred {
        Some(id) => {
            if is_reserved_domain(id) {
                return Err(IrqError::InvalidIrq);
            }
            if domains.iter().any(|domain| domain.id == id) {
                return Err(IrqError::Busy);
            }
            id
        }
        None => next_dynamic_domain(&domains)?,
    };

    domains.push(IrqDomain {
        id,
        owner,
        parent,
        kind,
    });
    if let Some(slot) = domain_slot(kind) {
        slot.store(id.0, Ordering::Release);
    }
    Ok(id)
}

fn domain_slot(kind: IrqDomainKind) -> Option<&'static AtomicU16> {
    match kind {
        IrqDomainKind::X86IoApic => Some(&X86_IOAPIC_DOMAIN_SLOT),
        IrqDomainKind::AArch64Gic => Some(&AARCH64_GIC_DOMAIN_SLOT),
        IrqDomainKind::RiscvPlic => Some(&RISCV_PLIC_DOMAIN_SLOT),
        IrqDomainKind::LoongArchEioIntc => Some(&LOONGARCH_EIOINTC_DOMAIN_SLOT),
        IrqDomainKind::LoongArchPchPic => Some(&LOONGARCH_PCH_PIC_DOMAIN_SLOT),
        IrqDomainKind::LoongArchLioIntc => Some(&LOONGARCH_LIOINTC_DOMAIN_SLOT),
        IrqDomainKind::MsiParent | IrqDomainKind::PciMsix => None,
    }
}

fn is_reserved_domain(id: IrqDomainId) -> bool {
    id.0 < DYNAMIC_IRQ_DOMAIN_BASE || id.0 == u16::MAX
}

fn next_dynamic_domain(domains: &[IrqDomain]) -> Result<IrqDomainId, IrqError> {
    for id in DYNAMIC_IRQ_DOMAIN_BASE..u16::MAX {
        let id = IrqDomainId(id);
        if domains.iter().all(|domain| domain.id != id) {
            return Ok(id);
        }
    }
    Err(IrqError::NoMemory)
}

pub fn domain_by_id(id: IrqDomainId) -> Option<IrqDomain> {
    IRQ_DOMAINS
        .lock()
        .iter()
        .find(|domain| domain.id == id)
        .copied()
}

pub fn domain_by_owner(owner: DeviceId) -> Option<IrqDomain> {
    let domains = IRQ_DOMAINS.lock();
    domains
        .iter()
        .find(|domain| domain.owner == owner && domain.parent.is_none())
        .or_else(|| domains.iter().find(|domain| domain.owner == owner))
        .copied()
}

pub fn domain_by_kind(kind: IrqDomainKind) -> Option<IrqDomain> {
    domain_by_kind_fast(kind).and_then(domain_by_id)
}

pub fn domain_by_kind_fast(kind: IrqDomainKind) -> Option<IrqDomainId> {
    let slot = domain_slot(kind)?;
    match slot.load(Ordering::Acquire) {
        INVALID_IRQ_DOMAIN => None,
        id => Some(IrqDomainId(id)),
    }
}

pub fn domain_is_kind(id: IrqDomainId, kind: IrqDomainKind) -> bool {
    if domain_slot(kind).is_some() {
        return domain_by_kind_fast(kind) == Some(id);
    }
    domain_by_id(id).is_some_and(|domain| domain.kind == kind)
}

pub fn intc_by_domain(domain: IrqDomainId) -> Result<Device<Intc>, IrqError> {
    let domain = domain_by_id(domain).ok_or(IrqError::Unsupported)?;
    rdrive::get::<Intc>(domain.owner).map_err(|_| IrqError::Unsupported)
}

pub fn set_controller_irq_enabled(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    let intc = intc_by_domain(irq.domain)?;
    let mut intc = intc.try_lock().map_err(|_| IrqError::Busy)?;
    intc.set_enabled(irq.hwirq, enabled)
}

#[must_use = "dropping ActiveIrq completes the interrupt in the interrupt controller"]
pub struct ActiveIrq {
    inner: <Plat as PlatOp>::ActiveIrq,
}

impl ActiveIrq {
    /// Returns the controller IRQ before any parent-to-leaf route is applied.
    pub fn controller_id(&self) -> IrqId {
        Plat::active_irq_id(&self.inner)
    }

    pub fn id(&self) -> IrqId {
        resolve_irq_route(self.controller_id())
    }
}

pub fn map_irq_route(parent: IrqId, leaf: IrqId) -> Result<(), IrqError> {
    if parent == leaf {
        return Err(IrqError::InvalidIrq);
    }

    let domains = IRQ_DOMAINS.lock();
    if !domains.iter().any(|domain| domain.id == parent.domain)
        || !domain_has_strict_ancestor(&domains, leaf.domain, parent.domain)
    {
        return Err(IrqError::InvalidIrq);
    }
    drop(domains);

    let mut routes = IRQ_ROUTES.lock();
    if let Some(route) = routes.iter().find(|route| route.parent == parent) {
        return if route.leaf == leaf {
            Ok(())
        } else {
            Err(IrqError::Busy)
        };
    }
    if routes.iter().any(|route| route.leaf == leaf) {
        return Err(IrqError::Busy);
    }
    routes.push(IrqRoute { parent, leaf });
    Ok(())
}

fn domain_has_strict_ancestor(
    domains: &[IrqDomain],
    child: IrqDomainId,
    ancestor: IrqDomainId,
) -> bool {
    let mut next = domains
        .iter()
        .find(|domain| domain.id == child)
        .and_then(|domain| domain.parent);
    for _ in 0..domains.len() {
        let Some(id) = next else {
            return false;
        };
        if id == ancestor {
            return true;
        }
        next = domains
            .iter()
            .find(|domain| domain.id == id)
            .and_then(|domain| domain.parent);
    }
    false
}

pub fn unmap_irq_route(parent: IrqId, leaf: IrqId) -> Result<(), IrqError> {
    let mut routes = IRQ_ROUTES.lock();
    let Some(index) = routes
        .iter()
        .position(|route| route.parent == parent && route.leaf == leaf)
    else {
        return Err(IrqError::InvalidIrq);
    };
    routes.swap_remove(index);
    Ok(())
}

/// Resolves a controller IRQ claimed in hard-IRQ context to its leaf IRQ.
///
/// Route mutation is a control-plane operation performed while the source is
/// masked or before it is enabled. The interrupt path only reads the stable
/// mapping and never performs rdrive lookup, allocation, or free.
pub fn resolve_irq_route(parent: IrqId) -> IrqId {
    IRQ_ROUTES
        .lock()
        .iter()
        .find(|route| route.parent == parent)
        .map_or(parent, |route| route.leaf)
}

pub fn parent_irq_for_leaf(leaf: IrqId) -> Option<IrqId> {
    IRQ_ROUTES
        .lock()
        .iter()
        .find(|route| route.leaf == leaf)
        .map(|route| route.parent)
}

/// Hardware routing preference for a global IRQ line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqAffinity {
    /// Leave routing unchanged or platform-selected.
    Any,
    /// Route to one logical CPU.
    Fixed { cpu_id: usize },
}

fn setup_irq_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    let mut intc = rdrive::get::<Intc>(irq_parent)
        .map_err(|_| IrqError::Unsupported)?
        .lock()
        .map_err(|_| IrqError::Controller)?;
    debug!("Setting up IRQ {:?}", irq_cell);
    let translation = intc.translate_fdt(irq_cell)?;
    intc.configure(&translation)?;
    Ok(translation.id)
}

#[cfg(target_arch = "aarch64")]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

#[cfg(target_arch = "riscv64")]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

#[cfg(target_arch = "loongarch64")]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> Result<IrqId, IrqError> {
    setup_irq_by_fdt(irq_parent, irq_cell)
}

pub fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
    debug!("Setting IRQ {:?} enable to {}", irq, enable);
    Plat::irq_set_enable(parent_irq_for_leaf(irq).unwrap_or(irq), enable)
}

pub fn irq_set_affinity(irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
    Plat::irq_set_affinity(parent_irq_for_leaf(irq).unwrap_or(irq), affinity)
}

/// Resolves and permanently leases one RISC-V PLIC source for a fixed IRQ-side
/// endpoint. This is a control-plane operation and must run before the source
/// can enter hard-IRQ forwarding.
#[cfg(target_arch = "riscv64")]
pub fn lease_riscv_plic_irq_endpoint(
    irq: IrqId,
    affinity: IrqAffinity,
) -> Result<RiscvPlicIrqEndpoint, IrqError> {
    crate::arch::lease_riscv_plic_irq_endpoint(parent_irq_for_leaf(irq).unwrap_or(irq), affinity)
}

/// Atomically leases a validated batch of RISC-V PLIC sources.
///
/// No source changes affinity, enablement, or ownership unless the complete
/// batch can commit under the controller lock.
#[cfg(target_arch = "riscv64")]
pub fn lease_riscv_plic_irq_endpoints(
    irqs: &[IrqId],
    affinity: IrqAffinity,
) -> Result<Vec<RiscvPlicIrqEndpoint>, IrqError> {
    let mut parents = Vec::with_capacity(irqs.len());
    for &irq in irqs {
        parents.push(parent_irq_for_leaf(irq).unwrap_or(irq));
    }
    crate::arch::lease_riscv_plic_irq_endpoints(&parents, affinity)
}

/// Atomically releases a complete generation-checked RISC-V PLIC lease batch.
#[cfg(target_arch = "riscv64")]
pub fn release_riscv_plic_irq_endpoints(leases: &[RiscvPlicLeaseId]) -> Result<(), IrqError> {
    crate::arch::release_riscv_plic_irq_endpoints(leases)
}

/// Sends an IPI while local IRQ nesting excludes every nested sender.
///
/// The lowest public sender requires the guard because xAPIC commits one IPI
/// through a shared high/low register pair. Keeping the guard across current
/// CPU identification, target validation, and the architecture transaction
/// makes that pair structurally non-reentrant.
pub fn send_ipi(irq: IrqId, target: CpuIpiTarget, irq_guard: &IrqGuard) -> IpiSendStatus {
    // Materialize the pin before observing the logical CPU. Its lifetime is
    // tied to `irq_guard`, which remains borrowed through the transaction.
    let _cpu_pin = irq_guard.cpu_pin();
    let Some(current_cpu) = crate::cpu::runtime_current_cpu() else {
        return IpiSendStatus::Invalid;
    };
    Plat::send_ipi(irq, target, current_cpu)
}

pub fn ipi_irq() -> IrqId {
    Plat::ipi_irq()
}

pub fn systick_irq() -> IrqId {
    Plat::systick_irq()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuBootRole {
    Primary,
    Secondary,
}

pub fn init_boot_irqs(cpu_id: usize) -> Result<(), IrqError> {
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip boot IRQ probe");
        return Plat::init_boot_irq_cpu(cpu_id, CpuBootRole::Primary);
    }

    finish_boot_irq_probe_stage(
        BootIrqProbeStage::Required("CLK/INTC/TIMER"),
        rdrive::probe_pre_kernel_until(rdrive::register::ProbePriority::TIMER, true),
    )?;
    finish_boot_irq_probe_stage(
        BootIrqProbeStage::Optional("MSI"),
        rdrive::probe_pre_kernel_until(rdrive::register::ProbePriority::MSI, false),
    )?;
    Plat::init_boot_irq_cpu(cpu_id, CpuBootRole::Primary)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootIrqProbeStage {
    Required(&'static str),
    Optional(&'static str),
}

fn finish_boot_irq_probe_stage(
    stage: BootIrqProbeStage,
    result: Result<(), rdrive::ProbeError>,
) -> Result<(), IrqError> {
    let Err(err) = result else {
        return Ok(());
    };

    match stage {
        BootIrqProbeStage::Required(name) => {
            warn!("failed to run required boot IRQ {name} probes: {err:?}");
            Err(IrqError::Controller)
        }
        BootIrqProbeStage::Optional(name) => {
            warn!("optional boot IRQ {name} probes failed; continuing without them: {err:?}");
            Ok(())
        }
    }
}

pub fn init_secondary_boot_irqs(cpu_id: usize) -> Result<(), IrqError> {
    Plat::init_secondary_boot_irqs(cpu_id)
}

#[cfg(target_arch = "aarch64")]
pub fn aarch64_gic_irq_id(hwirq: HwIrq) -> IrqId {
    crate::arch::gic_irq_id(hwirq)
}

#[cfg(target_arch = "aarch64")]
pub fn aarch64_gic_irq_id_checked(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    crate::arch::gic_irq_id_checked(hwirq)
}

pub fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    Plat::begin_irq(raw).map(|inner| ActiveIrq { inner })
}

pub fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
    Plat::resolve_irq_source(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_domains() {
        IRQ_DOMAINS.lock().clear();
        IRQ_ROUTES.lock().clear();
        for kind in [
            IrqDomainKind::X86IoApic,
            IrqDomainKind::AArch64Gic,
            IrqDomainKind::RiscvPlic,
            IrqDomainKind::LoongArchEioIntc,
            IrqDomainKind::LoongArchPchPic,
            IrqDomainKind::LoongArchLioIntc,
        ] {
            domain_slot(kind)
                .unwrap()
                .store(INVALID_IRQ_DOMAIN, Ordering::Release);
        }
    }

    #[test]
    fn alloc_irq_domain_starts_after_compat_domains_and_is_idempotent() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let owner = DeviceId::new();
        let domain = alloc_irq_domain(owner, IrqDomainKind::RiscvPlic).unwrap();

        assert_eq!(domain, IrqDomainId(7));
        assert_eq!(
            alloc_irq_domain(owner, IrqDomainKind::RiscvPlic),
            Ok(domain)
        );
        assert_eq!(domain_by_owner(owner).unwrap().id, domain);
        assert_eq!(domain_by_kind(IrqDomainKind::RiscvPlic).unwrap().id, domain);
    }

    #[test]
    fn register_irq_domain_rejects_owner_and_id_conflicts() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let owner_a = DeviceId::new();
        let owner_b = DeviceId::new();
        let preferred = IrqDomainId(42);

        assert_eq!(
            register_irq_domain(owner_a, Some(preferred), IrqDomainKind::AArch64Gic),
            Ok(preferred)
        );
        assert_eq!(
            register_irq_domain(owner_a, Some(preferred), IrqDomainKind::RiscvPlic),
            Err(IrqError::Busy)
        );
        assert_eq!(
            register_irq_domain(owner_b, Some(preferred), IrqDomainKind::RiscvPlic),
            Err(IrqError::Busy)
        );
    }

    #[test]
    fn register_irq_domain_rejects_second_controller_of_same_kind() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let owner_a = DeviceId::new();
        let owner_b = DeviceId::new();

        alloc_irq_domain(owner_a, IrqDomainKind::AArch64Gic).unwrap();
        assert_eq!(
            alloc_irq_domain(owner_b, IrqDomainKind::AArch64Gic),
            Err(IrqError::Unsupported)
        );
    }

    #[test]
    fn register_irq_domain_rejects_reserved_preferred_ids() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        for id in [
            IrqDomainId(0),
            X86_LAPIC_DOMAIN,
            CPU_LOCAL_IRQ_DOMAIN,
            X86_IOAPIC_DOMAIN,
            AARCH64_GIC_DOMAIN,
            RISCV_PLIC_DOMAIN,
            LOONGARCH_EIOINTC_DOMAIN,
            LOONGARCH_PCH_PIC_DOMAIN,
            IrqDomainId(u16::MAX),
        ] {
            assert_eq!(
                register_irq_domain(DeviceId::new(), Some(id), IrqDomainKind::X86IoApic),
                Err(IrqError::InvalidIrq)
            );
        }
    }

    #[test]
    fn child_domains_allow_multiple_instances_of_same_kind() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let root_owner = DeviceId::new();
        let parent = alloc_irq_domain(root_owner, IrqDomainKind::AArch64Gic).unwrap();
        let child_a =
            alloc_child_irq_domain(DeviceId::new(), parent, IrqDomainKind::PciMsix).unwrap();
        let child_b =
            alloc_child_irq_domain(DeviceId::new(), parent, IrqDomainKind::PciMsix).unwrap();

        assert_ne!(child_a, child_b);
        assert_eq!(domain_by_id(child_a).unwrap().parent, Some(parent));
        assert_eq!(domain_by_id(child_b).unwrap().parent, Some(parent));
        assert_eq!(domain_by_id(parent).unwrap().parent, None);
    }

    #[test]
    fn child_domain_allocation_is_idempotent_for_owner_parent_and_kind() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let root_owner = DeviceId::new();
        let child_owner = DeviceId::new();
        let parent = alloc_irq_domain(root_owner, IrqDomainKind::AArch64Gic).unwrap();
        let child = alloc_child_irq_domain(child_owner, parent, IrqDomainKind::PciMsix).unwrap();

        assert_eq!(
            alloc_child_irq_domain(child_owner, parent, IrqDomainKind::PciMsix),
            Ok(child)
        );
        assert_eq!(
            register_child_irq_domain(
                child_owner,
                Some(IrqDomainId(42)),
                parent,
                IrqDomainKind::PciMsix
            ),
            Err(IrqError::Busy)
        );
        assert_eq!(domain_by_kind(IrqDomainKind::PciMsix), None);
    }

    #[test]
    fn irq_routes_resolve_parent_lpi_to_leaf_irq_and_can_be_removed() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let parent_domain = alloc_irq_domain(DeviceId::new(), IrqDomainKind::AArch64Gic).unwrap();
        let leaf_domain =
            alloc_child_irq_domain(DeviceId::new(), parent_domain, IrqDomainKind::PciMsix).unwrap();
        let parent_irq = IrqId::new(parent_domain, HwIrq(8192));
        let leaf_irq = IrqId::new(leaf_domain, HwIrq(0));

        assert_eq!(resolve_irq_route(parent_irq), parent_irq);
        map_irq_route(parent_irq, leaf_irq).unwrap();
        assert_eq!(resolve_irq_route(parent_irq), leaf_irq);
        assert_eq!(parent_irq_for_leaf(leaf_irq), Some(parent_irq));
        assert_eq!(map_irq_route(parent_irq, leaf_irq), Ok(()));
        assert_eq!(
            map_irq_route(parent_irq, IrqId::new(leaf_domain, HwIrq(1))),
            Err(IrqError::Busy)
        );

        assert_eq!(unmap_irq_route(parent_irq, leaf_irq), Ok(()));
        assert_eq!(resolve_irq_route(parent_irq), parent_irq);
        assert_eq!(parent_irq_for_leaf(leaf_irq), None);
        assert_eq!(
            unmap_irq_route(parent_irq, leaf_irq),
            Err(IrqError::InvalidIrq)
        );
    }

    #[test]
    fn irq_route_rejects_leaf_outside_parent_domain_chain() {
        let _guard = TEST_LOCK.lock();
        reset_domains();

        let gic_domain = alloc_irq_domain(DeviceId::new(), IrqDomainKind::AArch64Gic).unwrap();
        let plic_domain = register_irq_domain(
            DeviceId::new(),
            Some(IrqDomainId(42)),
            IrqDomainKind::RiscvPlic,
        )
        .unwrap();
        let leaf_domain =
            alloc_child_irq_domain(DeviceId::new(), plic_domain, IrqDomainKind::PciMsix).unwrap();

        assert_eq!(
            map_irq_route(
                IrqId::new(gic_domain, HwIrq(8192)),
                IrqId::new(leaf_domain, HwIrq(0))
            ),
            Err(IrqError::InvalidIrq)
        );
    }

    #[test]
    fn boot_irq_optional_msi_probe_failure_is_nonfatal() {
        let result = finish_boot_irq_probe_stage(
            BootIrqProbeStage::Optional("MSI"),
            Err(rdrive::ProbeError::OnProbe(
                rdrive::probe::OnProbeError::Unsupported("missing MSI controller"),
            )),
        );

        assert_eq!(result, Ok(()));
    }
}
