//! Allocation-backed callback transport kept only for compatibility.
//!
//! New cross-CPU kernel work must use [`crate::call_on_cpu`] or publish its
//! owner state before [`crate::notify_cpu`]. This module remains for Starry's
//! current stop-machine path and compatibility tests until stopper tasks land.

mod event;
mod queue;

use ax_hal::{
    irq::{CpuId, IrqError},
    percpu::this_cpu_id,
};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
pub use event::{Callback, MulticastCallback};
use queue::IpiEventQueue;

#[ax_percpu::def_percpu]
static IPI_EVENT_QUEUE: LazyInit<SpinNoIrq<IpiEventQueue>> = LazyInit::new();

pub(crate) fn init_current_queue() {
    // SAFETY: runtime initialization keeps this CPU offline with IRQ/re-entry
    // excluded until its queue becomes reachable from another processor.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                IPI_EVENT_QUEUE.with_current_mut(exclusive, |queue| {
                    queue.init_once(SpinNoIrq::new(IpiEventQueue::default()));
                })
            })
        })
    }
    .expect("legacy IPI initialization requires an installed CPU-local area");
}

/// Executes an allocation-backed callback on one CPU.
pub fn run_on_cpu<T: Into<Callback>>(dest_cpu: usize, callback: T) -> Result<(), IrqError> {
    crate::validate_target_cpu(CpuId(dest_cpu))?;
    if dest_cpu == this_cpu_id() {
        callback.into().call();
        return Ok(());
    }

    let area = crate::remote_cpu_area(CpuId(dest_cpu))?;
    // SAFETY: the CPU-local area is permanent and SpinNoIrq serializes remote
    // producers with the owner CPU's interrupt consumer.
    unsafe { IPI_EVENT_QUEUE.remote_ptr(area).as_ref() }
        .lock()
        .push(this_cpu_id(), callback.into());
    crate::notify_cpu(CpuId(dest_cpu)).map(|_| ())
}

/// Executes an allocation-backed callback on every online CPU.
pub fn run_on_each_cpu<T: Into<MulticastCallback>>(callback: T) -> Result<(), IrqError> {
    let current_cpu = this_cpu_id();
    let callback = callback.into();
    callback.clone().call();

    for cpu_id in 0..ax_hal::cpu_num() {
        if cpu_id == current_cpu {
            continue;
        }
        let cpu = CpuId(cpu_id);
        crate::validate_target_cpu(cpu)?;
        let area = crate::remote_cpu_area(cpu)?;
        // SAFETY: the permanent remote object is internally synchronized.
        unsafe { IPI_EVENT_QUEUE.remote_ptr(area).as_ref() }
            .lock()
            .push(current_cpu, callback.clone().into_unicast());
        crate::notify_cpu(cpu)?;
    }
    Ok(())
}

/// Drains all compatibility callbacks queued for the current CPU.
pub fn drain_current_callbacks() {
    while let Some((_source_cpu, callback)) = unsafe {
        // SAFETY: interrupt entry pins this CPU and the queue lock serializes
        // all local and remote producers.
        ax_percpu::with_cpu_pin(|pin| {
            IPI_EVENT_QUEUE.with_current(pin, |queue| queue.lock().pop_one())
        })
    }
    .expect("legacy IPI handling requires an installed CPU-local area")
    {
        callback.call();
    }
}
