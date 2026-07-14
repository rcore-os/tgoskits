#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use alloc::vec::Vec;
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use core::sync::atomic::{AtomicPtr, AtomicUsize};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use ax_kspin::SpinNoPreempt;
#[cfg(test)]
use ax_plat::irq::IrqOutcome;
use ax_plat::irq::{
    CpuId, IrqAffinity, IrqError, IrqId, IrqIf, IrqSource, TrapVector, dispatch_irq_on,
};

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
mod loongarch64_hv;

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
// Hard IRQs use only immutable endpoint capabilities and never acquire this
// short control-plane lock. A complete CPU/source key and generation reserve
// ownership while allocation, controller leasing, and MMIO run lock-free.
static VIRTUAL_IRQ_ROUTE_CONTROL: SpinNoPreempt<VirtualIrqRouteState> =
    SpinNoPreempt::new(VirtualIrqRouteState::new());
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_ENDPOINTS: [spin::Once<RiscvVirtualIrqEndpoint>; RISCV_PLIC_SOURCE_COUNT] =
    [const { spin::Once::new() }; RISCV_PLIC_SOURCE_COUNT];
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static FORWARDED_IRQ_STATE: [ForwardedIrqState; RISCV_PLIC_SOURCE_COUNT] =
    [const { ForwardedIrqState::new() }; RISCV_PLIC_SOURCE_COUNT];
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static FORWARDED_IRQ_FAULTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_SINK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const RISCV_PLIC_SOURCE_COUNT: usize = 1024;

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
struct RiscvVirtualIrqEndpoint {
    controller_irq: IrqId,
    endpoint: somehal::irq::RiscvPlicIrqEndpoint,
    target_cpu: usize,
    activated: AtomicBool,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
const VIRTUAL_IRQ_SOURCE_WORDS: usize = RISCV_PLIC_SOURCE_COUNT.div_ceil(u64::BITS as usize);

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VirtualIrqRouteKey {
    target_cpu: usize,
    irq_sources: [u64; VIRTUAL_IRQ_SOURCE_WORDS],
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VirtualIrqRoutePhase {
    Vacant,
    Reserved {
        key: VirtualIrqRouteKey,
        generation: u64,
    },
    Published {
        key: VirtualIrqRouteKey,
        generation: u64,
    },
    Activating {
        key: VirtualIrqRouteKey,
        generation: u64,
    },
    Active {
        key: VirtualIrqRouteKey,
        generation: u64,
    },
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
struct VirtualIrqRouteState {
    phase: VirtualIrqRoutePhase,
    next_generation: u64,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl VirtualIrqRouteState {
    const fn new() -> Self {
        Self {
            phase: VirtualIrqRoutePhase::Vacant,
            next_generation: 0,
        }
    }
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const FORWARDED_STATE_MASK: u64 = 0b11;
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const FORWARDED_UNMASKED: u64 = 0;
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const FORWARDED_MASKED: u64 = 1;
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const FORWARDED_UNMASKING: u64 = 2;

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const FORWARDED_GENERATION_MAX: u64 = u64::MAX >> 2;

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct ForwardedGeneration(u64);

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
impl ForwardedGeneration {
    const fn new(raw: u64) -> Option<Self> {
        if raw == 0 || raw > FORWARDED_GENERATION_MAX {
            None
        } else {
            Some(Self(raw))
        }
    }

    const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
struct ForwardedIrqState(AtomicU64);

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
impl ForwardedIrqState {
    const fn new() -> Self {
        Self(AtomicU64::new(FORWARDED_UNMASKED))
    }

    fn begin_mask(&self) -> Option<ForwardedGeneration> {
        let observed = self.0.load(Ordering::Acquire);
        if observed & FORWARDED_STATE_MASK != FORWARDED_UNMASKED {
            return None;
        }
        let generation = next_forwarded_generation(observed >> 2);
        let masked = (generation.get() << 2) | FORWARDED_MASKED;
        self.0
            .compare_exchange(observed, masked, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| generation)
    }

    fn begin_unmask(&self, generation: ForwardedGeneration) -> Option<ForwardedUnmaskPermit> {
        let masked = (generation.get() << 2) | FORWARDED_MASKED;
        let unmasking = (generation.get() << 2) | FORWARDED_UNMASKING;
        self.0
            .compare_exchange(masked, unmasking, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ForwardedUnmaskPermit { generation })
    }

    fn finish_unmask(&self, permit: ForwardedUnmaskPermit) {
        self.0.store(
            permit.generation.get() << 2 | FORWARDED_UNMASKED,
            Ordering::Release,
        );
    }
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
struct ForwardedUnmaskPermit {
    generation: ForwardedGeneration,
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const fn next_forwarded_generation(generation: u64) -> ForwardedGeneration {
    let next = generation.wrapping_add(1) & FORWARDED_GENERATION_MAX;
    ForwardedGeneration(if next == 0 { 1 } else { next })
}

/// Opaque token for one physical PLIC source that was claimed, masked, and
/// completed before software delivery to a guest.
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RiscvForwardedIrq {
    source: u32,
    generation: u64,
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
impl RiscvForwardedIrq {
    /// Reconstructs a platform claim after validating its packed generation.
    pub const fn try_new(source: u32, generation: u64) -> Option<Self> {
        if source == 0 || ForwardedGeneration::new(generation).is_none() {
            None
        } else {
            Some(Self { source, generation })
        }
    }

    const fn from_generation(source: u32, generation: ForwardedGeneration) -> Self {
        Self {
            source,
            generation: generation.get(),
        }
    }

    pub const fn source(self) -> u32 {
        self.source
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Hard-IRQ-safe monitor sink for one physical PLIC ownership transfer.
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub type RiscvVirtualIrqSink = unsafe extern "C" fn(u32, u64) -> bool;

/// Installs the monitor-wide hard-IRQ sink for configured guest PLIC sources.
///
/// # Safety
///
/// The sink and everything it references must remain valid until shutdown.
/// It must not allocate, free, block, acquire any lock, invoke guest code, or
/// unwind. It may only publish into preallocated lock-free state and wake the
/// fixed owner through a hard-IRQ-safe wake handle.
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub unsafe fn register_virtual_irq_sink(sink: RiscvVirtualIrqSink) -> bool {
    let sink = sink as *mut ();
    match VIRTUAL_IRQ_SINK.compare_exchange(
        core::ptr::null_mut(),
        sink,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => true,
        Err(installed) => installed == sink,
    }
}

/// Result category for one VM-wide RISC-V PLIC passthrough route transaction.
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RiscvVirtualIrqRouteStatus {
    /// Every requested endpoint is leased, published, and still masked.
    Prepared          = 0,
    /// Every requested endpoint was activated after route publication.
    Activated         = 1,
    /// A source was zero, outside the PLIC range, or repeated in the request.
    InvalidSource     = 2,
    /// Another route already fixed the monitor-wide target to another CPU.
    ConflictingTarget = 3,
    /// The physical PLIC domain is unavailable.
    DomainUnavailable = 4,
    /// The physical controller could not lease the source endpoint.
    LeaseFailed       = 5,
    /// A previously prepared endpoint has incompatible immutable ownership.
    EndpointConflict  = 6,
    /// The same canonical route is in another transaction phase.
    TransactionBusy   = 7,
    /// A different canonical CPU/source owner is reserved or active.
    RouteConflict     = 8,
}

/// Typed result of preparing and activating a VM-wide passthrough route.
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RiscvVirtualIrqRouteResult {
    status: RiscvVirtualIrqRouteStatus,
    source: u32,
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
impl RiscvVirtualIrqRouteResult {
    const fn prepared() -> Self {
        Self {
            status: RiscvVirtualIrqRouteStatus::Prepared,
            source: 0,
        }
    }

    const fn activated() -> Self {
        Self {
            status: RiscvVirtualIrqRouteStatus::Activated,
            source: 0,
        }
    }

    const fn failed(status: RiscvVirtualIrqRouteStatus, source: u32) -> Self {
        Self { status, source }
    }

    /// Returns the transaction status.
    pub const fn status(self) -> RiscvVirtualIrqRouteStatus {
        self.status
    }

    /// Returns the first source that prevented activation, or zero for a
    /// route-wide failure without a source.
    pub const fn source(self) -> u32 {
        self.source
    }

    /// Returns whether all requested sources were activated.
    pub const fn is_activated(self) -> bool {
        matches!(self.status, RiscvVirtualIrqRouteStatus::Activated)
    }

    /// Returns whether every source is published while still masked.
    pub const fn is_prepared(self) -> bool {
        matches!(self.status, RiscvVirtualIrqRouteStatus::Prepared)
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl VirtualIrqRouteKey {
    fn new(target_cpu: usize, irq_sources: &[u32]) -> Result<Self, RiscvVirtualIrqRouteResult> {
        let mut canonical_sources = [0; VIRTUAL_IRQ_SOURCE_WORDS];
        for &source in irq_sources {
            let source_index = source as usize;
            if source == 0 || source_index >= RISCV_PLIC_SOURCE_COUNT {
                return Err(RiscvVirtualIrqRouteResult::failed(
                    RiscvVirtualIrqRouteStatus::InvalidSource,
                    source,
                ));
            }
            let word = &mut canonical_sources[source_index / u64::BITS as usize];
            let bit = 1 << (source_index % u64::BITS as usize);
            if *word & bit != 0 {
                return Err(RiscvVirtualIrqRouteResult::failed(
                    RiscvVirtualIrqRouteStatus::InvalidSource,
                    source,
                ));
            }
            *word |= bit;
        }
        Ok(Self {
            target_cpu,
            irq_sources: canonical_sources,
        })
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
enum VirtualIrqRoutePreparation {
    Existing,
    Reserved(VirtualIrqPreparePermit),
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
enum VirtualIrqRouteActivation {
    Existing,
    Reserved(VirtualIrqActivatePermit),
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
struct VirtualIrqPreparePermit {
    key: VirtualIrqRouteKey,
    generation: u64,
    rollback: bool,
    not_send: core::marker::PhantomData<*mut ()>,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl VirtualIrqPreparePermit {
    /// Quarantines the reservation after the physical lease commits.
    fn begin_irreversible(&mut self) {
        self.rollback = false;
    }

    fn publish(mut self) {
        let mut state = VIRTUAL_IRQ_ROUTE_CONTROL.lock();
        assert!(
            state.phase
                == (VirtualIrqRoutePhase::Reserved {
                    key: self.key,
                    generation: self.generation,
                }),
            "RISC-V platform route preparation lost its reserved generation"
        );
        state.phase = VirtualIrqRoutePhase::Published {
            key: self.key,
            generation: self.generation,
        };
        self.rollback = false;
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl Drop for VirtualIrqPreparePermit {
    fn drop(&mut self) {
        if !self.rollback {
            return;
        }
        let mut state = VIRTUAL_IRQ_ROUTE_CONTROL.lock();
        assert!(
            state.phase
                == (VirtualIrqRoutePhase::Reserved {
                    key: self.key,
                    generation: self.generation,
                }),
            "RISC-V platform route rollback observed a different generation"
        );
        state.phase = VirtualIrqRoutePhase::Vacant;
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
struct VirtualIrqActivatePermit {
    key: VirtualIrqRouteKey,
    generation: u64,
    rollback: bool,
    not_send: core::marker::PhantomData<*mut ()>,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl VirtualIrqActivatePermit {
    /// Marks the following MMIO activation as infallible and irreversible.
    ///
    /// Every endpoint and ownership invariant must be checked before this
    /// point. A panic after this transition is a fatal platform invariant and
    /// intentionally leaves the route in the activating phase, never falsely
    /// rolling a partially unmasked route back to the published phase.
    fn begin_irreversible(&mut self) {
        self.rollback = false;
    }

    fn finish(self) {
        let mut state = VIRTUAL_IRQ_ROUTE_CONTROL.lock();
        assert!(
            state.phase
                == (VirtualIrqRoutePhase::Activating {
                    key: self.key,
                    generation: self.generation,
                }),
            "RISC-V platform route activation lost its generation"
        );
        state.phase = VirtualIrqRoutePhase::Active {
            key: self.key,
            generation: self.generation,
        };
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl Drop for VirtualIrqActivatePermit {
    fn drop(&mut self) {
        if !self.rollback {
            return;
        }
        let mut state = VIRTUAL_IRQ_ROUTE_CONTROL.lock();
        assert!(
            state.phase
                == (VirtualIrqRoutePhase::Activating {
                    key: self.key,
                    generation: self.generation,
                }),
            "RISC-V platform activation rollback observed a different generation"
        );
        state.phase = VirtualIrqRoutePhase::Published {
            key: self.key,
            generation: self.generation,
        };
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn reserve_virtual_irq_route(
    key: VirtualIrqRouteKey,
) -> Result<VirtualIrqRoutePreparation, RiscvVirtualIrqRouteResult> {
    let mut state = VIRTUAL_IRQ_ROUTE_CONTROL.lock();
    match state.phase {
        VirtualIrqRoutePhase::Vacant => {
            let generation = next_route_generation(state.next_generation);
            state.next_generation = generation;
            state.phase = VirtualIrqRoutePhase::Reserved { key, generation };
            Ok(VirtualIrqRoutePreparation::Reserved(
                VirtualIrqPreparePermit {
                    key,
                    generation,
                    rollback: true,
                    not_send: core::marker::PhantomData,
                },
            ))
        }
        VirtualIrqRoutePhase::Active { key: owner, .. } if owner == key => {
            Ok(VirtualIrqRoutePreparation::Existing)
        }
        VirtualIrqRoutePhase::Reserved { key: owner, .. }
        | VirtualIrqRoutePhase::Published { key: owner, .. }
        | VirtualIrqRoutePhase::Activating { key: owner, .. }
            if owner == key =>
        {
            Err(RiscvVirtualIrqRouteResult::failed(
                RiscvVirtualIrqRouteStatus::TransactionBusy,
                0,
            ))
        }
        _ => Err(RiscvVirtualIrqRouteResult::failed(
            RiscvVirtualIrqRouteStatus::RouteConflict,
            0,
        )),
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn reserve_virtual_irq_activation(
    key: VirtualIrqRouteKey,
) -> Result<VirtualIrqRouteActivation, RiscvVirtualIrqRouteResult> {
    let mut state = VIRTUAL_IRQ_ROUTE_CONTROL.lock();
    match state.phase {
        VirtualIrqRoutePhase::Published {
            key: owner,
            generation,
        } if owner == key => {
            state.phase = VirtualIrqRoutePhase::Activating { key, generation };
            Ok(VirtualIrqRouteActivation::Reserved(
                VirtualIrqActivatePermit {
                    key,
                    generation,
                    rollback: true,
                    not_send: core::marker::PhantomData,
                },
            ))
        }
        VirtualIrqRoutePhase::Active { key: owner, .. } if owner == key => {
            Ok(VirtualIrqRouteActivation::Existing)
        }
        VirtualIrqRoutePhase::Reserved { key: owner, .. }
        | VirtualIrqRoutePhase::Activating { key: owner, .. }
            if owner == key =>
        {
            Err(RiscvVirtualIrqRouteResult::failed(
                RiscvVirtualIrqRouteStatus::TransactionBusy,
                0,
            ))
        }
        _ => Err(RiscvVirtualIrqRouteResult::failed(
            RiscvVirtualIrqRouteStatus::RouteConflict,
            0,
        )),
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
const fn next_route_generation(current: u64) -> u64 {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn prepare_virtual_irq_targets(
    cpu_id: usize,
    irq_sources: &[u32],
    cpu_pin: &ax_percpu::CpuPin,
) -> RiscvVirtualIrqRouteResult {
    if let Some(error) = validate_pinned_virtual_irq_target(cpu_id, cpu_pin) {
        return error;
    }
    let route_key = match VirtualIrqRouteKey::new(cpu_id, irq_sources) {
        Ok(key) => key,
        Err(error) => return error,
    };
    let mut preparation = match reserve_virtual_irq_route(route_key) {
        Ok(VirtualIrqRoutePreparation::Existing) => {
            return RiscvVirtualIrqRouteResult::activated();
        }
        Ok(VirtualIrqRoutePreparation::Reserved(permit)) => permit,
        Err(error) => return error,
    };
    let Some(domain) = somehal::irq::domain_by_kind_fast(somehal::irq::IrqDomainKind::RiscvPlic)
    else {
        return RiscvVirtualIrqRouteResult::failed(
            RiscvVirtualIrqRouteStatus::DomainUnavailable,
            0,
        );
    };

    let mut new_irqs = Vec::with_capacity(irq_sources.len());
    for &source in irq_sources {
        assert!(
            VIRTUAL_IRQ_ENDPOINTS[source as usize].get().is_none(),
            "a vacant RISC-V route transaction retained a leased endpoint"
        );
        new_irqs.push(IrqId::new(domain, ax_plat::irq::HwIrq(source)));
    }
    let affinity = somehal::irq::IrqAffinity::Fixed { cpu_id };
    let endpoints = match somehal::irq::lease_riscv_plic_irq_endpoints(&new_irqs, affinity) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            warn!("cannot atomically lease RISC-V virtual IRQ batch for CPU {cpu_id}: {error:?}");
            return RiscvVirtualIrqRouteResult::failed(
                RiscvVirtualIrqRouteStatus::LeaseFailed,
                new_irqs.first().map_or(0, |irq| irq.hwirq.0),
            );
        }
    };
    // A successful controller batch lease is permanent. From here onward an
    // invariant failure must leave Reserved quarantine; it must never expose
    // a false Vacant state to a second owner.
    preparation.begin_irreversible();

    // The controller batch lease validates every source before changing any
    // source. Once it succeeds, publication is deliberately infallible:
    // returning a recoverable error would strand a permanent physical lease.
    assert_eq!(
        new_irqs.len(),
        endpoints.len(),
        "a successful PLIC batch lease returned a partial endpoint set"
    );
    for (irq_id, endpoint) in new_irqs.iter().copied().zip(endpoints) {
        let source = irq_id.hwirq.0 as usize;
        let installed = VIRTUAL_IRQ_ENDPOINTS[source].call_once(|| RiscvVirtualIrqEndpoint {
            controller_irq: irq_id,
            endpoint,
            target_cpu: cpu_id,
            activated: AtomicBool::new(false),
        });
        assert_eq!(
            installed.controller_irq, irq_id,
            "a reserved PLIC source changed controller identity during publication"
        );
        assert_eq!(
            installed.target_cpu, cpu_id,
            "a reserved PLIC source changed target CPU during publication"
        );
    }
    preparation.publish();
    RiscvVirtualIrqRouteResult::prepared()
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn activate_virtual_irq_targets(
    cpu_id: usize,
    irq_sources: &[u32],
    cpu_pin: &ax_percpu::CpuPin,
) -> RiscvVirtualIrqRouteResult {
    if let Some(error) = validate_pinned_virtual_irq_target(cpu_id, cpu_pin) {
        return error;
    }
    let route_key = match VirtualIrqRouteKey::new(cpu_id, irq_sources) {
        Ok(key) => key,
        Err(error) => return error,
    };
    let mut activation = match reserve_virtual_irq_activation(route_key) {
        Ok(VirtualIrqRouteActivation::Existing) => {
            return RiscvVirtualIrqRouteResult::activated();
        }
        Ok(VirtualIrqRouteActivation::Reserved(permit)) => permit,
        Err(error) => return error,
    };
    for &source in irq_sources {
        let endpoint = VIRTUAL_IRQ_ENDPOINTS[source as usize]
            .get()
            .expect("a published RISC-V route must own every endpoint before activation");
        assert_eq!(
            endpoint.target_cpu, cpu_id,
            "a published RISC-V endpoint changed target CPU before activation"
        );
    }

    // All ordinary failures are checked before this point. Endpoint unmask is
    // an infallible MMIO commit; a panic is a fatal platform invariant and
    // must not make a partially active route appear rollback-safe.
    activation.begin_irreversible();
    for &source in irq_sources {
        activate_virtual_irq_endpoint(source);
    }
    activation.finish();
    RiscvVirtualIrqRouteResult::activated()
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    fn prepare(_vector: TrapVector) {}

    fn init_boot_irqs(cpu_id: usize) -> Result<(), IrqError> {
        somehal::irq::init_boot_irqs(cpu_id)
    }

    #[cfg(feature = "smp")]
    fn init_secondary_boot_irqs(cpu_id: usize) -> Result<(), IrqError> {
        somehal::irq::init_secondary_boot_irqs(cpu_id)
    }

    /// Enables or disables the given IRQ.
    fn set_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
        somehal::irq::irq_set_enable(irq, enabled)
    }

    fn set_affinity(irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
        let affinity = match affinity {
            IrqAffinity::Any => somehal::irq::IrqAffinity::Any,
            IrqAffinity::Fixed(cpu) => somehal::irq::IrqAffinity::Fixed { cpu_id: cpu.0 },
        };
        somehal::irq::irq_set_affinity(irq, affinity)
    }

    /// Handles the IRQ.
    fn handle(vector: TrapVector) -> Option<IrqId> {
        let irq = {
            let active = somehal::irq::begin_irq(vector.0)?;
            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            let controller_irq = active.controller_id();

            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            if forward_claimed_virtual_irq(controller_irq) {
                return Some(controller_irq);
            }

            let irq = active.id();
            dispatch_claimed_host_irq(irq);
            irq
        };
        Some(irq)
    }

    fn send_ipi(
        id: IrqId,
        target: ax_plat::irq::CpuIpiTarget,
        irq_guard: &ax_kspin::IrqGuard,
    ) -> ax_plat::irq::IpiSendStatus {
        somehal::irq::send_ipi(id, target, irq_guard)
    }

    fn ipi_irq() -> IrqId {
        somehal::irq::ipi_irq()
    }

    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError> {
        somehal::irq::resolve_irq_source(source)
    }

    fn resolve_percpu(hwirq: ax_plat::irq::HwIrq) -> Result<IrqId, IrqError> {
        #[cfg(target_arch = "aarch64")]
        {
            somehal::irq::aarch64_gic_irq_id_checked(hwirq)
        }
        #[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
        {
            Ok(IrqId::new(somehal::irq::CPU_LOCAL_IRQ_DOMAIN, hwirq))
        }
        #[cfg(target_arch = "riscv64")]
        {
            Ok(IrqId::new(somehal::irq::CPU_LOCAL_IRQ_DOMAIN, hwirq))
        }
    }
}

fn current_irq_cpu() -> CpuId {
    CpuId(ax_plat::percpu::this_cpu_id())
}

fn dispatch_claimed_host_irq(irq: IrqId) {
    let cpu = current_irq_cpu();
    let outcome = dispatch_irq_on(irq, cpu);
    if outcome.handled {
        return;
    }

    #[cfg(all(target_arch = "loongarch64", feature = "hv"))]
    if is_loongarch_guest_forwardable(irq)
        && loongarch64_hv::inject_virtual_irq(irq.hwirq.0 as usize)
    {
        return;
    }

    if outcome.called == 0 {
        warn!("Unhandled IRQ {irq:?} on CPU {}", cpu.0);
    } else {
        debug!("Spurious IRQ {irq:?}");
    }
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct RiscvPlicSource(usize);

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
impl RiscvPlicSource {
    fn from_irq(irq: IrqId) -> Option<Self> {
        if !somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::RiscvPlic) {
            return None;
        }
        let source = irq.hwirq.0 as usize;
        (1..RISCV_PLIC_SOURCE_COUNT)
            .contains(&source)
            .then_some(Self(source))
    }

    const fn index(self) -> usize {
        self.0
    }
}

#[cfg(test)]
fn is_guest_forwardable(irq: IrqId) -> bool {
    RiscvPlicSource::from_irq(irq).is_some()
}

#[cfg(test)]
fn should_forward_riscv_guest_irq(irq: IrqId, _host_outcome: IrqOutcome) -> bool {
    is_guest_forwardable(irq)
}

#[cfg(test)]
fn riscv_plic_source_index(irq: IrqId) -> Option<usize> {
    RiscvPlicSource::from_irq(irq).map(RiscvPlicSource::index)
}

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
fn is_loongarch_guest_forwardable(irq: IrqId) -> bool {
    somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::LoongArchEioIntc)
        || somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::LoongArchPchPic)
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn claim_and_mask_virtual_irq(vector: usize) -> Option<RiscvForwardedIrq> {
    let active = somehal::irq::begin_irq(vector)?;
    let controller_irq = active.controller_id();
    match mask_forwarded_virtual_irq(controller_irq) {
        ForwardedMaskOutcome::NotForwarded => {
            let host_irq = active.id();
            dispatch_claimed_host_irq(host_irq);
            None
        }
        ForwardedMaskOutcome::Forwarded(claim) => {
            // `active` drops here while local IRQs are still disabled,
            // completing the consumed physical claim. The masked source
            // cannot reassert until the guest publishes completion.
            Some(claim)
        }
        ForwardedMaskOutcome::Quarantined => {
            // A leased guest source in an unexpected generation state must
            // never fall through to a host handler. Dropping `active`
            // completes this physical claim while priority zero keeps the
            // source fail-closed.
            None
        }
    }
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForwardedMaskDecision {
    NotForwarded,
    Forwarded(ForwardedGeneration),
    Quarantined,
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
fn decide_forwarded_mask(
    endpoint_matches: Option<bool>,
    state: &ForwardedIrqState,
) -> ForwardedMaskDecision {
    match endpoint_matches {
        None => ForwardedMaskDecision::NotForwarded,
        Some(false) => ForwardedMaskDecision::Quarantined,
        Some(true) => state.begin_mask().map_or(
            ForwardedMaskDecision::Quarantined,
            ForwardedMaskDecision::Forwarded,
        ),
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
enum ForwardedMaskOutcome {
    NotForwarded,
    Forwarded(RiscvForwardedIrq),
    Quarantined,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn mask_forwarded_virtual_irq(controller_irq: IrqId) -> ForwardedMaskOutcome {
    let Some(source) = RiscvPlicSource::from_irq(controller_irq) else {
        return ForwardedMaskOutcome::NotForwarded;
    };
    let source = source.index();
    let endpoint = VIRTUAL_IRQ_ENDPOINTS[source].get();
    let endpoint_matches = endpoint.map(|endpoint| endpoint.controller_irq == controller_irq);
    match decide_forwarded_mask(endpoint_matches, &FORWARDED_IRQ_STATE[source]) {
        ForwardedMaskDecision::NotForwarded => ForwardedMaskOutcome::NotForwarded,
        ForwardedMaskDecision::Forwarded(generation) => {
            let endpoint = endpoint.expect("a forwarded decision requires a leased endpoint");
            endpoint.endpoint.mask();
            ForwardedMaskOutcome::Forwarded(RiscvForwardedIrq::from_generation(
                source as u32,
                generation,
            ))
        }
        ForwardedMaskDecision::Quarantined => {
            FORWARDED_IRQ_FAULTS.fetch_add(1, Ordering::Relaxed);
            if let Some(endpoint) = endpoint {
                endpoint.endpoint.mask();
            }
            ForwardedMaskOutcome::Quarantined
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn forward_claimed_virtual_irq(controller_irq: IrqId) -> bool {
    let claim = match mask_forwarded_virtual_irq(controller_irq) {
        ForwardedMaskOutcome::NotForwarded => return false,
        ForwardedMaskOutcome::Forwarded(claim) => claim,
        ForwardedMaskOutcome::Quarantined => return true,
    };

    let sink = VIRTUAL_IRQ_SINK.load(Ordering::Acquire);
    if sink.is_null() {
        FORWARDED_IRQ_FAULTS.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    // SAFETY: `register_virtual_irq_sink` stores exactly a function pointer of
    // this signature and the monitor-wide registration is never replaced or
    // unloaded while IRQ delivery is enabled.
    let sink = unsafe { core::mem::transmute::<*mut (), RiscvVirtualIrqSink>(sink) };
    // SAFETY: registration requires a shutdown-stable, non-unwinding,
    // allocation-free hard-IRQ sink. The pointer is immutable after publish.
    if !unsafe { sink(claim.source(), claim.generation()) } {
        FORWARDED_IRQ_FAULTS.fetch_add(1, Ordering::Relaxed);
    }
    true
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn unmask_virtual_irq(claim: RiscvForwardedIrq, current_cpu: usize) -> bool {
    let source = claim.source as usize;
    if !(1..RISCV_PLIC_SOURCE_COUNT).contains(&source) {
        return false;
    }
    let Some(endpoint) = VIRTUAL_IRQ_ENDPOINTS[source].get() else {
        return false;
    };
    if endpoint.target_cpu != current_cpu {
        return false;
    }
    if ax_cpu::asm::irqs_enabled() {
        return false;
    }
    let Some(generation) = ForwardedGeneration::new(claim.generation) else {
        return false;
    };
    let Some(permit) = FORWARDED_IRQ_STATE[source].begin_unmask(generation) else {
        return false;
    };
    endpoint.endpoint.unmask();
    FORWARDED_IRQ_STATE[source].finish_unmask(permit);
    true
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn validate_pinned_virtual_irq_target(
    target_cpu: usize,
    cpu_pin: &ax_percpu::CpuPin,
) -> Option<RiscvVirtualIrqRouteResult> {
    let current_cpu = match ax_percpu::bound_current(cpu_pin) {
        Ok(bound_pin) => bound_pin.cpu_index().as_usize(),
        Err(_) => {
            return Some(RiscvVirtualIrqRouteResult::failed(
                RiscvVirtualIrqRouteStatus::ConflictingTarget,
                0,
            ));
        }
    };
    (current_cpu != target_cpu).then(|| {
        RiscvVirtualIrqRouteResult::failed(RiscvVirtualIrqRouteStatus::ConflictingTarget, 0)
    })
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn activate_virtual_irq_endpoint(irq: u32) {
    let endpoint = VIRTUAL_IRQ_ENDPOINTS[irq as usize]
        .get()
        .expect("all virtual IRQ endpoints are validated before activation");
    activate_endpoint_once(&endpoint.activated, || endpoint.endpoint.unmask());
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
fn activate_endpoint_once(activated: &AtomicBool, activate: impl FnOnce()) -> bool {
    if activated.swap(true, Ordering::AcqRel) {
        return false;
    }
    activate();
    true
}

#[cfg(test)]
fn prepare_and_publish_virtual_irqs<T>(
    prepare: impl FnOnce() -> Result<T, RiscvVirtualIrqRouteResult>,
    publish: impl FnOnce(T),
) -> RiscvVirtualIrqRouteResult {
    let prepared = match prepare() {
        Ok(prepared) => prepared,
        Err(error) => return error,
    };
    publish(prepared);
    RiscvVirtualIrqRouteResult::prepared()
}

#[cfg(test)]
mod tests {
    use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};
    use ax_plat::irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqId};
    use spin::Once;

    struct TestLockRuntime;

    impl_trait! {
        impl LockRuntime for TestLockRuntime {
            fn irq_enter() {}
            fn irq_exit() {}
            fn preempt_enter() {}
            fn preempt_exit() {}
            unsafe fn preempt_exit_irq_return() {}
            fn current_thread_id() -> u64 { 1 }
            fn lockdep_acquire(_event: LockdepEvent) {}
            fn lockdep_release(_event: LockdepEvent) {}
            fn lockdep_set_trace_enabled(_enabled: bool) {}
            fn lockdep_dump_trace() {}
        }
    }

    fn plic_irq(hwirq: u32) -> IrqId {
        static PLIC_DOMAIN: Once<somehal::irq::IrqDomainId> = Once::new();

        let domain = *PLIC_DOMAIN.call_once(|| {
            somehal::irq::domain_by_kind(somehal::irq::IrqDomainKind::RiscvPlic)
                .map(|domain| domain.id)
                .unwrap_or_else(|| {
                    somehal::irq::alloc_irq_domain(
                        rdrive::DeviceId::new(),
                        somehal::irq::IrqDomainKind::RiscvPlic,
                    )
                    .unwrap()
                })
        });
        IrqId::new(domain, HwIrq(hwirq))
    }

    #[test]
    fn cpu_local_irq_is_never_forwarded_to_guest() {
        let irq = IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(5));

        assert!(!super::is_guest_forwardable(irq));
    }

    #[test]
    fn plic_irq_can_be_forwarded_to_guest() {
        let irq = plic_irq(10);

        assert!(super::is_guest_forwardable(irq));
    }

    #[test]
    fn handled_plic_irq_remains_forwardable_to_passthrough_guest() {
        let irq = plic_irq(1);
        let host_outcome = ax_plat::irq::IrqOutcome {
            handled: true,
            wake: false,
            called: 1,
        };

        assert!(super::should_forward_riscv_guest_irq(irq, host_outcome));
    }

    #[test]
    fn unhandled_plic_irq_can_be_forwarded_to_guest() {
        let irq = plic_irq(2);

        assert!(super::should_forward_riscv_guest_irq(
            irq,
            ax_plat::irq::IrqOutcome::default()
        ));
    }

    #[test]
    fn only_real_plic_sources_have_virtual_irq_source_index() {
        let irq = plic_irq(2);
        assert_eq!(super::riscv_plic_source_index(irq), Some(2));

        let reserved = IrqId::new(irq.domain, HwIrq(0));
        assert_eq!(super::riscv_plic_source_index(reserved), None);

        let out_of_range = IrqId::new(irq.domain, HwIrq(super::RISCV_PLIC_SOURCE_COUNT as u32));
        assert_eq!(super::riscv_plic_source_index(out_of_range), None);
    }

    #[test]
    fn stale_completion_cannot_clear_the_next_forwarded_generation() {
        let state = super::ForwardedIrqState::new();

        let generation_one = state.begin_mask().unwrap();
        let permit_one = state.begin_unmask(generation_one).unwrap();
        assert!(state.begin_unmask(generation_one).is_none());
        state.finish_unmask(permit_one);

        let generation_two = state.begin_mask().unwrap();
        assert_ne!(generation_two, generation_one);
        assert!(state.begin_unmask(generation_one).is_none());
        let permit_two = state.begin_unmask(generation_two).unwrap();
        assert!(state.begin_unmask(generation_two).is_none());
        state.finish_unmask(permit_two);
        assert!(state.begin_unmask(generation_two).is_none());
    }

    #[test]
    fn forwarded_generation_rejects_zero_and_shift_aliases() {
        let canonical = 7;
        assert!(super::RiscvForwardedIrq::try_new(10, 0).is_none());
        let max = super::RiscvForwardedIrq::try_new(10, super::FORWARDED_GENERATION_MAX).unwrap();
        assert_eq!(max.source(), 10);
        assert_eq!(max.generation(), super::FORWARDED_GENERATION_MAX);
        let generation = super::ForwardedGeneration::new(canonical).unwrap();
        let canonical_claim = super::RiscvForwardedIrq::from_generation(10, generation);
        assert_eq!(canonical_claim.source(), 10);
        assert_eq!(canonical_claim.generation(), canonical);
        assert!(
            super::RiscvForwardedIrq::try_new(10, canonical + (1u64 << 62)).is_none(),
            "a generation that aliases after the packed-state shift must be rejected"
        );
    }

    #[test]
    fn route_result_reports_every_typed_transaction_status() {
        let prepared = super::RiscvVirtualIrqRouteResult::prepared();
        assert!(prepared.is_prepared());
        assert!(!prepared.is_activated());

        let activated = super::RiscvVirtualIrqRouteResult::activated();
        assert!(activated.is_activated());
        assert!(!activated.is_prepared());

        for status in [
            super::RiscvVirtualIrqRouteStatus::InvalidSource,
            super::RiscvVirtualIrqRouteStatus::ConflictingTarget,
            super::RiscvVirtualIrqRouteStatus::DomainUnavailable,
            super::RiscvVirtualIrqRouteStatus::LeaseFailed,
            super::RiscvVirtualIrqRouteStatus::EndpointConflict,
            super::RiscvVirtualIrqRouteStatus::TransactionBusy,
            super::RiscvVirtualIrqRouteStatus::RouteConflict,
        ] {
            let failed = super::RiscvVirtualIrqRouteResult::failed(status, 11);
            assert_eq!(failed.status(), status);
            assert_eq!(failed.source(), 11);
            assert!(!failed.is_prepared());
            assert!(!failed.is_activated());
        }
    }

    #[test]
    fn leased_source_busy_state_is_quarantined_instead_of_host_dispatched() {
        let state = super::ForwardedIrqState::new();
        assert!(matches!(
            super::decide_forwarded_mask(Some(true), &state),
            super::ForwardedMaskDecision::Forwarded(_)
        ));
        assert_eq!(
            super::decide_forwarded_mask(Some(true), &state),
            super::ForwardedMaskDecision::Quarantined
        );
        assert_eq!(
            super::decide_forwarded_mask(None, &super::ForwardedIrqState::new()),
            super::ForwardedMaskDecision::NotForwarded
        );
    }

    #[test]
    fn second_prepare_failure_activates_no_virtual_irq_endpoint() {
        let mut published = 0;
        let result = super::prepare_and_publish_virtual_irqs(
            || {
                Err::<(), _>(super::RiscvVirtualIrqRouteResult::failed(
                    super::RiscvVirtualIrqRouteStatus::LeaseFailed,
                    11,
                ))
            },
            |_| published += 1,
        );

        assert_eq!(published, 0);
        assert_eq!(
            result.status(),
            super::RiscvVirtualIrqRouteStatus::LeaseFailed
        );
        assert_eq!(result.source(), 11);
    }

    #[test]
    fn an_activated_endpoint_is_never_unmasked_twice() {
        let activated = core::sync::atomic::AtomicBool::new(false);
        let unmask_count = core::sync::atomic::AtomicUsize::new(0);

        assert!(super::activate_endpoint_once(&activated, || {
            unmask_count.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }));
        assert!(!super::activate_endpoint_once(&activated, || {
            unmask_count.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }));
        assert_eq!(unmask_count.load(core::sync::atomic::Ordering::Relaxed), 1);
    }
}
