//! Physical inter-processor notification and hard-call transport for ArceOS.
//!
//! Logical pending state belongs to each subsystem. A publisher must make its
//! state visible with Release ordering before calling [`notify_cpu`]. The IPI
//! controller must be acknowledged before entering the logical handler; the
//! handler then calls [`claim_current_delivery`] before it observes and drains
//! scheduler work or hard calls. This matches Linux's IPI doorbell ordering and
//! lets a publication racing with draining obtain a fresh physical edge.
//! [`IpiNotification::Coalesced`] acknowledges only that a physical edge is
//! armed or its controller send is in flight; it does not acknowledge
//! completion of any logical work.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{
    pin::pin,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ax_hal::{
    irq::{CpuId, IpiTarget, IrqError},
    percpu::this_cpu_id,
};
use ax_lazyinit::LazyInit;

mod hard_call;
mod notification;

#[cfg(test)]
mod hard_call_contract;
#[cfg(test)]
mod notification_contract;

use hard_call::{HardCall, HardCallQueue};
pub use notification::IpiNotification;

const IPI_CPU_NOT_READY: u8 = 0;
const IPI_CPU_INITIALIZING: u8 = 1;
const IPI_CPU_READY: u8 = 2;

struct CpuEndpoint {
    edge: notification::DeliveryEdge,
    state: AtomicU8,
    hard_calls: HardCallQueue,
}

impl CpuEndpoint {
    const fn new() -> Self {
        Self {
            edge: notification::DeliveryEdge::new(),
            state: AtomicU8::new(IPI_CPU_NOT_READY),
            hard_calls: HardCallQueue::new(),
        }
    }
}

static CPU_ENDPOINTS: LazyInit<Box<[CpuEndpoint]>> = LazyInit::new();
static IPI_READY_CPUS: AtomicUsize = AtomicUsize::new(0);

const HARD_CALL_IRQ_BUDGET: usize = 64;

/// Initializes the current CPU's IPI endpoint in the `Initializing` state.
///
/// The runtime must install and enable local IPI delivery, perform any final
/// architecture policy such as a local TLB flush, and then call
/// [`mark_current_cpu_ready`].
pub fn init() {
    if !CPU_ENDPOINTS.is_inited() {
        assert!(
            ax_hal::percpu::this_cpu_is_bsp(),
            "the BSP must preallocate IPI endpoints before secondary CPUs initialize"
        );
        CPU_ENDPOINTS.get_or_init(|| {
            let cpu_num = ax_hal::cpu_num();
            let mut endpoints = Vec::with_capacity(cpu_num);
            endpoints.resize_with(cpu_num, CpuEndpoint::new);
            endpoints.into_boxed_slice()
        });
    }

    endpoint(CpuId(this_cpu_id()))
        .expect("current CPU must have a preallocated IPI endpoint")
        .state
        .compare_exchange(
            IPI_CPU_NOT_READY,
            IPI_CPU_INITIALIZING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .expect("IPI endpoint must be initialized exactly once per CPU");
}

/// Publishes that the current CPU can receive and drain IPI work.
///
/// This function owns only endpoint state. Platform-specific finalization must
/// be completed by the runtime before this transition.
pub fn mark_current_cpu_ready() {
    endpoint(CpuId(this_cpu_id()))
        .expect("current CPU must have a preallocated IPI endpoint")
        .state
        .compare_exchange(
            IPI_CPU_INITIALIZING,
            IPI_CPU_READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .expect("IPI endpoint must transition from Initializing to Ready once");
    IPI_READY_CPUS.fetch_add(1, Ordering::Release);
}

/// Waits until every configured CPU has published a ready IPI endpoint.
pub fn wait_for_all_cpus_ready() {
    let cpu_num = ax_hal::cpu_num();
    while IPI_READY_CPUS.load(Ordering::Acquire) < cpu_num {
        core::hint::spin_loop();
    }
}

/// Returns whether `cpu_id` can receive and drain IPI work.
pub fn is_cpu_ready(cpu_id: usize) -> bool {
    endpoint(CpuId(cpu_id)).is_ok_and(|endpoint| {
        endpoint_delivery_ready(endpoint.state.load(Ordering::Acquire)).unwrap_or(false)
    })
}

fn endpoint_delivery_ready(state: u8) -> Result<bool, u8> {
    match state {
        IPI_CPU_READY => Ok(true),
        IPI_CPU_NOT_READY | IPI_CPU_INITIALIZING => Ok(false),
        _ => Err(state),
    }
}

/// Arms or coalesces one physical IPI edge for `cpu`.
///
/// The caller must publish its logical pending state or payload with Release
/// ordering before calling this function. A delivery error does not consume or
/// clear that owner state. [`IpiNotification::Coalesced`] means that a physical
/// edge is already armed or its controller send is in flight, not that the
/// caller's logical work has been drained. The in-flight case must remain
/// wait-free because an IRQ may preempt the sender and publish more work for the
/// same destination.
pub fn notify_cpu(cpu: CpuId) -> Result<IpiNotification, IrqError> {
    validate_target_cpu(cpu)?;
    endpoint(cpu)?.edge.notify(|| {
        let target = if cpu.0 == this_cpu_id() {
            IpiTarget::Current
        } else {
            IpiTarget::Cpu(cpu)
        };
        ax_hal::irq::send_ipi(ax_hal::irq::ipi_irq(), target)
    })
}

/// Claims the acknowledged physical delivery before logical owners are
/// inspected.
///
/// The platform IRQ entry must have already cleared or priority-dropped the
/// controller state for this IPI. Otherwise a racing publisher can re-arm the
/// software edge while the controller still treats the old IPI as in service.
pub fn claim_current_delivery() {
    endpoint(CpuId(this_cpu_id()))
        .expect("current CPU must have a preallocated IPI endpoint")
        .edge
        .claim();
}

/// Executes a raw operation synchronously on one CPU without allocation.
///
/// A remote caller spin-waits until the target completes the operation. This is
/// intended for bounded, one-off hard-IRQ work, not bulk delivery or sustained
/// pressure. A subsystem with queued work should publish its own logical state
/// and use [`notify_cpu`] to coalesce physical delivery.
///
/// # Safety
///
/// `arg` must remain valid until this function returns, and `operation` must be
/// safe to execute in hard-IRQ context without sleeping, allocating, dropping
/// owned state, or acquiring a lock that the caller may hold. It must always
/// terminate.
pub unsafe fn call_on_cpu(
    cpu: CpuId,
    operation: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), IrqError> {
    validate_target_cpu(cpu)?;
    if cpu.0 == this_cpu_id() {
        unsafe { operation(arg) };
        return Ok(());
    }

    let call = pin!(HardCall::new(operation, arg));
    let queue = &endpoint(cpu)?.hard_calls;
    // SAFETY: the request remains pinned on this stack and this function does
    // not return until the target or cancellation path marks it complete.
    unsafe { queue.publish(call.as_ref()) };

    if let Err(error) = notify_cpu(cpu)
        && queue.cancel_after_delivery_error(call.as_ref())
    {
        return Err(error);
    }

    call.wait();
    Ok(())
}

/// Drains at most 64 caller-owned hard calls on the current CPU.
///
/// Any bounded remainder obtains a self-notification unless another publisher
/// has already armed a fresh edge. This budget provides interrupt-transport
/// fairness; it does not make [`call_on_cpu`] a batching interface.
pub fn drain_hard_calls() -> Result<(), IrqError> {
    let outcome = endpoint(CpuId(this_cpu_id()))?
        .hard_calls
        .drain(HARD_CALL_IRQ_BUDGET);

    if outcome.more_work {
        notify_cpu(CpuId(this_cpu_id()))?;
    }
    Ok(())
}

pub(crate) fn validate_target_cpu(cpu: CpuId) -> Result<(), IrqError> {
    endpoint(cpu)?;
    if !ax_hal::irq::is_cpu_online(cpu.0) || !is_cpu_ready(cpu.0) {
        return Err(IrqError::CpuOffline);
    }
    Ok(())
}

fn endpoint(cpu: CpuId) -> Result<&'static CpuEndpoint, IrqError> {
    CPU_ENDPOINTS
        .get()
        .and_then(|endpoints| endpoints.get(cpu.0))
        .ok_or(IrqError::InvalidCpu)
}

#[cfg(test)]
mod tests {
    use super::{IPI_CPU_INITIALIZING, endpoint_delivery_ready};

    #[test]
    fn initializing_endpoint_is_not_a_delivery_target() {
        assert_eq!(endpoint_delivery_ready(IPI_CPU_INITIALIZING), Ok(false));
    }
}
