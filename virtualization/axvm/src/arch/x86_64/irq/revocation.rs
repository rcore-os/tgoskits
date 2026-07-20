//! Mask, drain, and revoke x86 IOAPIC passthrough ownership.

use core::sync::atomic::Ordering;

use ax_kspin::SpinRaw as Mutex;
use ax_std::os::arceos::task::{ThreadWakeHandle, WaitQueue, WakeResult, current_thread_handle};

use super::{activation::revoke_ioapic_forwarding_routes, state::*};
use crate::{
    AxVMRef, AxVmError, arch::x86_64::host_irq as irq, architecture::ops::VcpuIrqOwnerSession,
    config::VMInterruptMode, runtime::VCpuRef,
};

const OWNER_RELEASE_INACTIVE: usize = 0;
const OWNER_RELEASE_ARMED: usize = 1;
const OWNER_RELEASE_REQUESTED: usize = 2;
const OWNER_RELEASE_CLOSED: usize = 3;
const OWNER_RELEASE_FAILED: usize = 4;

static OWNER_RELEASE_STATE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(OWNER_RELEASE_INACTIVE);
static OWNER_RELEASE_VM_ID: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
static OWNER_RELEASE_VCPU_ID: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
static OWNER_RELEASE_WAKE: Mutex<Option<ThreadWakeHandle>> = Mutex::new(None);
static OWNER_RELEASE_COMPLETION: WaitQueue = WaitQueue::new();

pub(crate) fn prepare_ioapic_irq_owner_session(
    vm: &AxVMRef,
    vcpu: &VCpuRef,
) -> crate::AxVmResult<Option<VcpuIrqOwnerSession>> {
    let requires_owner = ioapic_irq_hook_gsis().any(ioapic_forwarding_route_requires_host_irq);
    if vm.interrupt_mode() != VMInterruptMode::Passthrough || vcpu.id() != 0 || !requires_owner {
        return Ok(None);
    }
    if vm.vcpu_num() != 1 {
        return Err(AxVmError::invalid_config(format_args!(
            "x86 passthrough IRQ ownership requires exactly one vCPU, got {}",
            vm.vcpu_num()
        )));
    }

    let session = VcpuIrqOwnerSession::acquire(
        vm.id(),
        vcpu.id(),
        ioapic_irq_owner_release_requested,
        close_ioapic_irq_forwarding_on_owner,
    )?;
    let owner_cpu = session.owner_cpu();
    let owner_mask = 1usize.checked_shl(owner_cpu as u32).ok_or_else(|| {
        AxVmError::invalid_config(format_args!(
            "x86 IOAPIC owner CPU {owner_cpu} exceeds the host CPU mask"
        ))
    })?;
    if vcpu.phys_cpu_set() != Some(owner_mask) {
        return Err(AxVmError::invalid_config(format_args!(
            "x86 passthrough VM[{}] VCpu[{}] must remain fixed to owner CPU {owner_cpu}",
            vm.id(),
            vcpu.id()
        )));
    }

    let thread = current_thread_handle().map_err(|error| {
        AxVmError::resource_unavailable("x86 IOAPIC forwarding owner thread", error)
    })?;
    let wake = thread.wake_handle();
    drop(thread);
    let wake_cpu = wake.target_cpu().map(|cpu| cpu.as_u32() as usize);
    if wake_cpu != Some(owner_cpu) {
        return Err(AxVmError::resource_conflict(
            "x86 IOAPIC forwarding owner wake",
            format_args!("wake targets {wake_cpu:?}, owner lease pins CPU {owner_cpu}"),
        ));
    }

    arm_ioapic_owner_release(vm.id(), vcpu.id(), wake)?;
    Ok(Some(session))
}

fn arm_ioapic_owner_release(
    vm_id: usize,
    vcpu_id: usize,
    wake: ThreadWakeHandle,
) -> crate::AxVmResult {
    let observed = OWNER_RELEASE_STATE.load(Ordering::Acquire);
    if !matches!(observed, OWNER_RELEASE_INACTIVE | OWNER_RELEASE_CLOSED) {
        return Err(AxVmError::resource_conflict(
            "arm x86 IOAPIC owner release",
            format_args!("a previous owner release remains in state {observed}"),
        ));
    }
    if IOAPIC_IRQ_HANDLES.iter().any(|slot| slot.lock().is_some()) {
        return Err(AxVmError::resource_conflict(
            "arm x86 IOAPIC owner release",
            "a previous owner still retains forwarding actions",
        ));
    }

    let mut owner_wake = OWNER_RELEASE_WAKE.lock();
    if owner_wake.is_some() {
        return Err(AxVmError::resource_conflict(
            "arm x86 IOAPIC owner release",
            "a previous owner wake capability remains installed",
        ));
    }
    *owner_wake = Some(wake);
    OWNER_RELEASE_VM_ID.store(vm_id, Ordering::Relaxed);
    OWNER_RELEASE_VCPU_ID.store(vcpu_id, Ordering::Relaxed);
    OWNER_RELEASE_STATE.store(OWNER_RELEASE_ARMED, Ordering::Release);
    Ok(())
}

/// Revokes every guest forwarding path and waits for callbacks that observed
/// the previous VM identity.
pub fn revoke_ioapic_irq_forwarding_for_vm(vm: &AxVMRef) -> crate::AxVmResult {
    let vm_id = vm.id();
    let published = loop {
        match OWNER_RELEASE_STATE.load(Ordering::Acquire) {
            OWNER_RELEASE_INACTIVE | OWNER_RELEASE_CLOSED => {
                return verify_ioapic_irq_forwarding_closed(vm_id);
            }
            OWNER_RELEASE_FAILED => return ioapic_owner_close_failure(vm_id),
            OWNER_RELEASE_ARMED => {
                ensure_release_identity(vm_id, 0)?;
                if OWNER_RELEASE_STATE
                    .compare_exchange(
                        OWNER_RELEASE_ARMED,
                        OWNER_RELEASE_REQUESTED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    break true;
                }
            }
            OWNER_RELEASE_REQUESTED => {
                ensure_release_identity(vm_id, 0)?;
                break false;
            }
            state => {
                return Err(AxVmError::invalid_state(
                    "request x86 IOAPIC owner release",
                    format_args!("unknown owner release state {state}"),
                ));
            }
        }
    };
    if published {
        wake_ioapic_irq_owner(vm_id)?;
    }

    OWNER_RELEASE_COMPLETION
        .try_wait_until(|| {
            matches!(
                OWNER_RELEASE_STATE.load(Ordering::Acquire),
                OWNER_RELEASE_CLOSED | OWNER_RELEASE_FAILED
            )
        })
        .map_err(|error| {
            AxVmError::resource_unavailable("wait for x86 IOAPIC owner close", error)
        })?;
    match OWNER_RELEASE_STATE.load(Ordering::Acquire) {
        OWNER_RELEASE_CLOSED => verify_ioapic_irq_forwarding_closed(vm_id),
        OWNER_RELEASE_FAILED => ioapic_owner_close_failure(vm_id),
        state => Err(AxVmError::invalid_state(
            "complete x86 IOAPIC owner release",
            format_args!("completion woke in non-terminal state {state}"),
        )),
    }
}

fn wake_ioapic_irq_owner(vm_id: usize) -> crate::AxVmResult {
    let wake = OWNER_RELEASE_WAKE.lock().clone();
    let Some(wake) = wake else {
        if OWNER_RELEASE_STATE.load(Ordering::Acquire) == OWNER_RELEASE_CLOSED {
            return Ok(());
        }
        return Err(AxVmError::resource_unavailable(
            "wake x86 IOAPIC owner",
            format_args!("VM[{vm_id}] has no retained owner wake capability"),
        ));
    };
    match wake.wake() {
        WakeResult::Notified | WakeResult::AlreadyPending => Ok(()),
        WakeResult::Exited | WakeResult::Unavailable
            if OWNER_RELEASE_STATE.load(Ordering::Acquire) == OWNER_RELEASE_CLOSED =>
        {
            Ok(())
        }
        WakeResult::Exited | WakeResult::Unavailable => Err(AxVmError::resource_unavailable(
            "wake x86 IOAPIC owner",
            format_args!("VM[{vm_id}] owner is no longer wakeable"),
        )),
    }
}

fn ioapic_irq_owner_release_requested(vm_id: usize, vcpu_id: usize) -> bool {
    OWNER_RELEASE_STATE.load(Ordering::Acquire) == OWNER_RELEASE_REQUESTED
        && OWNER_RELEASE_VM_ID.load(Ordering::Relaxed) == vm_id
        && OWNER_RELEASE_VCPU_ID.load(Ordering::Relaxed) == vcpu_id
}

fn ensure_release_identity(vm_id: usize, vcpu_id: usize) -> crate::AxVmResult {
    let owner_vm = OWNER_RELEASE_VM_ID.load(Ordering::Relaxed);
    let owner_vcpu = OWNER_RELEASE_VCPU_ID.load(Ordering::Relaxed);
    if owner_vm == vm_id && owner_vcpu == vcpu_id {
        return Ok(());
    }
    Err(AxVmError::resource_conflict(
        "x86 IOAPIC owner release",
        format_args!(
            "VM[{owner_vm}] VCpu[{owner_vcpu}] owns the session, not VM[{vm_id}] VCpu[{vcpu_id}]"
        ),
    ))
}

fn ioapic_owner_close_failure(vm_id: usize) -> crate::AxVmResult {
    ensure_release_identity(vm_id, 0)?;
    Err(AxVmError::invalid_state(
        "close x86 IOAPIC forwarding actions",
        format_args!("VM[{vm_id}] owner quarantined a failed action teardown"),
    ))
}

fn verify_ioapic_irq_forwarding_closed(vm_id: usize) -> crate::AxVmResult {
    let owner = IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire);
    let actions_live = IOAPIC_IRQ_HANDLES.iter().any(|slot| slot.lock().is_some());
    let state_live = IOAPIC_IRQ_FORWARDING_ENABLED.load(Ordering::Acquire)
        || IOAPIC_IRQ_HOOK_REGISTERED.load(Ordering::Acquire)
        || IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire) != 0
        || IOAPIC_IRQ_ACTION_DISABLED.load(Ordering::Acquire) != 0
        || IOAPIC_IRQ_OWNER_BOUND.load(Ordering::Acquire) != 0
        || IOAPIC_IRQ_PENDING.load(Ordering::Acquire) != 0
        || IOAPIC_IRQ_PENDING_LEVEL.load(Ordering::Acquire) != 0;
    if owner != usize::MAX || actions_live || state_live {
        return Err(crate::AxVmError::invalid_state(
            "verify x86 IOAPIC forwarding owner close",
            format_args!(
                "VM[{vm_id}] owner thread has not completed IRQ action teardown (published owner \
                 {owner})"
            ),
        ));
    }
    Ok(())
}

fn close_ioapic_irq_forwarding_on_owner(vm_id: usize, vcpu_id: usize) -> crate::AxVmResult {
    ensure_release_identity(vm_id, vcpu_id)?;
    let result = close_ioapic_irq_forwarding_actions_on_owner(vm_id);
    match result {
        Ok(()) => {
            *OWNER_RELEASE_WAKE.lock() = None;
            OWNER_RELEASE_STATE.store(OWNER_RELEASE_CLOSED, Ordering::Release);
            OWNER_RELEASE_COMPLETION.notify_all();
            Ok(())
        }
        Err(error) => {
            OWNER_RELEASE_STATE.store(OWNER_RELEASE_FAILED, Ordering::Release);
            OWNER_RELEASE_COMPLETION.notify_all();
            Err(error)
        }
    }
}

fn close_ioapic_irq_forwarding_actions_on_owner(vm_id: usize) -> crate::AxVmResult {
    let _transaction = IoApicRouteTransaction::try_acquire().ok_or_else(|| {
        crate::AxVmError::invalid_state(
            "close x86 IOAPIC forwarding routes on owner",
            "another route activation or revocation transaction is active",
        )
    })?;
    let owner = IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire);
    if owner != usize::MAX && owner != vm_id {
        return Err(crate::AxVmError::resource_conflict(
            "revoke x86 IOAPIC forwarding owner",
            format_args!("VM[{owner}] owns the forwarding fabric, not VM[{vm_id}]"),
        ));
    }
    if IOAPIC_IRQ_OWNER_THREAD_ID.load(Ordering::Acquire) != u64::MAX
        || IOAPIC_IRQ_HANDLES.iter().any(|slot| slot.lock().is_some())
    {
        ensure_current_ioapic_forwarding_owner()?;
    }
    if ioapic_forwarding_activation_in_progress() {
        return Err(crate::AxVmError::invalid_state(
            "revoke x86 IOAPIC forwarding routes",
            "a route activation is still in progress",
        ));
    }

    let activated = disable_active_ioapic_forwarding_actions().map_err(revocation_irq_error)?;
    disable_ioapic_forwarding_actions().map_err(revocation_irq_error)?;
    IOAPIC_IRQ_FORWARD_VM_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARDING_ENABLED.store(false, Ordering::Release);
    drain_disabled_ioapic_forwarding().map_err(revocation_irq_error)?;
    let endpoint_result =
        revoke_ioapic_forwarding_routes(activated | quarantined_ioapic_route_mask());
    let action_result = release_ioapic_forwarding_actions();
    merge_revocation_results(endpoint_result, action_result)
}

fn ensure_current_ioapic_forwarding_owner() -> crate::AxVmResult {
    let expected_thread = IOAPIC_IRQ_OWNER_THREAD_ID.load(Ordering::Acquire);
    let expected_cpu = IOAPIC_IRQ_OWNER_CPU.load(Ordering::Acquire);
    let current = current_thread_handle().map_err(|error| {
        crate::AxVmError::resource_unavailable("x86 IOAPIC forwarding owner thread", error)
    })?;
    let wake = current.wake_handle();
    let current_thread = wake.thread_id().as_u64();
    let target_cpu = wake.target_cpu().map(|cpu| cpu.as_u32() as usize);
    let current_cpu = ax_std::os::arceos::modules::ax_hal::percpu::this_cpu_id();
    if current_thread == expected_thread
        && target_cpu == Some(expected_cpu)
        && current_cpu == expected_cpu
    {
        return Ok(());
    }
    Err(crate::AxVmError::resource_conflict(
        "close x86 IOAPIC forwarding actions",
        format_args!(
            "owner thread {expected_thread:#x} on CPU {expected_cpu} is required, current thread \
             is {current_thread:#x} with target {target_cpu:?} on CPU {current_cpu}"
        ),
    ))
}

fn revocation_irq_error(error: irq::IrqError) -> crate::AxVmError {
    crate::AxVmError::interrupt(
        "drain x86 passthrough IRQ forwarding",
        format_args!("{error:?}"),
    )
}

fn quarantined_ioapic_route_mask() -> usize {
    ioapic_irq_hook_gsis().fold(0, |mask, gsi| {
        if matches!(
            *IOAPIC_FORWARDING_ROUTES[gsi].lock(),
            IoApicForwardingRouteState::Quarantined(_)
        ) {
            mask | gsi_bit(gsi)
        } else {
            mask
        }
    })
}

fn disable_active_ioapic_forwarding_actions() -> Result<usize, irq::IrqError> {
    let activated = IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire);
    for gsi in ioapic_irq_hook_gsis() {
        let bit = gsi_bit(gsi);
        if activated & bit == 0 || IOAPIC_IRQ_ACTION_DISABLED.load(Ordering::Acquire) & bit != 0 {
            continue;
        }
        set_forwarding_action_enabled(gsi, false)?;
        IOAPIC_IRQ_ACTION_DISABLED.fetch_or(bit, Ordering::AcqRel);
    }
    Ok(activated)
}

fn disable_ioapic_forwarding_actions() -> Result<(), irq::IrqError> {
    for (gsi, slot) in IOAPIC_IRQ_HANDLES.iter().enumerate() {
        if let Some(handle) = *slot.lock() {
            match irq::disable_irq(handle) {
                Ok(()) => {}
                Err(irq::IrqError::NotFound) => clear_forwarding_handle(gsi, slot, handle),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn drain_disabled_ioapic_forwarding() -> Result<(), irq::IrqError> {
    for (gsi, slot) in IOAPIC_IRQ_HANDLES.iter().enumerate() {
        if let Some(handle) = *slot.lock() {
            match irq::synchronize_irq(handle) {
                Ok(()) => {}
                Err(irq::IrqError::NotFound) => clear_forwarding_handle(gsi, slot, handle),
                Err(error) => return Err(error),
            }
        }
    }
    IOAPIC_IRQ_PENDING.store(0, Ordering::Release);
    IOAPIC_IRQ_PENDING_LEVEL.store(0, Ordering::Release);
    Ok(())
}

fn release_ioapic_forwarding_actions() -> crate::AxVmResult {
    let mut first_error = None;
    for (gsi, slot) in IOAPIC_IRQ_HANDLES.iter().enumerate() {
        let Some(handle) = *slot.lock() else {
            continue;
        };
        match irq::free_irq(handle) {
            Ok(()) | Err(irq::IrqError::NotFound) => clear_forwarding_handle(gsi, slot, handle),
            Err(error) => {
                first_error.get_or_insert_with(|| {
                    forwarding_irq_error("release x86 IOAPIC forwarding IRQ action", gsi, error)
                });
            }
        }
    }

    if IOAPIC_IRQ_HANDLES
        .iter()
        .all(|handle| handle.lock().is_none())
    {
        IOAPIC_IRQ_HOOK_REGISTERED.store(false, Ordering::Release);
        IOAPIC_IRQ_ACTION_DISABLED.store(0, Ordering::Release);
        IOAPIC_IRQ_OWNER_BOUND.store(0, Ordering::Release);
        IOAPIC_IRQ_OWNER_THREAD_ID.store(u64::MAX, Ordering::Release);
        IOAPIC_IRQ_OWNER_CPU.store(usize::MAX, Ordering::Release);
    }
    first_error.map_or(Ok(()), Err)
}

fn clear_forwarding_handle(gsi: usize, slot: &IoApicForwardingHandleSlot, handle: irq::IrqHandle) {
    let mut current = slot.lock();
    if *current == Some(handle) {
        *current = None;
        IOAPIC_IRQ_OWNER_BOUND.fetch_and(!gsi_bit(gsi), Ordering::AcqRel);
    }
}

fn merge_revocation_results(
    endpoint_result: crate::AxVmResult,
    action_result: crate::AxVmResult,
) -> crate::AxVmResult {
    match (endpoint_result, action_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(endpoint), Err(action)) => Err(crate::AxVmError::interrupt(
            "revoke x86 IOAPIC forwarding ownership",
            format_args!("device endpoint revoke failed: {endpoint}; IRQ release failed: {action}"),
        )),
    }
}
