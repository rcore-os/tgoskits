//! Generation-owned LoongArch physical IRQ routes.

use ax_kspin::SpinNoIrq as Mutex;
use ax_std::os::arceos::{
    modules::ax_hal::irq as host_irq,
    task::{ThreadWakeHandle, WaitQueue, WakeResult, current_thread_handle},
};

use super::AxvmLoongArchVcpu;
use crate::{
    AxVMRef, AxVmError, AxVmResult, architecture::ops::VcpuIrqOwnerSession, vcpu::BoundVcpu,
};

mod state;

use state::RoutePublication;

const LOONGARCH_MAX_IRQ_ROUTES: usize = 256;
const LOONGARCH_ROUTE_OWNER_CPU: usize = 0;

static ROUTE_CATALOG: Mutex<()> = Mutex::new(());
static ROUTE_COMPLETION: WaitQueue = WaitQueue::new();
static GUEST_IRQ_ROUTES: [GuestIrqRouteSlot; LOONGARCH_MAX_IRQ_ROUTES] =
    [const { GuestIrqRouteSlot::new() }; LOONGARCH_MAX_IRQ_ROUTES];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestIrqRouteConfig {
    physical_irq: usize,
    vm_id: usize,
    vcpu_id: usize,
    guest_input: usize,
}

#[derive(Clone, Copy, Debug)]
struct ActiveGuestIrqRoute {
    config: GuestIrqRouteConfig,
    generation: u64,
    handle: host_irq::IrqHandle,
}

#[derive(Clone, Copy, Debug)]
enum GuestIrqRouteState {
    Vacant,
    Prepared {
        config: GuestIrqRouteConfig,
        generation: u64,
    },
    Installing {
        config: GuestIrqRouteConfig,
        generation: u64,
    },
    Active(ActiveGuestIrqRoute),
    RevokeRequested(ActiveGuestIrqRoute),
    Revoking(ActiveGuestIrqRoute),
    ReleaseFailed {
        route: ActiveGuestIrqRoute,
        error: host_irq::IrqError,
    },
}

impl GuestIrqRouteState {
    const fn configuration(self) -> Option<GuestIrqRouteConfig> {
        match self {
            Self::Vacant => None,
            Self::Prepared { config, .. }
            | Self::Installing { config, .. }
            | Self::Active(ActiveGuestIrqRoute { config, .. })
            | Self::RevokeRequested(ActiveGuestIrqRoute { config, .. })
            | Self::Revoking(ActiveGuestIrqRoute { config, .. })
            | Self::ReleaseFailed {
                route: ActiveGuestIrqRoute { config, .. },
                ..
            } => Some(config),
        }
    }
}

struct GuestIrqRouteSlot {
    publication: RoutePublication,
    control: Mutex<GuestIrqRouteState>,
    owner_wake: Mutex<Option<RouteOwnerWake>>,
}

struct RouteOwnerWake {
    vm_id: usize,
    vcpu_id: usize,
    generation: u64,
    wake: ThreadWakeHandle,
}

impl GuestIrqRouteSlot {
    const fn new() -> Self {
        Self {
            publication: RoutePublication::new(),
            control: Mutex::new(GuestIrqRouteState::Vacant),
            owner_wake: Mutex::new(None),
        }
    }

    fn capture(
        &self,
        generation: u64,
        wake: &ThreadWakeHandle,
        irq_cpu: host_irq::CpuId,
    ) -> host_irq::IrqReturn {
        let _capture = self.publication.capture(generation);
        let wake_target = wake.target_cpu().map(|cpu| cpu.as_u32() as usize);
        if irq_cpu.0 == LOONGARCH_ROUTE_OWNER_CPU && wake_target == Some(LOONGARCH_ROUTE_OWNER_CPU)
        {
            let _wake_published = matches!(
                wake.wake(),
                WakeResult::Notified | WakeResult::AlreadyPending
            );
        }

        // The passthrough endpoint cannot acknowledge the guest-owned device.
        // Quench the exclusive physical line until a typed guest PCH-PIC
        // deassertion proves that task context may reopen this exact action.
        host_irq::IrqReturn::MaskLineAndWake
    }

    fn active_route_for(&self, vm_id: usize, vcpu_id: usize) -> Option<ActiveGuestIrqRoute> {
        let state = *self.control.lock();
        match state {
            GuestIrqRouteState::Active(route)
                if route.config.vm_id == vm_id && route.config.vcpu_id == vcpu_id =>
            {
                Some(route)
            }
            _ => None,
        }
    }

    fn install_owner_wake(&self, route: ActiveGuestIrqRoute, wake: ThreadWakeHandle) {
        let mut slot = self.owner_wake.lock();
        assert!(
            slot.is_none(),
            "LoongArch physical IRQ {} retained an older owner wake",
            route.config.physical_irq
        );
        *slot = Some(RouteOwnerWake {
            vm_id: route.config.vm_id,
            vcpu_id: route.config.vcpu_id,
            generation: route.generation,
            wake,
        });
    }

    fn owner_wake_for(&self, route: ActiveGuestIrqRoute) -> AxVmResult<ThreadWakeHandle> {
        let wake = self.owner_wake.lock();
        let wake = wake.as_ref().ok_or_else(|| {
            AxVmError::invalid_state(
                "load LoongArch guest IRQ owner wake",
                format_args!(
                    "physical IRQ {} has no retained owner",
                    route.config.physical_irq
                ),
            )
        })?;
        if wake.vm_id != route.config.vm_id
            || wake.vcpu_id != route.config.vcpu_id
            || wake.generation != route.generation
        {
            return Err(AxVmError::invalid_state(
                "load LoongArch guest IRQ owner wake",
                format_args!(
                    "physical IRQ {} owner generation does not match its action",
                    route.config.physical_irq
                ),
            ));
        }
        Ok(wake.wake.clone())
    }

    fn take_owner_wake(&self, route: ActiveGuestIrqRoute) -> ThreadWakeHandle {
        let wake = self
            .owner_wake
            .lock()
            .take()
            .expect("active LoongArch IRQ action retains its owner wake");
        assert!(
            wake.vm_id == route.config.vm_id
                && wake.vcpu_id == route.config.vcpu_id
                && wake.generation == route.generation,
            "LoongArch IRQ action released a foreign owner wake"
        );
        wake.wake
    }
}

/// Reserves one LoongArch host IRQ route for later owner-thread activation.
///
/// This post-storage-handoff phase does not enable the physical line. The
/// fixed vCPU owner registers and enables its own action from
/// [`activate_guest_irq_owner`] before entering the guest.
pub fn register_guest_irq_route(
    physical_irq: usize,
    vm_id: usize,
    vcpu_id: usize,
    guest_vector: usize,
) -> AxVmResult {
    let slot = GUEST_IRQ_ROUTES.get(physical_irq).ok_or_else(|| {
        AxVmError::invalid_input(
            "reserve LoongArch guest IRQ route",
            format_args!("physical IRQ {physical_irq} exceeds the fixed route table"),
        )
    })?;
    let config = GuestIrqRouteConfig {
        physical_irq,
        vm_id,
        vcpu_id,
        guest_input: guest_vector,
    };
    let _catalog = ROUTE_CATALOG.lock();
    reject_conflicting_guest_input(config)?;

    let mut route = slot.control.lock();
    match *route {
        GuestIrqRouteState::Vacant => {
            let generation = slot.publication.allocate_generation().ok_or_else(|| {
                AxVmError::resource_unavailable(
                    "LoongArch guest IRQ route generation",
                    format_args!("physical IRQ {physical_irq} exhausted its generation space"),
                )
            })?;
            *route = GuestIrqRouteState::Prepared { config, generation };
            Ok(())
        }
        current if current.configuration() == Some(config) => Ok(()),
        current => Err(AxVmError::resource_conflict(
            "LoongArch guest IRQ route",
            format_args!("physical IRQ {physical_irq} is already owned by {current:?}"),
        )),
    }
}

/// Acquires the long-lived owner session before the vCPU registers an action.
pub(super) fn prepare_guest_irq_owner_session(
    vm: &AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>,
) -> AxVmResult<Option<VcpuIrqOwnerSession>> {
    if vcpu.id() != 0 || !guest_irq_owner_session_required(vm.id(), vcpu.id()) {
        return Ok(None);
    }
    require_fixed_owner_cpu(vcpu)?;
    let session = VcpuIrqOwnerSession::acquire(
        vm.id(),
        vcpu.id(),
        guest_irq_owner_release_requested,
        owner_release_guest_irq_routes,
    )?;
    if session.owner_cpu() != LOONGARCH_ROUTE_OWNER_CPU {
        return Err(AxVmError::resource_conflict(
            "LoongArch guest IRQ owner CPU",
            format_args!(
                "owner session acquired CPU {}, expected CPU {}",
                session.owner_cpu(),
                LOONGARCH_ROUTE_OWNER_CPU
            ),
        ));
    }
    Ok(Some(session))
}

fn guest_irq_owner_session_required(vm_id: usize, vcpu_id: usize) -> bool {
    GUEST_IRQ_ROUTES.iter().any(|slot| {
        slot.control
            .lock()
            .configuration()
            .is_some_and(|config| config.vm_id == vm_id && config.vcpu_id == vcpu_id)
    })
}

/// Installs every route belonging to the current fixed vCPU owner.
pub(super) fn activate_guest_irq_owner(
    vm: &AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>,
) -> AxVmResult {
    if vcpu.id() != 0 {
        return Ok(());
    }
    require_fixed_owner_cpu(vcpu)?;
    let thread = current_thread_handle()
        .map_err(|error| AxVmError::resource_unavailable("LoongArch IRQ owner thread", error))?;
    let wake = thread.wake_handle();
    drop(thread);
    require_wake_on_owner_cpu(&wake)?;

    for physical_irq in 0..GUEST_IRQ_ROUTES.len() {
        if route_is_prepared_for(physical_irq, vm.id(), vcpu.id())
            && let Err(activation_error) = install_guest_irq_action(physical_irq, wake.clone())
        {
            let revocation = owner_release_guest_irq_routes(vm.id(), vcpu.id());
            return Err(match revocation {
                Ok(()) => activation_error,
                Err(revocation_error) => AxVmError::interrupt(
                    "activate LoongArch guest IRQ routes",
                    format_args!(
                        "activation failed: {activation_error}; rollback failed: \
                         {revocation_error}"
                    ),
                ),
            });
        }
    }
    Ok(())
}

/// Drains stable hard-IRQ facts through the bound vCPU owner.
pub(super) fn drain_guest_irq_publications(
    vm: &AxVMRef,
    vcpu: &BoundVcpu<'_, '_, AxvmLoongArchVcpu>,
) -> AxVmResult {
    for slot in &GUEST_IRQ_ROUTES {
        let Some(route) = slot.active_route_for(vm.id(), vcpu.id()) else {
            continue;
        };
        if !slot.publication.take_pending(route.generation) {
            continue;
        }
        let Some(vector) = super::loongarch_external_irq_vector(
            vm,
            route.config.guest_input,
            route.config.physical_irq,
        ) else {
            continue;
        };
        let result = vcpu.with_arch_vcpu("inject LoongArch passthrough IRQ", |arch_vcpu| {
            arch_vcpu.inject_external_interrupt(vector, route.config.physical_irq)
        })?;
        if let Err(error) = result {
            slot.publication.restore_pending(route.generation);
            return Err(error);
        }
    }
    Ok(())
}

/// Publishes a guest PCH-PIC EOI to the fixed action owner.
///
/// The MMIO exit may belong to another vCPU, so it cannot operate on the IRQ
/// handle. The owner consumes this generation-bearing fact from
/// [`service_guest_irq_owner`] in ordinary task context.
pub(super) fn complete_guest_irq_route(vm_id: usize, guest_input: usize) -> AxVmResult {
    let mut owner_wake = None;
    for slot in &GUEST_IRQ_ROUTES {
        let Some(route) = slot.active_route_for(vm_id, 0) else {
            continue;
        };
        if route.config.guest_input != guest_input {
            continue;
        }
        if slot.publication.request_rearm(route.generation) {
            merge_owner_wake(
                &mut owner_wake,
                slot.owner_wake_for(route)?,
                "publish LoongArch guest IRQ rearm",
            )?;
        }
    }
    if let Some(wake) = owner_wake {
        wake_route_owner(&wake, "publish LoongArch guest IRQ rearm")?;
    }
    Ok(())
}

/// Applies guest EOI/rearm facts on the thread that registered the action.
pub(super) fn service_guest_irq_owner(
    vm: &AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>,
) -> AxVmResult {
    if vcpu.id() != 0 {
        return Ok(());
    }
    require_fixed_owner_cpu(vcpu)?;

    for slot in &GUEST_IRQ_ROUTES {
        let Some(route) = slot.active_route_for(vm.id(), vcpu.id()) else {
            continue;
        };
        if !slot.publication.take_rearm_request(route.generation) {
            continue;
        }
        if let Err(error) = host_irq::synchronize_irq(route.handle) {
            slot.publication.restore_rearm_request(route.generation);
            return Err(route_irq_error(
                "synchronize LoongArch guest IRQ before rearm",
                route.config.physical_irq,
                error,
            ));
        }

        // Serialize the short controller unmask against manager publication.
        // A manager that won the state transition leaves the line quenched for
        // the owner-close protocol instead of allowing a late guest EOI to
        // reopen it.
        let state = slot.control.lock();
        if !matches!(
            *state,
            GuestIrqRouteState::Active(observed)
                if observed.generation == route.generation
                    && observed.config == route.config
        ) {
            continue;
        }
        if !slot.publication.begin_rearm(route.generation) {
            continue;
        }
        if let Err(error) = host_irq::release_irq_quench(route.handle) {
            slot.publication.restore_quench(route.generation);
            slot.publication.restore_rearm_request(route.generation);
            return Err(route_irq_error(
                "rearm LoongArch guest IRQ action",
                route.config.physical_irq,
                error,
            ));
        }
    }
    Ok(())
}

/// Requests owner-thread route close and waits for its typed completion.
///
/// This manager-side entry never operates on an [`host_irq::IrqHandle`]. The
/// fixed vCPU owner observes the preallocated cause, performs every fallible
/// IRQ operation, and publishes either Vacant or ReleaseFailed before waking
/// this wait queue.
pub fn revoke_guest_irq_routes(vm_id: usize) -> AxVmResult {
    let owner_wake = request_guest_irq_route_revocation(vm_id)?;
    if let Some(wake) = owner_wake {
        wake_route_owner(&wake, "request LoongArch guest IRQ owner close")?;
        ROUTE_COMPLETION
            .try_wait_until(|| guest_irq_route_revocation_finished(vm_id))
            .map_err(|error| {
                AxVmError::resource_unavailable("wait for LoongArch guest IRQ owner close", error)
            })?;
    }
    guest_irq_route_revocation_result(vm_id)
}

fn request_guest_irq_route_revocation(vm_id: usize) -> AxVmResult<Option<ThreadWakeHandle>> {
    let _catalog = ROUTE_CATALOG.lock();
    let mut owner_wake = None;

    // Validate the complete domain before publishing any irreversible state.
    // A corrupt later route must not strand an earlier route in
    // RevokeRequested without waking its retained owner.
    for slot in &GUEST_IRQ_ROUTES {
        let state = *slot.control.lock();
        match state {
            GuestIrqRouteState::Installing { config, .. } if config.vm_id == vm_id => {
                return Err(AxVmError::invalid_state(
                    "request LoongArch guest IRQ route revocation",
                    format_args!("physical IRQ {} is still installing", config.physical_irq),
                ));
            }
            GuestIrqRouteState::Active(route)
            | GuestIrqRouteState::RevokeRequested(route)
            | GuestIrqRouteState::Revoking(route)
                if route.config.vm_id == vm_id =>
            {
                merge_owner_wake(
                    &mut owner_wake,
                    slot.owner_wake_for(route)?,
                    "request LoongArch guest IRQ owner close",
                )?;
            }
            GuestIrqRouteState::ReleaseFailed { route, error } if route.config.vm_id == vm_id => {
                return Err(route_irq_error(
                    "LoongArch guest IRQ owner close",
                    route.config.physical_irq,
                    error,
                ));
            }
            _ => {}
        }
    }

    for slot in &GUEST_IRQ_ROUTES {
        let mut state = slot.control.lock();
        match *state {
            GuestIrqRouteState::Vacant => {}
            GuestIrqRouteState::Prepared { config, generation } if config.vm_id == vm_id => {
                slot.publication.clear_after_release(generation);
                *state = GuestIrqRouteState::Vacant;
            }
            GuestIrqRouteState::Active(route) if route.config.vm_id == vm_id => {
                slot.publication.deactivate(route.generation);
                *state = GuestIrqRouteState::RevokeRequested(route);
            }
            _ => {}
        }
    }
    Ok(owner_wake)
}

fn merge_owner_wake(
    retained: &mut Option<ThreadWakeHandle>,
    candidate: ThreadWakeHandle,
    operation: &'static str,
) -> AxVmResult {
    require_wake_on_owner_cpu(&candidate)?;
    if let Some(existing) = retained {
        if existing.thread_id() != candidate.thread_id()
            || existing.target_cpu() != candidate.target_cpu()
        {
            return Err(AxVmError::resource_conflict(
                operation,
                "one LoongArch IRQ domain resolved to multiple owner threads",
            ));
        }
    } else {
        *retained = Some(candidate);
    }
    Ok(())
}

fn wake_route_owner(wake: &ThreadWakeHandle, operation: &'static str) -> AxVmResult {
    match wake.wake() {
        WakeResult::Notified | WakeResult::AlreadyPending => Ok(()),
        WakeResult::Exited => Err(AxVmError::resource_unavailable(
            operation,
            "the fixed LoongArch IRQ owner thread has exited",
        )),
        WakeResult::Unavailable => Err(AxVmError::resource_unavailable(
            operation,
            "the fixed LoongArch IRQ owner CPU is unavailable",
        )),
    }
}

fn guest_irq_route_revocation_finished(vm_id: usize) -> bool {
    GUEST_IRQ_ROUTES.iter().all(|slot| {
        let state = *slot.control.lock();
        match state {
            GuestIrqRouteState::ReleaseFailed { .. } => true,
            _ => state
                .configuration()
                .is_none_or(|config| config.vm_id != vm_id),
        }
    })
}

fn guest_irq_route_revocation_result(vm_id: usize) -> AxVmResult {
    for slot in &GUEST_IRQ_ROUTES {
        let state = *slot.control.lock();
        match state {
            GuestIrqRouteState::ReleaseFailed { route, error } if route.config.vm_id == vm_id => {
                return Err(route_irq_error(
                    "LoongArch guest IRQ owner close",
                    route.config.physical_irq,
                    error,
                ));
            }
            state
                if state
                    .configuration()
                    .is_some_and(|config| config.vm_id == vm_id) =>
            {
                return Err(AxVmError::invalid_state(
                    "complete LoongArch guest IRQ route revocation",
                    format_args!("route remains in {state:?}"),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn reject_conflicting_guest_input(candidate: GuestIrqRouteConfig) -> AxVmResult {
    if GUEST_IRQ_ROUTES.iter().any(|slot| {
        slot.control.lock().configuration().is_some_and(|config| {
            config.vm_id == candidate.vm_id
                && config.guest_input == candidate.guest_input
                && config.physical_irq != candidate.physical_irq
        })
    }) {
        return Err(AxVmError::resource_conflict(
            "LoongArch guest PCH-PIC input",
            format_args!(
                "VM[{}] input {} already maps another physical IRQ",
                candidate.vm_id, candidate.guest_input
            ),
        ));
    }
    Ok(())
}

fn require_fixed_owner_cpu(vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>) -> AxVmResult {
    if vcpu.phys_cpu_set() != Some(1usize << LOONGARCH_ROUTE_OWNER_CPU) {
        return Err(AxVmError::invalid_config(format_args!(
            "LoongArch passthrough VM[{}] VCpu[{}] must be fixed to host CPU {}",
            vcpu.vm_id(),
            vcpu.id(),
            LOONGARCH_ROUTE_OWNER_CPU
        )));
    }
    Ok(())
}

fn require_wake_on_owner_cpu(wake: &ThreadWakeHandle) -> AxVmResult {
    let target = wake.target_cpu().map(|cpu| cpu.as_u32() as usize);
    if target != Some(LOONGARCH_ROUTE_OWNER_CPU) {
        return Err(AxVmError::resource_conflict(
            "LoongArch guest IRQ owner CPU",
            format_args!("owner wake targets {target:?}, expected CPU {LOONGARCH_ROUTE_OWNER_CPU}"),
        ));
    }
    Ok(())
}

fn route_is_prepared_for(physical_irq: usize, vm_id: usize, vcpu_id: usize) -> bool {
    matches!(
        *GUEST_IRQ_ROUTES[physical_irq].control.lock(),
        GuestIrqRouteState::Prepared { config, .. }
            if config.vm_id == vm_id && config.vcpu_id == vcpu_id
    )
}

fn install_guest_irq_action(physical_irq: usize, wake: ThreadWakeHandle) -> AxVmResult {
    let slot = &GUEST_IRQ_ROUTES[physical_irq];
    let (config, generation) = {
        let mut route = slot.control.lock();
        let GuestIrqRouteState::Prepared { config, generation } = *route else {
            return Ok(());
        };
        *route = GuestIrqRouteState::Installing { config, generation };
        (config, generation)
    };

    let irq = resolve_physical_irq(physical_irq).map_err(|error| {
        restore_prepared_route(slot, config, generation);
        route_irq_error("resolve LoongArch guest IRQ", physical_irq, error)
    })?;
    let callback_wake = wake.clone();
    let request = host_irq::IrqRequest::new(move |context| {
        slot.capture(generation, &callback_wake, context.cpu)
    })
    .affinity(host_irq::IrqAffinity::Fixed(host_irq::CpuId(
        LOONGARCH_ROUTE_OWNER_CPU,
    )))
    .share_mode(host_irq::ShareMode::Exclusive)
    .auto_enable(host_irq::AutoEnable::No);
    let handle = host_irq::request_irq(irq, request).map_err(|error| {
        restore_prepared_route(slot, config, generation);
        route_irq_error("reserve LoongArch guest IRQ action", physical_irq, error)
    })?;
    let active = ActiveGuestIrqRoute {
        config,
        generation,
        handle,
    };
    {
        let mut route = slot.control.lock();
        assert!(matches!(
            *route,
            GuestIrqRouteState::Installing {
                config: observed,
                generation: observed_generation,
            } if observed == config && observed_generation == generation
        ));
        slot.install_owner_wake(active, wake);
        *route = GuestIrqRouteState::Active(active);
        slot.publication.activate(generation);
    }
    if let Err(error) = host_irq::enable_irq(handle) {
        slot.publication.deactivate(generation);
        return Err(route_irq_error(
            "enable LoongArch guest IRQ action",
            physical_irq,
            error,
        ));
    }
    Ok(())
}

fn restore_prepared_route(slot: &GuestIrqRouteSlot, config: GuestIrqRouteConfig, generation: u64) {
    let mut route = slot.control.lock();
    if matches!(
        *route,
        GuestIrqRouteState::Installing {
            config: observed,
            generation: observed_generation,
        } if observed == config && observed_generation == generation
    ) {
        *route = GuestIrqRouteState::Prepared { config, generation };
    }
}

fn guest_irq_owner_release_requested(vm_id: usize, vcpu_id: usize) -> bool {
    GUEST_IRQ_ROUTES.iter().any(|slot| {
        matches!(
            *slot.control.lock(),
            GuestIrqRouteState::RevokeRequested(route)
                if route.config.vm_id == vm_id && route.config.vcpu_id == vcpu_id
        )
    })
}

fn owner_release_guest_irq_routes(vm_id: usize, vcpu_id: usize) -> AxVmResult {
    let mut first_error = None;
    for slot in &GUEST_IRQ_ROUTES {
        match begin_owner_route_release(slot, vm_id, vcpu_id) {
            Ok(Some(route)) => {
                if let Err(error) = owner_release_guest_irq_action(slot, route) {
                    first_error.get_or_insert(error);
                }
            }
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    ROUTE_COMPLETION.notify_all();
    first_error.map_or(Ok(()), Err)
}

fn begin_owner_route_release(
    slot: &GuestIrqRouteSlot,
    vm_id: usize,
    vcpu_id: usize,
) -> AxVmResult<Option<ActiveGuestIrqRoute>> {
    let mut state = slot.control.lock();
    match *state {
        GuestIrqRouteState::Vacant => Ok(None),
        GuestIrqRouteState::Prepared { config, generation }
            if config.vm_id == vm_id && config.vcpu_id == vcpu_id =>
        {
            slot.publication.clear_after_release(generation);
            *state = GuestIrqRouteState::Vacant;
            Ok(None)
        }
        GuestIrqRouteState::Installing { config, .. }
            if config.vm_id == vm_id && config.vcpu_id == vcpu_id =>
        {
            Err(AxVmError::invalid_state(
                "owner-close LoongArch guest IRQ route",
                format_args!("physical IRQ {} is still installing", config.physical_irq),
            ))
        }
        GuestIrqRouteState::Active(route)
        | GuestIrqRouteState::RevokeRequested(route)
        | GuestIrqRouteState::Revoking(route)
            if route.config.vm_id == vm_id && route.config.vcpu_id == vcpu_id =>
        {
            slot.publication.deactivate(route.generation);
            *state = GuestIrqRouteState::Revoking(route);
            Ok(Some(route))
        }
        GuestIrqRouteState::ReleaseFailed { route, .. }
            if route.config.vm_id == vm_id && route.config.vcpu_id == vcpu_id =>
        {
            *state = GuestIrqRouteState::Revoking(route);
            Ok(Some(route))
        }
        _ => Ok(None),
    }
}

fn owner_release_guest_irq_action(
    slot: &GuestIrqRouteSlot,
    route: ActiveGuestIrqRoute,
) -> AxVmResult {
    if let Err(error) = host_irq::disable_irq(route.handle) {
        return record_route_release_failure(slot, route, error);
    }
    if let Err(error) = host_irq::synchronize_irq(route.handle) {
        return record_route_release_failure(slot, route, error);
    }
    if slot.publication.begin_rearm(route.generation)
        && let Err(error) = host_irq::release_irq_quench(route.handle)
    {
        slot.publication.restore_quench(route.generation);
        return record_route_release_failure(slot, route, error);
    }
    if let Err(error) = host_irq::free_irq(route.handle) {
        return record_route_release_failure(slot, route, error);
    }

    slot.publication.clear_after_release(route.generation);
    let mut state = slot.control.lock();
    assert!(matches!(
        *state,
        GuestIrqRouteState::Revoking(observed) if observed.generation == route.generation
    ));
    let owner_wake = slot.take_owner_wake(route);
    *state = GuestIrqRouteState::Vacant;
    drop(state);
    // The action callback is gone, so the final wake reference can be
    // released safely in its owner task context.
    drop(owner_wake);
    Ok(())
}

fn record_route_release_failure(
    slot: &GuestIrqRouteSlot,
    route: ActiveGuestIrqRoute,
    error: host_irq::IrqError,
) -> AxVmResult {
    *slot.control.lock() = GuestIrqRouteState::ReleaseFailed { route, error };
    Err(route_irq_error(
        "release LoongArch guest IRQ action",
        route.config.physical_irq,
        error,
    ))
}

fn resolve_physical_irq(physical_irq: usize) -> Result<host_irq::IrqId, host_irq::IrqError> {
    let gsi = u32::try_from(physical_irq).map_err(|_| host_irq::IrqError::InvalidIrq)?;
    host_irq::resolve_irq_source(host_irq::IrqSource::AcpiGsi(gsi))
}

fn route_irq_error(
    operation: &'static str,
    physical_irq: usize,
    error: host_irq::IrqError,
) -> AxVmError {
    AxVmError::interrupt(
        operation,
        format_args!("physical IRQ {physical_irq}: {error:?}"),
    )
}
