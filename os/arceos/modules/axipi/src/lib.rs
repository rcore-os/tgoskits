//! [ArceOS](https://github.com/arceos-org/arceos) Inter-Processor Interrupt (IPI) primitives.

#![cfg_attr(not(test), no_std)]

#[macro_use]
extern crate log;
extern crate alloc;

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ax_hal::{irq::IpiTarget, percpu::this_cpu_id};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;

mod event;
mod queue;

mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

pub use event::{Callback, MulticastCallback};
use queue::IpiEventQueue;

#[ax_percpu::def_percpu]
static IPI_EVENT_QUEUE: LazyInit<SpinNoIrq<IpiEventQueue>> = LazyInit::new();

const IPI_CPU_NOT_READY: u8 = 0;
const IPI_CPU_BECOMING_READY: u8 = 1;
const IPI_CPU_READY: u8 = 2;

static IPI_CPU_STATE: [AtomicU8; build_info::CPU_CAPACITY] =
    [const { AtomicU8::new(IPI_CPU_NOT_READY) }; build_info::CPU_CAPACITY];

static IPI_READY_CPUS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Initialize the per-CPU IPI event queue.
pub fn init() {
    IPI_EVENT_QUEUE.with_current(|ipi_queue| {
        ipi_queue.init_once(SpinNoIrq::new(IpiEventQueue::default()));
    });
}

/// Marks the current CPU ready to receive and handle queued IPI callbacks.
///
/// The runtime should call this after the local IPI event queue is initialized,
/// the IPI handler is installed, and local IRQs are enabled.
pub fn mark_current_cpu_ready() {
    let cpu_id = this_cpu_id();
    IPI_CPU_STATE[cpu_id].store(IPI_CPU_BECOMING_READY, Ordering::Release);
    ax_hal::asm::flush_tlb(None);
    IPI_CPU_STATE[cpu_id].store(IPI_CPU_READY, Ordering::Release);
    IPI_READY_CPUS.fetch_add(1, Ordering::Release);
}

/// Waits until every online CPU has completed [`mark_current_cpu_ready`].
pub fn wait_for_all_cpus_ready() {
    let cpu_num = ax_hal::cpu_num();
    while IPI_READY_CPUS.load(Ordering::Acquire) < cpu_num {
        core::hint::spin_loop();
    }
}

/// Returns whether `cpu_id` is ready to receive and handle queued IPI callbacks.
pub fn is_cpu_ready(cpu_id: usize) -> bool {
    cpu_id < build_info::CPU_CAPACITY
        && IPI_CPU_STATE[cpu_id].load(Ordering::Acquire) == IPI_CPU_READY
}

/// Waits while `cpu_id` is becoming ready, and returns whether it is ready.
///
/// If a page-table update races with a CPU publishing IPI readiness, the caller
/// must not skip the CPU after it has already started its final local TLB flush.
/// Waiting for the transition to complete lets the caller send a conservative
/// follow-up IPI after the CPU can receive callbacks.
pub fn wait_until_cpu_ready(cpu_id: usize) -> bool {
    if cpu_id >= build_info::CPU_CAPACITY {
        return false;
    }

    loop {
        match IPI_CPU_STATE[cpu_id].load(Ordering::Acquire) {
            IPI_CPU_READY => return true,
            IPI_CPU_NOT_READY => return false,
            _ => core::hint::spin_loop(),
        }
    }
}

/// Executes a callback on the specified destination CPU via IPI.
pub fn run_on_cpu<T: Into<Callback>>(dest_cpu: usize, callback: T) {
    debug!("Send IPI event to CPU {dest_cpu}");
    if dest_cpu == this_cpu_id() {
        // Execute callback on current CPU immediately
        callback.into().call();
    } else {
        unsafe { IPI_EVENT_QUEUE.remote_ref_raw(dest_cpu) }
            .lock()
            .push(this_cpu_id(), callback.into());
        ax_hal::irq::send_ipi(
            ax_hal::irq::ipi_irq(),
            IpiTarget::Other { cpu_id: dest_cpu },
        );
    }
}

/// Executes a raw thunk synchronously on the specified CPU via IPI.
///
/// # Safety
///
/// `arg` must remain valid until this function returns, and `f` must be safe
/// to execute in the target CPU's IPI handler context.
pub unsafe fn run_on_cpu_sync_raw(
    dest_cpu: usize,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), ax_hal::irq::IrqError> {
    if dest_cpu >= ax_hal::cpu_num() {
        return Err(ax_hal::irq::IrqError::InvalidCpu);
    }
    if !wait_until_cpu_ready(dest_cpu) {
        return Err(ax_hal::irq::IrqError::CpuOffline);
    }
    if dest_cpu == this_cpu_id() {
        unsafe { f(arg) };
        return Ok(());
    }

    struct SyncCall {
        done: AtomicBool,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    }

    let call = SyncCall {
        done: AtomicBool::new(false),
        f,
        arg,
    };
    let call_ptr = &call as *const SyncCall as usize;
    run_on_cpu(dest_cpu, move || {
        let call = unsafe { &*(call_ptr as *const SyncCall) };
        unsafe { (call.f)(call.arg) };
        call.done.store(true, Ordering::Release);
    });
    while !call.done.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    Ok(())
}

/// Executes a callback on all other CPUs via IPI.
pub fn run_on_each_cpu<T: Into<MulticastCallback>>(callback: T) {
    info!("Send IPI event to all other CPUs");
    let current_cpu_id = this_cpu_id();
    let cpu_num = ax_hal::cpu_num();
    let callback = callback.into();

    // Execute callback on current CPU immediately
    callback.clone().call();
    // Push the callback to all other CPUs' IPI event queues
    for cpu_id in 0..cpu_num {
        if cpu_id != current_cpu_id {
            unsafe { IPI_EVENT_QUEUE.remote_ref_raw(cpu_id) }
                .lock()
                .push(current_cpu_id, callback.clone().into_unicast());
        }
    }
    // Send IPI to all other CPUs to trigger their callbacks
    ax_hal::irq::send_ipi(
        ax_hal::irq::ipi_irq(),
        IpiTarget::AllExceptCurrent {
            cpu_id: current_cpu_id,
            cpu_num,
        },
    );
}

/// The handler for IPI events. It retrieves the events from the queue and calls the corresponding callbacks.
pub fn ipi_handler() {
    while let Some((src_cpu_id, callback)) = unsafe { IPI_EVENT_QUEUE.current_ref_mut_raw() }
        .lock()
        .pop_one()
    {
        debug!("Received IPI event from CPU {src_cpu_id}");
        callback.call();
    }
}
