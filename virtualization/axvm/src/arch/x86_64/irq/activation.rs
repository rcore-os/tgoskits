//! Host IRQ action setup and transactional guest-route activation.

use core::sync::atomic::Ordering;

use ax_std::os::arceos::task::{ThreadWakeHandle, current_thread_handle};

use super::{handler::capture_forwarded_ioapic_irq, state::*};
use crate::{
    AxVmError, AxVmResult,
    config::VMInterruptMode,
    runtime::{VCpuRef, VMRef},
};

/// Registers the reversible device-endpoint operations for one guest GSI.
///
/// # Errors
///
/// Returns an error for an unsupported GSI or when an existing activation or
/// quarantine still owns the route.
pub fn register_ioapic_irq_forwarding_activation(
    guest_gsi: usize,
    operations: IoApicForwardingActivationOps,
) -> AxVmResult {
    if !should_register_ioapic_gsi_hook(guest_gsi) {
        return Err(AxVmError::invalid_input(
            "register x86 IOAPIC forwarding activation",
            format_args!("unsupported guest GSI {guest_gsi}"),
        ));
    }

    let mut route = IOAPIC_FORWARDING_ROUTES[guest_gsi].lock();
    match *route {
        IoApicForwardingRouteState::Vacant | IoApicForwardingRouteState::Prepared(_) => {
            *route = IoApicForwardingRouteState::Prepared(operations);
            Ok(())
        }
        IoApicForwardingRouteState::Activating
        | IoApicForwardingRouteState::Active(_)
        | IoApicForwardingRouteState::Quarantined(_) => Err(AxVmError::invalid_state(
            "register x86 IOAPIC forwarding activation",
            format_args!("guest GSI {guest_gsi} route is activating, active, or quarantined"),
        )),
    }
}

/// Validates one required forwarding source before its owner vCPU starts.
///
/// This operation belongs to the host-to-guest ownership transaction. The
/// selected device endpoint must already be masked and the previous host
/// action must already be unregistered. It deliberately does not install a
/// temporary callback: only the fixed vCPU maintenance thread may register or
/// release the live action.
///
/// # Errors
///
/// Returns an error when the route is incomplete, aliases another explicit
/// guest route, or another route transaction is in progress. The definitive
/// descriptor ownership check happens when the vCPU owner registers its
/// exclusive fixed-affinity action.
pub fn validate_ioapic_irq_forwarding_source(guest_gsi: usize) -> AxVmResult {
    if !should_register_ioapic_gsi_hook(guest_gsi)
        || !ioapic_forwarding_route_requires_host_irq(guest_gsi)
    {
        return Err(AxVmError::invalid_input(
            "validate x86 IOAPIC forwarding source",
            format_args!("guest GSI {guest_gsi} has no required forwarding route"),
        ));
    }

    let _transaction = acquire_ioapic_route_activation_transaction()?;
    let host_irq = forwarded_host_irq_for_guest_gsi(guest_gsi).map_err(|error| {
        forwarding_irq_error(
            "resolve validated x86 IOAPIC forwarding source",
            guest_gsi,
            error,
        )
    })?;
    if IOAPIC_IRQ_HANDLES[guest_gsi].lock().is_some() {
        return Err(AxVmError::resource_conflict(
            "validate x86 IOAPIC forwarding source",
            format_args!("guest GSI {guest_gsi} already has a live owner action"),
        ));
    }
    if host_irq_has_explicit_route_for_other_gsi(host_irq, guest_gsi) {
        return Err(AxVmError::resource_conflict(
            "validate x86 IOAPIC forwarding source",
            format_args!(
                "guest GSI {guest_gsi} aliases host IRQ {host_irq:?} already assigned to another \
                 guest route"
            ),
        ));
    }

    Ok(())
}

pub fn enable_ioapic_irq_forwarding(vm: &VMRef, vcpu: &VCpuRef) -> AxVmResult {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return Ok(());
    }
    if !ioapic_irq_hook_gsis().any(ioapic_forwarding_route_requires_host_irq) {
        return Ok(());
    }

    // The emulated PC interrupt fabric targets the BSP. Secondary vCPU startup
    // must not replace that owner or race the one-time host action setup.
    if vcpu.id() != 0 {
        return Ok(());
    }
    if vm.vcpu_num() != 1 {
        return Err(AxVmError::invalid_config(format_args!(
            "x86 passthrough IRQ ownership requires exactly one vCPU, got {}",
            vm.vcpu_num()
        )));
    }

    ensure_no_quarantined_ioapic_routes()?;
    let owner = prepare_ioapic_forwarding_owner(vcpu)?;

    let publication = IoApicForwardingEnablePublication::capture();
    if publication.owner_vm != usize::MAX && publication.owner_vm != vm.id() {
        return Err(AxVmError::resource_conflict(
            "x86 IOAPIC forwarding owner",
            format_args!(
                "VM[{}] still owns the process-global forwarding fabric",
                publication.owner_vm
            ),
        ));
    }

    let transaction = acquire_ioapic_route_activation_transaction()?;
    if IOAPIC_IRQ_FORWARDING_ENABLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        ensure_ioapic_forwarding_owner(&owner)?;
        ensure_required_ioapic_forwarding_handles()?;
        publish_ioapic_forwarding_owner(vm.id(), vcpu.id());
        return activate_ready_ioapic_forwarding_routes_in_transaction(vm, &transaction).or_else(
            |error| {
                restore_ioapic_forwarding_enable_publication(publication)
                    .map_err(|rollback| activation_rollback_error(error.clone(), rollback))?;
                Err(error)
            },
        );
    }

    if let Err(error) = register_ioapic_forwarding_actions(&owner) {
        IOAPIC_IRQ_FORWARDING_ENABLED.store(publication.enabled, Ordering::Release);
        return Err(error);
    }

    publish_ioapic_forwarding_owner(vm.id(), vcpu.id());
    activate_ready_ioapic_forwarding_routes_in_transaction(vm, &transaction).or_else(|error| {
        restore_ioapic_forwarding_enable_publication(publication)
            .map_err(|rollback| activation_rollback_error(error.clone(), rollback))?;
        Err(error)
    })
}

struct IoApicForwardingOwner {
    thread_id: u64,
    cpu: usize,
    wake: ThreadWakeHandle,
}

fn prepare_ioapic_forwarding_owner(vcpu: &VCpuRef) -> AxVmResult<IoApicForwardingOwner> {
    let thread = current_thread_handle().map_err(|error| {
        AxVmError::resource_unavailable("x86 IOAPIC forwarding owner thread", error)
    })?;
    let wake = thread.wake_handle();
    let thread_id = wake.thread_id().as_u64();
    let cpu = wake
        .target_cpu()
        .map(|cpu| cpu.as_u32() as usize)
        .ok_or_else(|| {
            AxVmError::resource_unavailable(
                "x86 IOAPIC forwarding owner CPU",
                "the current vCPU thread has no scheduler target",
            )
        })?;
    drop(thread);

    let owner_mask = 1usize.checked_shl(cpu as u32).ok_or_else(|| {
        AxVmError::invalid_config(format_args!(
            "x86 IOAPIC forwarding owner CPU {cpu} exceeds the host CPU mask"
        ))
    })?;
    if vcpu.phys_cpu_set() != Some(owner_mask) {
        return Err(AxVmError::invalid_config(format_args!(
            "x86 passthrough VM[{}] VCpu[{}] must remain fixed to current host CPU {cpu}",
            vcpu.vm_id(),
            vcpu.id()
        )));
    }

    Ok(IoApicForwardingOwner {
        thread_id,
        cpu,
        wake,
    })
}

fn ensure_ioapic_forwarding_owner(owner: &IoApicForwardingOwner) -> AxVmResult {
    let installed_thread = IOAPIC_IRQ_OWNER_THREAD_ID.load(Ordering::Acquire);
    let installed_cpu = IOAPIC_IRQ_OWNER_CPU.load(Ordering::Acquire);
    if installed_thread == owner.thread_id && installed_cpu == owner.cpu {
        return Ok(());
    }
    Err(AxVmError::resource_conflict(
        "x86 IOAPIC forwarding owner",
        format_args!(
            "thread {installed_thread:#x} on CPU {installed_cpu} owns the forwarding actions, not \
             thread {:#x} on CPU {}",
            owner.thread_id, owner.cpu
        ),
    ))
}

fn register_ioapic_forwarding_actions(owner: &IoApicForwardingOwner) -> AxVmResult {
    let installed_thread = IOAPIC_IRQ_OWNER_THREAD_ID.load(Ordering::Acquire);
    let installed_cpu = IOAPIC_IRQ_OWNER_CPU.load(Ordering::Acquire);
    if installed_thread != u64::MAX || installed_cpu != usize::MAX {
        ensure_ioapic_forwarding_owner(owner)?;
    } else {
        IOAPIC_IRQ_OWNER_CPU.store(owner.cpu, Ordering::Release);
        IOAPIC_IRQ_OWNER_THREAD_ID.store(owner.thread_id, Ordering::Release);
    }

    let mut registered = 0;
    for gsi in ioapic_irq_hook_gsis() {
        let bit = gsi_bit(gsi);
        if IOAPIC_IRQ_OWNER_BOUND.load(Ordering::Acquire) & bit != 0 {
            continue;
        }

        if !ioapic_forwarding_route_requires_host_irq(gsi) {
            continue;
        }
        let host_irq = forwarded_host_irq_for_guest_gsi(gsi).map_err(|error| {
            forwarding_irq_error("resolve required x86 IOAPIC forwarding IRQ", gsi, error)
        })?;
        if host_irq_has_explicit_route_for_other_gsi(host_irq, gsi) {
            return Err(AxVmError::resource_conflict(
                "register x86 IOAPIC forwarding IRQ action",
                format_args!(
                    "guest GSI {gsi} aliases host IRQ {host_irq:?} already assigned to another \
                     guest route"
                ),
            ));
        }

        if IOAPIC_IRQ_HANDLES[gsi].lock().is_some() {
            return Err(AxVmError::resource_conflict(
                "register x86 IOAPIC forwarding IRQ action",
                format_args!("guest GSI {gsi} already retains an action"),
            ));
        }
        let wake = owner.wake.clone();
        let owner_cpu = owner.cpu;
        let handler =
            move |context| capture_forwarded_ioapic_irq(gsi, host_irq, owner_cpu, &wake, context);
        match crate::arch::x86_64::host_irq::request_exclusive_irq_disabled(
            host_irq,
            crate::arch::x86_64::host_irq::IrqAffinity::Fixed(irq_framework::CpuId(owner.cpu)),
            handler,
        ) {
            Ok(handle) => {
                *IOAPIC_IRQ_HANDLES[gsi].lock() = Some(handle);
                IOAPIC_IRQ_OWNER_BOUND.fetch_or(bit, Ordering::AcqRel);
                registered += 1;
            }
            Err(error) => {
                return Err(forwarding_irq_error(
                    "request required x86 IOAPIC forwarding IRQ action",
                    gsi,
                    error,
                ));
            }
        }
    }
    if IOAPIC_IRQ_HANDLES
        .iter()
        .any(|handle| handle.lock().is_some())
    {
        IOAPIC_IRQ_HOOK_REGISTERED.store(true, Ordering::Release);
    }
    info!("Registered {registered} required exclusive x86 IOAPIC forwarding IRQ actions");
    Ok(())
}

#[cfg(test)]
pub(super) fn register_test_ioapic_forwarding_action(guest_gsi: usize) -> AxVmResult {
    let host_irq = forwarded_host_irq_for_guest_gsi(guest_gsi).map_err(|error| {
        forwarding_irq_error("resolve test x86 IOAPIC forwarding IRQ", guest_gsi, error)
    })?;
    let handle = crate::arch::x86_64::host_irq::request_exclusive_irq_disabled(
        host_irq,
        crate::arch::x86_64::host_irq::IrqAffinity::Fixed(irq_framework::CpuId(0)),
        super::handler::reserved_ioapic_irq_action,
    )
    .map_err(|error| {
        forwarding_irq_error(
            "request test x86 IOAPIC forwarding IRQ action",
            guest_gsi,
            error,
        )
    })?;
    let mut slot = IOAPIC_IRQ_HANDLES[guest_gsi].lock();
    if slot.is_some() {
        drop(slot);
        crate::arch::x86_64::host_irq::free_irq(handle).map_err(|error| {
            forwarding_irq_error(
                "release duplicate test x86 IOAPIC forwarding IRQ action",
                guest_gsi,
                error,
            )
        })?;
        return Err(AxVmError::resource_conflict(
            "register test x86 IOAPIC forwarding IRQ action",
            format_args!("guest GSI {guest_gsi} already retains an action"),
        ));
    }
    *slot = Some(handle);
    IOAPIC_IRQ_OWNER_BOUND.fetch_or(gsi_bit(guest_gsi), Ordering::AcqRel);
    IOAPIC_IRQ_HOOK_REGISTERED.store(true, Ordering::Release);
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) struct IoApicForwardingEnablePublication {
    enabled: bool,
    owner_vm: usize,
    owner_vcpu: usize,
    activated: usize,
}

impl IoApicForwardingEnablePublication {
    pub(super) fn capture() -> Self {
        Self {
            enabled: IOAPIC_IRQ_FORWARDING_ENABLED.load(Ordering::Acquire),
            owner_vm: IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire),
            owner_vcpu: IOAPIC_IRQ_FORWARD_VCPU_ID.load(Ordering::Acquire),
            activated: IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire),
        }
    }
}

pub(super) fn publish_ioapic_forwarding_owner(vm_id: usize, vcpu_id: usize) {
    // Publish the target before the VM identity: the hard-IRQ handler treats
    // an invalid VM owner as disabled and never observes a new VM with an old
    // vCPU target.
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(vcpu_id, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VM_ID.store(vm_id, Ordering::Release);
}

pub(super) fn restore_ioapic_forwarding_enable_publication(
    publication: IoApicForwardingEnablePublication,
) -> AxVmResult {
    let activated = IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire);
    let leaked = activated & !publication.activated;
    if leaked != 0 {
        return Err(AxVmError::invalid_state(
            "restore x86 IOAPIC forwarding publication",
            format_args!("new active route mask {leaked:#x} could not be rolled back"),
        ));
    }

    // Disable publication while restoring the pair. If the previous owner was
    // valid it becomes visible only after its vCPU target has been restored.
    IOAPIC_IRQ_FORWARD_VM_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(publication.owner_vcpu, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VM_ID.store(publication.owner_vm, Ordering::Release);
    IOAPIC_IRQ_FORWARDING_ENABLED.store(publication.enabled, Ordering::Release);
    Ok(())
}

pub fn activate_ready_ioapic_forwarding_routes(vm: &VMRef) -> AxVmResult {
    if vm.interrupt_mode() != VMInterruptMode::Passthrough {
        return Ok(());
    }
    let transaction = acquire_ioapic_route_activation_transaction()?;
    activate_ready_ioapic_forwarding_routes_in_transaction(vm, &transaction)
}

fn activate_ready_ioapic_forwarding_routes_in_transaction(
    vm: &VMRef,
    _transaction: &IoApicRouteTransaction,
) -> AxVmResult {
    let owner = IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire);
    if owner != vm.id() {
        return Err(AxVmError::invalid_state(
            "activate x86 IOAPIC forwarding routes",
            format_args!(
                "VM[{}] does not own forwarding state (owner={owner})",
                vm.id()
            ),
        ));
    }

    let devices = vm.get_devices()?;
    activate_ioapic_forwarding_batch(
        ioapic_irq_hook_gsis().filter(|gsi| devices.x86_ioapic_vector_for_gsi(*gsi).is_some()),
    )
}

struct PreparedIoApicForwardingActivation {
    guest_gsi: usize,
    operations: IoApicForwardingActivationOps,
    disposition: ActivationDisposition,
}

#[derive(Clone, Copy)]
enum ActivationDisposition {
    Prepared,
    Active,
    Quarantined,
}

impl PreparedIoApicForwardingActivation {
    fn activate(mut self) -> AxVmResult {
        let gsi = self.guest_gsi;
        set_forwarding_action_enabled(gsi, false).map_err(|error| {
            forwarding_irq_error("disable x86 IOAPIC action before activation", gsi, error)
        })?;
        IOAPIC_IRQ_ACTION_DISABLED.fetch_or(gsi_bit(gsi), Ordering::AcqRel);
        clear_forwarded_ioapic_pending_state(gsi);

        if let Err(error) = self.operations.activate() {
            return Err(self.revoke_after_failed_activation(error));
        }

        // Clear the software gate before enabling the action. An IRQ that
        // arrives immediately after enable can then set the gate again without
        // this task racing it with a trailing clear.
        IOAPIC_IRQ_ACTION_DISABLED.fetch_and(!gsi_bit(gsi), Ordering::AcqRel);
        if let Err(error) = set_forwarding_action_enabled(gsi, true) {
            IOAPIC_IRQ_ACTION_DISABLED.fetch_or(gsi_bit(gsi), Ordering::AcqRel);
            let error = forwarding_irq_error("enable activated x86 IOAPIC action", gsi, error);
            return Err(self.revoke_after_failed_activation(error));
        }

        let mut route = IOAPIC_FORWARDING_ROUTES[gsi].lock();
        if !matches!(*route, IoApicForwardingRouteState::Activating) {
            return Err(AxVmError::invalid_state(
                "commit x86 IOAPIC forwarding activation",
                format_args!("guest GSI {gsi} lost its activation reservation"),
            ));
        }
        *route = IoApicForwardingRouteState::Active(self.operations);
        IOAPIC_IRQ_ACTIVATED.fetch_or(gsi_bit(gsi), Ordering::Release);
        self.disposition = ActivationDisposition::Active;
        Ok(())
    }

    fn revoke_after_failed_activation(&mut self, activation_error: AxVmError) -> AxVmError {
        clear_forwarded_ioapic_pending_state(self.guest_gsi);
        match self.operations.revoke() {
            Ok(()) => activation_error,
            Err(revoke_error) => {
                self.disposition = ActivationDisposition::Quarantined;
                AxVmError::interrupt(
                    "revoke failed x86 IOAPIC forwarding activation",
                    format_args!(
                        "activation failed: {activation_error}; device revoke failed: \
                         {revoke_error}"
                    ),
                )
            }
        }
    }
}

impl Drop for PreparedIoApicForwardingActivation {
    fn drop(&mut self) {
        if matches!(self.disposition, ActivationDisposition::Active) {
            return;
        }
        let mut route = IOAPIC_FORWARDING_ROUTES[self.guest_gsi].lock();
        if matches!(*route, IoApicForwardingRouteState::Activating) {
            *route = match self.disposition {
                ActivationDisposition::Prepared => {
                    IoApicForwardingRouteState::Prepared(self.operations)
                }
                ActivationDisposition::Quarantined => {
                    IoApicForwardingRouteState::Quarantined(self.operations)
                }
                ActivationDisposition::Active => unreachable!(
                    "active x86 IOAPIC activation returned without committing its route"
                ),
            };
        }
    }
}

fn prepare_ioapic_forwarding_activation(
    guest_gsi: usize,
) -> AxVmResult<Option<PreparedIoApicForwardingActivation>> {
    let mut route = IOAPIC_FORWARDING_ROUTES[guest_gsi].lock();
    match *route {
        IoApicForwardingRouteState::Vacant | IoApicForwardingRouteState::Active(_) => Ok(None),
        IoApicForwardingRouteState::Prepared(operations) => {
            *route = IoApicForwardingRouteState::Activating;
            Ok(Some(PreparedIoApicForwardingActivation {
                guest_gsi,
                operations,
                disposition: ActivationDisposition::Prepared,
            }))
        }
        IoApicForwardingRouteState::Activating => Err(AxVmError::invalid_state(
            "activate x86 IOAPIC forwarding route",
            format_args!("guest GSI {guest_gsi} already has an activation in progress"),
        )),
        IoApicForwardingRouteState::Quarantined(_) => Err(AxVmError::invalid_state(
            "activate x86 IOAPIC forwarding route",
            format_args!("guest GSI {guest_gsi} is quarantined after failed device revocation"),
        )),
    }
}

struct IoApicForwardingActivationBatch {
    activated: usize,
}

impl IoApicForwardingActivationBatch {
    const fn new() -> Self {
        Self { activated: 0 }
    }

    fn activate(&mut self, guest_gsi: usize) -> AxVmResult {
        let Some(prepared) = prepare_ioapic_forwarding_activation(guest_gsi)? else {
            return Ok(());
        };
        prepared.activate()?;
        self.activated |= gsi_bit(guest_gsi);
        Ok(())
    }

    fn rollback(&mut self) -> AxVmResult {
        if self.activated == 0 {
            return Ok(());
        }

        for gsi in ioapic_irq_hook_gsis() {
            let bit = gsi_bit(gsi);
            if self.activated & bit == 0
                || IOAPIC_IRQ_ACTION_DISABLED.load(Ordering::Acquire) & bit != 0
            {
                continue;
            }
            set_forwarding_action_enabled(gsi, false).map_err(|error| {
                forwarding_irq_error("disable x86 IOAPIC activation during rollback", gsi, error)
            })?;
            IOAPIC_IRQ_ACTION_DISABLED.fetch_or(bit, Ordering::AcqRel);
        }

        for gsi in ioapic_irq_hook_gsis() {
            if self.activated & gsi_bit(gsi) == 0 {
                continue;
            }
            if let Some(handle) = *IOAPIC_IRQ_HANDLES[gsi].lock() {
                crate::arch::x86_64::host_irq::synchronize_irq(handle).map_err(|error| {
                    forwarding_irq_error("synchronize x86 IOAPIC activation rollback", gsi, error)
                })?;
            }
        }

        IOAPIC_IRQ_PENDING.fetch_and(!self.activated, Ordering::AcqRel);
        IOAPIC_IRQ_PENDING_LEVEL.fetch_and(!self.activated, Ordering::AcqRel);
        let result = revoke_ioapic_forwarding_routes(self.activated);
        self.activated &= IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire);
        result
    }
}

pub(super) fn revoke_ioapic_forwarding_routes(route_mask: usize) -> AxVmResult {
    let mut first_error = None;
    for gsi in ioapic_irq_hook_gsis() {
        let bit = gsi_bit(gsi);
        if route_mask & bit == 0 {
            continue;
        }

        let operations = {
            let route = IOAPIC_FORWARDING_ROUTES[gsi].lock();
            match *route {
                IoApicForwardingRouteState::Active(operations)
                | IoApicForwardingRouteState::Quarantined(operations) => operations,
                _ => {
                    first_error.get_or_insert_with(|| {
                        AxVmError::invalid_state(
                            "revoke x86 IOAPIC forwarding route",
                            format_args!("guest GSI {gsi} is neither active nor quarantined"),
                        )
                    });
                    continue;
                }
            }
        };

        if let Err(error) = operations.revoke() {
            let mut route = IOAPIC_FORWARDING_ROUTES[gsi].lock();
            if matches!(
                *route,
                IoApicForwardingRouteState::Active(_) | IoApicForwardingRouteState::Quarantined(_)
            ) {
                *route = IoApicForwardingRouteState::Quarantined(operations);
                IOAPIC_IRQ_ACTIVATED.fetch_and(!bit, Ordering::AcqRel);
            }
            first_error.get_or_insert_with(|| {
                AxVmError::interrupt(
                    "revoke x86 IOAPIC forwarding device endpoint",
                    format_args!("guest GSI {gsi}: {error}"),
                )
            });
            continue;
        }

        let mut route = IOAPIC_FORWARDING_ROUTES[gsi].lock();
        if !matches!(
            *route,
            IoApicForwardingRouteState::Active(_) | IoApicForwardingRouteState::Quarantined(_)
        ) {
            first_error.get_or_insert_with(|| {
                AxVmError::invalid_state(
                    "commit x86 IOAPIC forwarding revocation",
                    format_args!("guest GSI {gsi} changed while its device endpoint was revoked"),
                )
            });
            continue;
        }
        *route = IoApicForwardingRouteState::Prepared(operations);
        IOAPIC_IRQ_ACTIVATED.fetch_and(!bit, Ordering::AcqRel);
    }

    first_error.map_or(Ok(()), Err)
}

fn ensure_no_quarantined_ioapic_routes() -> AxVmResult {
    for gsi in ioapic_irq_hook_gsis() {
        if matches!(
            *IOAPIC_FORWARDING_ROUTES[gsi].lock(),
            IoApicForwardingRouteState::Quarantined(_)
        ) {
            return Err(AxVmError::invalid_state(
                "enable x86 IOAPIC forwarding",
                format_args!("guest GSI {gsi} remains quarantined after failed device revocation"),
            ));
        }
    }
    Ok(())
}

fn activate_ioapic_forwarding_batch(guest_gsis: impl Iterator<Item = usize>) -> AxVmResult {
    let mut batch = IoApicForwardingActivationBatch::new();
    for guest_gsi in guest_gsis {
        if let Err(error) = batch.activate(guest_gsi) {
            return match batch.rollback() {
                Ok(()) => Err(error),
                Err(rollback) => Err(activation_rollback_error(error, rollback)),
            };
        }
    }
    Ok(())
}

fn activation_rollback_error(activation: AxVmError, rollback: AxVmError) -> AxVmError {
    AxVmError::interrupt(
        "rollback x86 IOAPIC forwarding activation",
        format_args!("activation failed: {activation}; rollback failed: {rollback}"),
    )
}

#[cfg(test)]
pub(super) fn activate_ready_ioapic_forwarding_route_for_test(
    guest_gsi: usize,
    route_ready: bool,
) -> AxVmResult {
    if !should_register_ioapic_gsi_hook(guest_gsi) || !route_ready {
        return Ok(());
    }

    if let Some(prepared) = prepare_ioapic_forwarding_activation(guest_gsi)? {
        prepared.activate()?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn activate_ready_ioapic_forwarding_batch_for_test(guest_gsis: &[usize]) -> AxVmResult {
    let _transaction = acquire_ioapic_route_activation_transaction()?;
    activate_ioapic_forwarding_batch(guest_gsis.iter().copied())
}
