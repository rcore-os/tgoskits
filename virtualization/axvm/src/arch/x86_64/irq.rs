use std::vec::Vec;

use ax_std::os::arceos::sync::RawSpinLock as Mutex;
use axdevice::*;
use axvm_types::VmArchVcpuOps;

use crate::{
    InterruptTriggerMode,
    arch::x86_64::{
        X86InterruptDomain, X86InterruptDomainRuntimeKey,
        host_irq::{self as irq, IrqSource},
    },
    runtime::{VCpuRef, VMRef},
};

pub(super) const IOAPIC_GSI_COUNT: usize = 24;

const PIT_TIMER_GSI: usize = 0;
const COM1_GSI: usize = 4;
type IoApicForwardingActivator = fn();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForwardingRouteError {
    UnsupportedGsi,
    AlreadyActive,
    HostIrqConflict,
    HostOwnedConsole,
    ResolveHostIrq(irq::IrqError),
}

#[derive(Clone, Copy)]
struct IoApicForwardingRoute {
    host_irq: Option<irq::IrqId>,
    explicit: bool,
    level_triggered: bool,
    activator: Option<IoApicForwardingActivator>,
}

impl IoApicForwardingRoute {
    const fn new() -> Self {
        Self {
            host_irq: None,
            explicit: false,
            level_triggered: false,
            activator: None,
        }
    }
}

/// VM-owned runtime state for x86 IOAPIC host IRQ forwarding.
///
/// The only remaining global state is the host IRQ lease table below, because
/// physical IRQ ownership is a host-wide resource. All guest-visible route,
/// pending, mask and activation state lives in the VM's interrupt domain.
pub(super) struct X86IoApicForwardingState {
    enabled: bool,
    hooks_registered: bool,
    owner_vcpu_id: Option<usize>,
    pending: usize,
    pending_level: usize,
    masked: usize,
    activated: usize,
    routes: [IoApicForwardingRoute; IOAPIC_GSI_COUNT],
}

impl X86IoApicForwardingState {
    pub(super) const fn new() -> Self {
        Self {
            enabled: false,
            hooks_registered: false,
            owner_vcpu_id: None,
            pending: 0,
            pending_level: 0,
            masked: 0,
            activated: 0,
            routes: [IoApicForwardingRoute::new(); IOAPIC_GSI_COUNT],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HostIrqLease {
    host_irq: irq::IrqId,
    vm_id: usize,
}

static HOST_IRQ_FORWARDING_LEASES: Mutex<Vec<HostIrqLease>> = Mutex::new(Vec::new());

fn should_register_ioapic_gsi_hook(gsi: usize) -> bool {
    gsi < IOAPIC_GSI_COUNT && gsi != PIT_TIMER_GSI && gsi != COM1_GSI
}

fn host_irq_is_guest_assignable(
    host_irq: irq::IrqId,
    host_console_irq: Option<irq::IrqId>,
) -> bool {
    host_console_irq != Some(host_irq)
}

fn ioapic_irq_hook_gsis() -> impl Iterator<Item = usize> {
    (0..IOAPIC_GSI_COUNT).filter(|gsi| should_register_ioapic_gsi_hook(*gsi))
}

fn interrupt_domain_for_vm(vm: &crate::AxVMRef) -> Option<std::sync::Arc<X86InterruptDomain>> {
    vm.get_devices()
        .ok()?
        .services()
        .require::<X86InterruptDomainRuntimeKey>()
        .ok()
}

fn require_interrupt_domain(
    vm: &crate::AxVMRef,
    operation: &'static str,
) -> crate::AxVmResult<std::sync::Arc<X86InterruptDomain>> {
    interrupt_domain_for_vm(vm).ok_or_else(|| crate::AxVmError::ResourceUnavailable {
        resource: "x86 interrupt domain",
        detail: std::format!("VM[{}] must be prepared before {operation}", vm.id()),
    })
}

fn forwarding_route_error(
    vm_id: usize,
    guest_gsi: usize,
    host_irq: impl std::fmt::Debug,
    error: ForwardingRouteError,
) -> crate::AxVmError {
    let detail = match error {
        ForwardingRouteError::UnsupportedGsi => {
            std::format!("guest GSI {guest_gsi} is not supported")
        }
        ForwardingRouteError::AlreadyActive => std::format!(
            "VM[{vm_id}] forwarding is already active; routes must be configured before the boot \
             vCPU starts"
        ),
        ForwardingRouteError::HostIrqConflict => std::format!(
            "host IRQ {host_irq:?} is already mapped to a different guest GSI in VM[{vm_id}]"
        ),
        ForwardingRouteError::HostOwnedConsole => {
            std::format!("host IRQ {host_irq:?} belongs to the runtime-selected physical console")
        }
        ForwardingRouteError::ResolveHostIrq(error) => {
            std::format!("failed to resolve host IRQ for guest GSI {guest_gsi}: {error:?}")
        }
    };
    crate::AxVmError::Interrupt {
        operation: "register x86 IOAPIC forwarding route",
        detail,
    }
}

impl X86InterruptDomain {
    fn set_forwarding_owner(&self, vcpu_id: usize) -> bool {
        let mut state = self.forwarding.lock();
        if state.enabled {
            return false;
        }
        state.owner_vcpu_id = Some(vcpu_id);
        state.enabled = true;
        true
    }

    #[cfg(test)]
    fn has_registered_forwarding_hooks_for(&self, vcpu_id: usize) -> bool {
        let state = self.forwarding.lock();
        state.hooks_registered && state.owner_vcpu_id == Some(vcpu_id)
    }

    fn mark_forwarding_hooks_registered(&self) {
        self.forwarding.lock().hooks_registered = true;
    }

    fn register_forwarding_route(
        &self,
        guest_gsi: usize,
        host_irq: irq::IrqId,
        trigger: InterruptTriggerMode,
        explicit: bool,
        host_console_irq: Option<irq::IrqId>,
    ) -> Result<(), ForwardingRouteError> {
        if !host_irq_is_guest_assignable(host_irq, host_console_irq) {
            return Err(ForwardingRouteError::HostOwnedConsole);
        }
        let mut state = self.forwarding.lock();
        if guest_gsi >= state.routes.len() {
            return Err(ForwardingRouteError::UnsupportedGsi);
        }
        if state.enabled && explicit {
            return Err(ForwardingRouteError::AlreadyActive);
        }
        if state
            .routes
            .iter()
            .enumerate()
            .any(|(gsi, route)| gsi != guest_gsi && route.host_irq == Some(host_irq))
        {
            return Err(ForwardingRouteError::HostIrqConflict);
        }
        let route = &mut state.routes[guest_gsi];
        route.host_irq = Some(host_irq);
        route.explicit = explicit;
        route.level_triggered = matches!(trigger, InterruptTriggerMode::LevelTriggered);
        Ok(())
    }

    fn register_forwarding_activator(
        &self,
        guest_gsi: usize,
        activator: IoApicForwardingActivator,
    ) -> Result<(), ForwardingRouteError> {
        let mut state = self.forwarding.lock();
        if guest_gsi >= state.routes.len() {
            return Err(ForwardingRouteError::UnsupportedGsi);
        }
        if state.enabled {
            return Err(ForwardingRouteError::AlreadyActive);
        }
        state.routes[guest_gsi].activator = Some(activator);
        Ok(())
    }

    fn forwarded_host_irq_for_guest_gsi(
        &self,
        guest_gsi: usize,
        host_console_irq: Option<irq::IrqId>,
    ) -> Result<irq::IrqId, ForwardingRouteError> {
        if let Some(host_irq) = self
            .forwarding
            .lock()
            .routes
            .get(guest_gsi)
            .and_then(|route| route.host_irq)
        {
            if !host_irq_is_guest_assignable(host_irq, host_console_irq) {
                return Err(ForwardingRouteError::HostOwnedConsole);
            }
            return Ok(host_irq);
        }

        let source = IrqSource::AcpiGsi(guest_gsi as u32);
        let host_irq =
            irq::resolve_irq_source(source).map_err(ForwardingRouteError::ResolveHostIrq)?;
        self.register_forwarding_route(
            guest_gsi,
            host_irq,
            InterruptTriggerMode::EdgeTriggered,
            false,
            host_console_irq,
        )?;
        Ok(host_irq)
    }

    fn guest_gsi_for_host_irq(&self, host_irq: irq::IrqId) -> Option<usize> {
        let state = self.forwarding.lock();
        if let Some((gsi, _)) = state
            .routes
            .iter()
            .enumerate()
            .filter(|(_, route)| route.explicit)
            .find(|(_, route)| route.host_irq == Some(host_irq))
        {
            return Some(gsi);
        }
        state
            .routes
            .iter()
            .position(|route| route.host_irq == Some(host_irq))
    }

    fn is_forwarded_host_gsi_level_triggered(&self, gsi: usize) -> bool {
        self.forwarding
            .lock()
            .routes
            .get(gsi)
            .is_some_and(|route| route.level_triggered)
    }

    fn forwarded_host_irq_for_registered_gsi(&self, gsi: usize) -> Option<irq::IrqId> {
        self.forwarding
            .lock()
            .routes
            .get(gsi)
            .and_then(|route| route.host_irq)
    }

    fn take_pending_forwarded_gsis_for(&self, vcpu_id: usize) -> Option<(usize, usize)> {
        let mut state = self.forwarding.lock();
        if !state.hooks_registered || state.owner_vcpu_id != Some(vcpu_id) {
            return None;
        }
        let pending = std::mem::take(&mut state.pending);
        if pending == 0 {
            return None;
        }
        let pending_level = state.pending_level & pending;
        state.pending_level &= !pending;
        Some((pending, pending_level))
    }

    fn retry_pending_forwarded_gsis(&self, pending: usize, pending_level: usize) {
        let mut state = self.forwarding.lock();
        state.pending |= pending;
        state.pending_level |= pending_level;
    }

    fn mark_forwarded_gsi_pending(&self, gsi: usize, level_triggered: bool) {
        let bit = gsi_bit(gsi);
        let mut state = self.forwarding.lock();
        if !state.enabled || state.owner_vcpu_id.is_none() {
            return;
        }
        if level_triggered {
            state.pending_level |= bit;
        }
        state.pending |= bit;
        drop(state);
        let _ = self.wired.kick.publish_from_irq(0);
    }

    fn set_forwarded_gsi_masked(&self, gsi: usize) -> bool {
        let bit = gsi_bit(gsi);
        let mut state = self.forwarding.lock();
        if !state.enabled {
            return false;
        }
        if state.masked & bit != 0 {
            return true;
        }
        state.masked |= bit;
        true
    }

    fn clear_forwarded_gsi_masked(&self, gsi: usize) {
        self.forwarding.lock().masked &= !gsi_bit(gsi);
    }

    fn forwarding_is_enabled(&self) -> bool {
        self.forwarding.lock().enabled
    }

    fn clear_forwarded_gsi_state(&self, gsi: usize) -> bool {
        if gsi >= IOAPIC_GSI_COUNT {
            return false;
        }
        let bit = gsi_bit(gsi);
        let mut state = self.forwarding.lock();
        state.pending &= !bit;
        state.pending_level &= !bit;
        let was_masked = state.masked & bit != 0;
        state.masked &= !bit;
        was_masked
    }

    fn forwarded_gsi_state(&self, gsi: usize) -> (bool, bool, bool) {
        if gsi >= IOAPIC_GSI_COUNT {
            return (false, false, false);
        }
        let bit = gsi_bit(gsi);
        let state = self.forwarding.lock();
        (
            state.pending & bit != 0,
            state.pending_level & bit != 0,
            state.masked & bit != 0,
        )
    }

    #[cfg(test)]
    fn mark_forwarded_gsi_state_for_test(&self, gsi: usize) {
        if !should_register_ioapic_gsi_hook(gsi) {
            return;
        }
        let bit = gsi_bit(gsi);
        let mut state = self.forwarding.lock();
        state.pending |= bit;
        state.pending_level |= bit;
        state.masked |= bit;
    }

    #[cfg(test)]
    fn activate_ready_forwarding_route_for_test(&self, guest_gsi: usize, route_ready: bool) {
        if route_ready {
            self.try_activate_forwarding_route(guest_gsi);
        }
    }

    fn try_activate_forwarding_route(&self, guest_gsi: usize) {
        if !should_register_ioapic_gsi_hook(guest_gsi) {
            return;
        }
        let activator = {
            let mut state = self.forwarding.lock();
            let activator = state.routes[guest_gsi].activator;
            if state.routes[guest_gsi].host_irq.is_none()
                || activator.is_none()
                || state.activated & gsi_bit(guest_gsi) != 0
            {
                return;
            }
            state.activated |= gsi_bit(guest_gsi);
            activator
        };
        if let Some(activator) = activator {
            self.activate_forwarded_gsi(guest_gsi, activator);
        }
    }

    fn activate_forwarded_gsi(&self, gsi: usize, activator: IoApicForwardingActivator) {
        let was_masked = self.clear_forwarded_gsi_state(gsi);
        activator();
        if was_masked {
            self.set_forwarded_host_gsi_enabled(gsi, true);
        }
    }

    fn disable_forwarding(&self) -> Vec<irq::IrqId> {
        let mut state = self.forwarding.lock();
        state.owner_vcpu_id = None;
        state.pending = 0;
        state.pending_level = 0;
        state.hooks_registered = false;
        state.enabled = false;
        let masked = std::mem::take(&mut state.masked);
        state
            .routes
            .iter()
            .enumerate()
            .filter_map(|(gsi, route)| {
                if masked & gsi_bit(gsi) != 0 {
                    route.host_irq
                } else {
                    None
                }
            })
            .collect()
    }

    fn set_forwarded_host_gsi_enabled(&self, gsi: usize, enabled: bool) {
        if let Some(host_irq) = self.forwarded_host_irq_for_registered_gsi(gsi) {
            set_host_irq_enabled(host_irq, gsi, enabled);
        }
    }
}

pub fn start_deferred_irq_delivery(vm: &VMRef) {
    if let Some(domain) = interrupt_domain_for_vm(vm) {
        domain.start_kick_worker();
    }
}

pub fn stop_deferred_irq_delivery(vm: &VMRef) {
    if let Some(domain) = interrupt_domain_for_vm(vm) {
        domain.stop_kick_worker();
    }
}

pub fn drain_pending_wired_irqs(vm: &VMRef, vcpu: &VCpuRef) {
    if vcpu.id() != 0 {
        return;
    }
    let Some(domain) = interrupt_domain_for_vm(vm) else {
        return;
    };
    let (pending, pending_level) = domain.take_pending_wired_gsis();
    if pending == 0 {
        return;
    }

    let mut retry = 0;
    let mut retry_level = 0;
    for gsi in 0..IOAPIC_GSI_COUNT {
        let bit = gsi_bit(gsi);
        if pending & bit == 0 {
            continue;
        }
        let Some(vector) = domain.vector_for_gsi(gsi) else {
            retry |= bit;
            retry_level |= pending_level & bit;
            continue;
        };
        let trigger = if pending_level & bit != 0 {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        };
        if let Err(error) = vcpu
            .get_arch_vcpu()
            .inject_interrupt_with_trigger(vector as usize, trigger)
        {
            warn!("failed to inject x86 controller-owned GSI {gsi} vector {vector:#x}: {error:?}");
            retry |= bit;
            retry_level |= pending_level & bit;
        }
    }
    if retry != 0 {
        domain
            .wired
            .pending
            .fetch_or(retry, std::sync::atomic::Ordering::Release);
        domain
            .wired
            .pending_level
            .fetch_or(retry_level, std::sync::atomic::Ordering::Release);
    }
}

pub fn register_ioapic_irq_forwarding_route(
    vm: &crate::AxVMRef,
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
) -> crate::AxVmResult {
    register_ioapic_irq_forwarding_route_with_trigger(
        vm,
        guest_gsi,
        host_irq,
        InterruptTriggerMode::EdgeTriggered,
    )
}

pub fn register_ioapic_irq_forwarding_route_with_trigger(
    vm: &crate::AxVMRef,
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
    trigger: InterruptTriggerMode,
) -> crate::AxVmResult {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return Err(crate::AxVmError::InvalidInput {
            operation: "register x86 IOAPIC forwarding route",
            detail: std::format!("unsupported guest GSI {guest_gsi}"),
        });
    }

    let domain = require_interrupt_domain(vm, "register x86 IOAPIC forwarding route")?;
    let host_console_irq = crate::host::arceos::host_console_irq();
    domain
        .register_forwarding_route(guest_gsi, host_irq, trigger, true, host_console_irq)
        .map_err(|error| forwarding_route_error(vm.id(), guest_gsi, host_irq, error))?;
    info!(
        "Registered x86 IOAPIC forwarding route: guest GSI {guest_gsi} <- host IRQ {host_irq:?}, \
         trigger {trigger:?}"
    );
    Ok(())
}

pub fn register_ioapic_irq_forwarding_activator(
    vm: &crate::AxVMRef,
    guest_gsi: usize,
    activator: IoApicForwardingActivator,
) -> crate::AxVmResult {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return Err(crate::AxVmError::InvalidInput {
            operation: "register x86 IOAPIC forwarding activator",
            detail: std::format!("unsupported guest GSI {guest_gsi}"),
        });
    }

    let domain = require_interrupt_domain(vm, "register x86 IOAPIC forwarding activator")?;
    domain
        .register_forwarding_activator(guest_gsi, activator)
        .map_err(|error| forwarding_route_error(vm.id(), guest_gsi, "activator", error))
}

pub fn inject_due_pit_irq0(vm: &VMRef, vcpu: &VCpuRef) {
    if !vm.uses_passthrough_address_space() {
        return;
    }

    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    if !devices
        .services()
        .require::<X86PitServiceKey>()
        .is_ok_and(|pit| pit.consume_irq0_if_due(now_ns))
    {
        return;
    }
    if let Some(vector) = devices
        .services()
        .require::<X86PicServiceKey>()
        .ok()
        .and_then(|pic| pic.pulse_irq(PIT_TIMER_GSI as u8))
    {
        vcpu.get_arch_vcpu()
            .inject_interrupt_with_trigger(vector as _, InterruptTriggerMode::EdgeTriggered)
            .unwrap();
        return;
    }

    let Some(irq) = devices
        .services()
        .require::<X86InterruptDomainKey>()
        .ok()
        .and_then(|ioapic| ioapic.assert_gsi(PIT_TIMER_GSI))
    else {
        trace!("x86 PIT IRQ0 due but vIOAPIC GSI0 is not ready");
        return;
    };

    vcpu.get_arch_vcpu()
        .inject_interrupt_with_trigger(
            irq.vector as _,
            if irq.level_triggered {
                InterruptTriggerMode::LevelTriggered
            } else {
                InterruptTriggerMode::EdgeTriggered
            },
        )
        .unwrap();
}

pub fn inject_pending_ioapic_irq_after_eoi(vm: &VMRef, vcpu: &VCpuRef, vector: u8) {
    if !vm.uses_passthrough_address_space() {
        return;
    }

    let Ok(devices) = vm.get_devices() else {
        return;
    };
    let Some(eoi) = devices
        .services()
        .require::<X86InterruptDomainKey>()
        .ok()
        .and_then(|ioapic| ioapic.end_of_interrupt(vector))
    else {
        return;
    };
    let pending = eoi.pending;
    if should_rearm_forwarded_host_gsi_after_eoi(pending)
        && let Some(domain) = interrupt_domain_for_vm(vm)
    {
        unmask_forwarded_host_gsi(&domain, eoi.gsi);
    }

    let Some(irq) = pending else {
        return;
    };

    trace!(
        "Injecting pending x86 IOAPIC level IRQ vector {:#x} after EOI {vector:#x}",
        irq.vector
    );
    vcpu.get_arch_vcpu()
        .inject_interrupt_with_trigger(
            irq.vector as _,
            if irq.level_triggered {
                InterruptTriggerMode::LevelTriggered
            } else {
                InterruptTriggerMode::EdgeTriggered
            },
        )
        .unwrap();
}

fn should_rearm_forwarded_host_gsi_after_eoi(pending: Option<x86_vlapic::IoApicInterrupt>) -> bool {
    !pending.is_some_and(|irq| irq.level_triggered)
}

pub fn drain_pending_ioapic_irqs(vm: &VMRef, vcpu: &VCpuRef) {
    let Some(domain) = interrupt_domain_for_vm(vm) else {
        return;
    };
    let Some((pending, pending_level)) = domain.take_pending_forwarded_gsis_for(vcpu.id()) else {
        return;
    };

    let mut retry_pending = 0;
    let mut retry_level_pending = 0;
    for gsi in 0..IOAPIC_GSI_COUNT {
        let bit = 1usize << gsi;
        if pending & bit != 0 {
            let level_triggered = pending_level & bit != 0;
            if forward_passthrough_gsi(vm, vcpu, gsi, level_triggered) {
                if !level_triggered {
                    unmask_forwarded_host_gsi(&domain, gsi);
                }
            } else {
                retry_pending |= bit;
                retry_level_pending |= pending_level & bit;
            }
        }
    }

    if retry_pending != 0 {
        domain.retry_pending_forwarded_gsis(retry_pending, retry_level_pending);
    }
}

pub fn enable_ioapic_irq_forwarding(vm: &VMRef, vcpu: &VCpuRef) {
    if !vm.uses_passthrough_address_space() {
        return;
    }

    // Host IRQ forwarding is intentionally drained by the boot vCPU.  Choosing
    // a fixed owner avoids changing the injection target with vCPU start order.
    if vcpu.id() != 0 {
        return;
    }

    let Some(domain) = interrupt_domain_for_vm(vm) else {
        return;
    };
    if !domain.set_forwarding_owner(vcpu.id()) {
        return;
    }

    let mut registered = 0;
    let host_console_irq = crate::host::arceos::host_console_irq();
    for gsi in ioapic_irq_hook_gsis() {
        match domain.forwarded_host_irq_for_guest_gsi(gsi, host_console_irq) {
            Ok(host_irq) => {
                if !acquire_host_irq_forwarding_lease(host_irq, vm.id()) {
                    warn!(
                        "skip x86 IOAPIC forwarding route for VM[{}] guest GSI {gsi}: host IRQ \
                         {host_irq:?} is already leased",
                        vm.id()
                    );
                    continue;
                }

                let handler_domain = domain.clone();
                match irq::request_shared_irq(host_irq, move |ctx| {
                    ioapic_irq_forwarding_handler(&handler_domain, ctx)
                }) {
                    Ok(handle) => {
                        domain.add_forwarding_hook(handle);
                        registered += 1;
                    }
                    Err(err) => {
                        release_host_irq_forwarding_lease(host_irq, vm.id());
                        warn!(
                            "failed to request x86 IOAPIC forwarding IRQ action for host GSI \
                             {gsi}: {err:?}"
                        );
                    }
                }
            }
            Err(err) => {
                trace!("skip x86 IOAPIC forwarding hook for guest GSI {gsi}: {err:?}");
            }
        }
    }
    if registered != 0 {
        domain.mark_forwarding_hooks_registered();
    }
    info!(
        "Enabled x86 IOAPIC IRQ forwarding for guest-assignable host GSIs 0..{} excluding \
         machine-owned resources ({} newly registered)",
        IOAPIC_GSI_COUNT - 1,
        registered
    );
    activate_ready_ioapic_forwarding_routes(vm);
}

pub fn activate_ready_ioapic_forwarding_routes(vm: &VMRef) {
    if !vm.uses_passthrough_address_space() {
        return;
    }
    let Some(domain) = interrupt_domain_for_vm(vm) else {
        return;
    };

    for gsi in ioapic_irq_hook_gsis() {
        let ioapic_route_ready = matches!(
            vm.get_devices()
                .ok()
                .and_then(|devices| devices.services().require::<X86InterruptDomainKey>().ok()),
            Some(ioapic) if ioapic.vector_for_gsi(gsi).is_some()
        );
        if !ioapic_route_ready {
            continue;
        }
        domain.try_activate_forwarding_route(gsi);
    }
}

pub fn disable_ioapic_irq_forwarding_for_vm(vm: &VMRef) {
    let Some(domain) = interrupt_domain_for_vm(vm) else {
        return;
    };
    for host_irq in domain.disable_forwarding() {
        set_host_irq_enabled(host_irq, usize::MAX, true);
    }
    release_forwarding_hooks(&domain);
    release_host_irq_forwarding_leases_for_vm(vm.id());
}

fn release_forwarding_hooks(domain: &X86InterruptDomain) {
    let hooks = domain.take_forwarding_hooks();
    for handle in hooks {
        if let Err(error) = irq::free_shared_irq(handle) {
            warn!("failed to free x86 IOAPIC forwarding IRQ action {handle:?}: {error:?}");
        }
    }
}

fn forward_passthrough_gsi(
    vm: &VMRef,
    vcpu: &VCpuRef,
    guest_gsi: usize,
    host_level_triggered: bool,
) -> bool {
    if !vm.uses_passthrough_address_space() {
        return true;
    }

    if guest_gsi >= IOAPIC_GSI_COUNT {
        return true;
    }

    let Ok(devices) = vm.get_devices() else {
        return false;
    };
    let ioapic = devices.services().require::<X86InterruptDomainKey>().ok();
    let Some(guest_irq) = ioapic
        .as_ref()
        .and_then(|ioapic| ioapic.assert_gsi(guest_gsi))
    else {
        if ioapic
            .as_ref()
            .is_some_and(|ioapic| ioapic.vector_for_gsi(guest_gsi).is_some())
        {
            trace!(
                "x86 passthrough IRQ for guest GSI {guest_gsi} is deferred by guest vIOAPIC state"
            );
            if !host_level_triggered && let Some(domain) = interrupt_domain_for_vm(vm) {
                unmask_forwarded_host_gsi(&domain, guest_gsi);
            }
            return true;
        }

        trace!("x86 passthrough IRQ has no injectable guest vIOAPIC route for GSI {guest_gsi}");
        return false;
    };

    vcpu.get_arch_vcpu()
        .inject_interrupt_with_trigger(
            guest_irq.vector as _,
            if guest_irq.level_triggered {
                InterruptTriggerMode::LevelTriggered
            } else {
                InterruptTriggerMode::EdgeTriggered
            },
        )
        .unwrap();
    true
}

fn gsi_bit(gsi: usize) -> usize {
    1usize << gsi
}

#[cfg(test)]
fn host_irq_to_raw(irq: irq::IrqId) -> usize {
    (usize::from(irq.domain.0) << 32) | irq.hwirq.0 as usize
}

#[cfg(test)]
fn raw_to_host_irq(raw: usize) -> irq::IrqId {
    irq::make_irq_id((raw >> 32) as u16, raw as u32)
}

fn set_host_irq_enabled(host_irq: irq::IrqId, gsi: usize, enabled: bool) {
    if let Err(err) = irq::set_host_irq_enable(host_irq, enabled) {
        warn!(
            "failed to set forwarded IOAPIC GSI {gsi} host IRQ {host_irq:?} enabled={enabled}: \
             {err:?}"
        );
    }
}

fn mask_forwarded_host_gsi(domain: &X86InterruptDomain, gsi: usize) -> bool {
    if !domain.set_forwarded_gsi_masked(gsi) {
        return false;
    }
    let Some(host_irq) = domain.forwarded_host_irq_for_registered_gsi(gsi) else {
        domain.clear_forwarded_gsi_masked(gsi);
        return false;
    };

    if let Err(err) = irq::set_host_irq_enable(host_irq, false) {
        domain.clear_forwarded_gsi_masked(gsi);
        warn!("failed to mask forwarded IOAPIC GSI {gsi} host IRQ {host_irq:?}: {err:?}");
        return false;
    }
    // Teardown may have started after the state transition above but before
    // the physical line was masked. Restore the line in that case instead of
    // leaving an IRQ disabled after its forwarding hook is removed.
    if !domain.forwarding_is_enabled() {
        set_host_irq_enabled(host_irq, gsi, true);
        domain.clear_forwarded_gsi_masked(gsi);
        return false;
    }
    true
}

fn unmask_forwarded_host_gsi(domain: &X86InterruptDomain, gsi: usize) {
    if gsi >= IOAPIC_GSI_COUNT {
        return;
    }
    if !domain.forwarded_gsi_state(gsi).2 {
        return;
    }

    let Some(host_irq) = domain.forwarded_host_irq_for_registered_gsi(gsi) else {
        return;
    };

    if let Err(err) = irq::set_host_irq_enable(host_irq, true) {
        warn!("failed to unmask forwarded IOAPIC GSI {gsi} host IRQ {host_irq:?}: {err:?}");
        return;
    }
    domain.clear_forwarded_gsi_masked(gsi);
}

fn ioapic_irq_forwarding_handler(
    domain: &X86InterruptDomain,
    ctx: irq::IrqContext,
) -> irq::IrqReturn {
    let Some(gsi) = domain.guest_gsi_for_host_irq(ctx.irq) else {
        return irq::IrqReturn::Unhandled;
    };

    if !mask_forwarded_host_gsi(domain, gsi) {
        return irq::IrqReturn::Unhandled;
    }
    let level_triggered = domain.is_forwarded_host_gsi_level_triggered(gsi);
    domain.mark_forwarded_gsi_pending(gsi, level_triggered);
    irq::IrqReturn::Handled
}

fn acquire_host_irq_forwarding_lease(host_irq: irq::IrqId, vm_id: usize) -> bool {
    let mut leases = HOST_IRQ_FORWARDING_LEASES.lock();
    if leases
        .iter()
        .any(|lease| lease.host_irq == host_irq && lease.vm_id != vm_id)
    {
        return false;
    }
    if !leases
        .iter()
        .any(|lease| lease.host_irq == host_irq && lease.vm_id == vm_id)
    {
        leases.push(HostIrqLease { host_irq, vm_id });
    }
    true
}

fn release_host_irq_forwarding_lease(host_irq: irq::IrqId, vm_id: usize) {
    HOST_IRQ_FORWARDING_LEASES
        .lock()
        .retain(|lease| !(lease.host_irq == host_irq && lease.vm_id == vm_id));
}

fn release_host_irq_forwarding_leases_for_vm(vm_id: usize) {
    HOST_IRQ_FORWARDING_LEASES
        .lock()
        .retain(|lease| lease.vm_id != vm_id);
}

#[cfg(test)]
fn reset_host_irq_forwarding_leases() {
    HOST_IRQ_FORWARDING_LEASES.lock().clear();
}

#[cfg(test)]
fn host_irq_forwarding_lease_count() -> usize {
    HOST_IRQ_FORWARDING_LEASES.lock().len()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ax_std::os::arceos::sync::RawSpinLock as Mutex;
    use axdevice::X86IoApicDeviceOps;

    use super::{
        COM1_GSI, IOAPIC_GSI_COUNT, PIT_TIMER_GSI, acquire_host_irq_forwarding_lease, gsi_bit,
        host_irq_forwarding_lease_count, host_irq_is_guest_assignable, host_irq_to_raw,
        ioapic_irq_hook_gsis, raw_to_host_irq, release_host_irq_forwarding_leases_for_vm,
        reset_host_irq_forwarding_leases, should_rearm_forwarded_host_gsi_after_eoi,
        should_register_ioapic_gsi_hook,
    };
    use crate::{InterruptTriggerMode, arch::x86_64::X86InterruptDomain};

    static ROUTE_TEST_LOCK: Mutex<()> = Mutex::new(());
    static ACTIVATION_COUNT: AtomicUsize = AtomicUsize::new(0);

    struct FakeIoApic;

    impl X86IoApicDeviceOps for FakeIoApic {
        fn vector_for_gsi(&self, _gsi: usize) -> Option<u8> {
            Some(0x40)
        }

        fn assert_gsi(&self, _gsi: usize) -> Option<x86_vlapic::IoApicInterrupt> {
            None
        }

        fn set_gsi_level(
            &self,
            _gsi: usize,
            _asserted: bool,
        ) -> Option<x86_vlapic::IoApicInterrupt> {
            None
        }

        fn end_of_interrupt(&self, _vector: u8) -> Option<x86_vlapic::IoApicEoi> {
            None
        }
    }

    fn new_domain() -> X86InterruptDomain {
        X86InterruptDomain::new(1, Arc::new(FakeIoApic))
    }

    fn reset_forwarding_routes() {
        crate::arch::x86_64::host_irq::reset_test_irq_enable_state();
        reset_host_irq_forwarding_leases();
    }

    fn with_clean_forwarding_routes(test: impl FnOnce()) {
        let _guard = ROUTE_TEST_LOCK.lock();
        reset_forwarding_routes();
        test();
    }

    #[test]
    fn pit_gsi_uses_synthetic_injection_not_host_irq_hook() {
        assert!(!should_register_ioapic_gsi_hook(PIT_TIMER_GSI));
    }

    #[test]
    fn machine_owned_uart_gsi_never_registers_a_host_irq_hook() {
        assert!(!should_register_ioapic_gsi_hook(COM1_GSI));
    }

    #[test]
    fn runtime_selected_host_console_irq_is_not_guest_assignable() {
        let console_irq = crate::arch::x86_64::host_irq::make_irq_id(7, 19);
        let unrelated_irq = crate::arch::x86_64::host_irq::make_irq_id(7, 20);

        assert!(!host_irq_is_guest_assignable(
            console_irq,
            Some(console_irq)
        ));
        assert!(host_irq_is_guest_assignable(
            unrelated_irq,
            Some(console_irq)
        ));
        assert!(host_irq_is_guest_assignable(unrelated_irq, None));

        let domain = new_domain();
        assert_eq!(
            domain.register_forwarding_route(
                19,
                console_irq,
                InterruptTriggerMode::LevelTriggered,
                true,
                Some(console_irq),
            ),
            Err(super::ForwardingRouteError::HostOwnedConsole)
        );
        assert_eq!(domain.guest_gsi_for_host_irq(console_irq), None);
    }

    #[test]
    fn passthrough_gsis_still_register_host_irq_hooks() {
        assert!(should_register_ioapic_gsi_hook(18));
        assert!(should_register_ioapic_gsi_hook(IOAPIC_GSI_COUNT - 1));
        assert!(!should_register_ioapic_gsi_hook(IOAPIC_GSI_COUNT));
    }

    #[test]
    fn hook_gsi_iterator_matches_registration_policy() {
        for gsi in 0..=IOAPIC_GSI_COUNT {
            assert_eq!(
                ioapic_irq_hook_gsis().any(|hook| hook == gsi),
                should_register_ioapic_gsi_hook(gsi)
            );
        }
    }

    #[test]
    fn forwarded_gsi_bits_are_stable() {
        assert_eq!(gsi_bit(0), 1);
        assert_eq!(gsi_bit(18), 1usize << 18);
    }

    #[test]
    fn host_irq_storage_preserves_domain_and_hwirq() {
        let irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);
        assert_eq!(raw_to_host_irq(host_irq_to_raw(irq)), irq);
    }

    #[test]
    fn forwarding_route_rejects_host_irq_already_mapped_to_another_gsi() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let fallback_guest_gsi = 7;
            let explicit_guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 7);
            domain
                .register_forwarding_route(
                    fallback_guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    false,
                    None,
                )
                .unwrap();

            assert_eq!(
                domain.register_forwarding_route(
                    explicit_guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                ),
                Err(super::ForwardingRouteError::HostIrqConflict)
            );

            assert_eq!(
                domain.guest_gsi_for_host_irq(host_irq),
                Some(fallback_guest_gsi)
            );
        });
    }

    #[test]
    fn explicit_route_reserves_its_host_irq_from_fallback_registration() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let fallback_guest_gsi = 10;
            let explicit_guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
            domain
                .register_forwarding_route(
                    explicit_guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                )
                .unwrap();

            assert_eq!(
                domain.register_forwarding_route(
                    fallback_guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    false,
                    None,
                ),
                Err(super::ForwardingRouteError::HostIrqConflict)
            );
        });
    }

    #[test]
    fn forwarding_trigger_mode_comes_from_registered_route_not_gsi_number() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let low_level_gsi = COM1_GSI;
            let high_edge_gsi = 18;
            let low_host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, low_level_gsi as u32);
            let high_host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, high_edge_gsi as u32);

            domain
                .register_forwarding_route(
                    low_level_gsi,
                    low_host_irq,
                    InterruptTriggerMode::LevelTriggered,
                    true,
                    None,
                )
                .unwrap();
            domain
                .register_forwarding_route(
                    high_edge_gsi,
                    high_host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                )
                .unwrap();

            assert!(domain.is_forwarded_host_gsi_level_triggered(low_level_gsi));
            assert!(!domain.is_forwarded_host_gsi_level_triggered(high_edge_gsi));
        });
    }

    fn count_activation() {
        ACTIVATION_COUNT.fetch_add(1, Ordering::AcqRel);
    }

    #[test]
    fn forwarding_activator_waits_for_guest_route_and_runs_once() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);
            ACTIVATION_COUNT.store(0, Ordering::Release);
            domain
                .register_forwarding_route(
                    guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                )
                .unwrap();
            domain
                .register_forwarding_activator(guest_gsi, count_activation)
                .unwrap();

            domain.activate_ready_forwarding_route_for_test(guest_gsi, false);
            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 0);

            domain.activate_ready_forwarding_route_for_test(guest_gsi, true);
            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);

            domain.activate_ready_forwarding_route_for_test(guest_gsi, true);
            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
        });
    }

    #[test]
    fn forwarding_activator_does_not_start_without_an_owned_host_irq_route() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let guest_gsi = 18;
            ACTIVATION_COUNT.store(0, Ordering::Release);
            domain
                .register_forwarding_activator(guest_gsi, count_activation)
                .unwrap();

            domain.activate_ready_forwarding_route_for_test(guest_gsi, true);

            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 0);
        });
    }

    #[test]
    fn forwarding_activator_drops_pre_activation_pending_state() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 10);
            ACTIVATION_COUNT.store(0, Ordering::Release);
            domain
                .register_forwarding_route(
                    guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                )
                .unwrap();
            domain
                .register_forwarding_activator(guest_gsi, count_activation)
                .unwrap();
            domain.mark_forwarded_gsi_state_for_test(guest_gsi);

            domain.activate_ready_forwarding_route_for_test(guest_gsi, true);

            assert_eq!(ACTIVATION_COUNT.load(Ordering::Acquire), 1);
            assert_eq!(domain.forwarded_gsi_state(guest_gsi), (false, false, false));
            assert!(crate::arch::x86_64::host_irq::test_irq_is_enabled(host_irq));
        });
    }

    #[test]
    fn clearing_forwarded_gsi_state_reports_masked_host_line() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let guest_gsi = 18;
            domain.mark_forwarded_gsi_state_for_test(guest_gsi);

            assert!(domain.clear_forwarded_gsi_state(guest_gsi));
            assert_eq!(domain.forwarded_gsi_state(guest_gsi), (false, false, false));
            assert!(!domain.clear_forwarded_gsi_state(guest_gsi));
        });
    }

    #[test]
    fn forwarded_level_intx_stays_masked_when_guest_eoi_has_deferred_pending() {
        let pending = x86_vlapic::IoApicInterrupt {
            vector: 0x51,
            level_triggered: true,
        };

        assert!(!should_rearm_forwarded_host_gsi_after_eoi(Some(pending)));
    }

    #[test]
    fn forwarded_intx_rearms_host_line_when_guest_eoi_has_no_deferred_level() {
        let pending = x86_vlapic::IoApicInterrupt {
            vector: 0x51,
            level_triggered: false,
        };

        assert!(should_rearm_forwarded_host_gsi_after_eoi(None));
        assert!(should_rearm_forwarded_host_gsi_after_eoi(Some(pending)));
    }

    #[test]
    fn forwarding_teardown_unmasks_lines_and_discards_pending_work_for_its_vm() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);

            domain
                .register_forwarding_route(
                    guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                )
                .unwrap();
            assert!(domain.set_forwarding_owner(1));
            domain.mark_forwarding_hooks_registered();
            domain.mark_forwarded_gsi_state_for_test(guest_gsi);
            crate::arch::x86_64::host_irq::set_host_irq_enable(host_irq, false).unwrap();

            let masked = domain.disable_forwarding();
            for irq in masked {
                crate::arch::x86_64::host_irq::set_host_irq_enable(irq, true).unwrap();
            }

            assert_eq!(domain.forwarded_gsi_state(guest_gsi), (false, false, false));
            assert!(!domain.has_registered_forwarding_hooks_for(1));
            assert!(crate::arch::x86_64::host_irq::test_irq_is_enabled(host_irq));
        });
    }

    #[test]
    fn forwarding_teardown_refuses_to_mask_a_host_irq_after_disable() {
        with_clean_forwarding_routes(|| {
            let domain = new_domain();
            let guest_gsi = 18;
            let host_irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);
            domain
                .register_forwarding_route(
                    guest_gsi,
                    host_irq,
                    InterruptTriggerMode::EdgeTriggered,
                    true,
                    None,
                )
                .unwrap();
            assert!(domain.set_forwarding_owner(0));
            crate::arch::x86_64::host_irq::set_host_irq_enable(host_irq, true).unwrap();

            let _ = domain.disable_forwarding();

            assert!(!super::mask_forwarded_host_gsi(&domain, guest_gsi));
            assert!(crate::arch::x86_64::host_irq::test_irq_is_enabled(host_irq));
            assert_eq!(domain.forwarded_gsi_state(guest_gsi), (false, false, false));
        });
    }

    #[test]
    fn host_irq_leases_reject_cross_vm_ownership_and_release_by_vm() {
        with_clean_forwarding_routes(|| {
            let irq = crate::arch::x86_64::host_irq::make_irq_id(2, 18);

            assert!(acquire_host_irq_forwarding_lease(irq, 7));
            assert!(acquire_host_irq_forwarding_lease(irq, 7));
            assert!(!acquire_host_irq_forwarding_lease(irq, 8));
            assert_eq!(host_irq_forwarding_lease_count(), 1);

            release_host_irq_forwarding_leases_for_vm(7);

            assert_eq!(host_irq_forwarding_lease_count(), 0);
            assert!(acquire_host_irq_forwarding_lease(irq, 8));
        });
    }
}
