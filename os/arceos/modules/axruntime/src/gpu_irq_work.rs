//! Task-context service for GPU interrupts and display events.

use alloc::string::String;

use ax_lazyinit::OnceLock;

use crate::irq::FixedIrqWorkerSignal;

static GPU_IRQ_WORK: FixedIrqWorkerSignal = FixedIrqWorkerSignal::new();
static GPU_IRQ_HANDLE: OnceLock<ax_hal::irq::IrqHandle> = OnceLock::new();

pub(crate) fn start() {
    if ax_gpu::irq_id().is_none() {
        return;
    }
    if let Err(error) = crate::thread::builder(String::from("gpu-irq-work")).spawn(|| {
        loop {
            GPU_IRQ_WORK
                .wait()
                .unwrap_or_else(|error| panic!("GPU IRQ worker could not wait: {error}"));
            if let Err(error) = ax_gpu::service_irq_work() {
                warn!("GPU IRQ work failed: {error}");
            }
        }
    }) {
        warn!("failed to start GPU IRQ worker: {error:?}");
        return;
    }
    ax_gpu::set_irq_work_notifier(notify);
    let irq = ax_gpu::irq_id().unwrap();
    let request = ax_hal::irq::IrqRequest::new(|_| {
        if ax_gpu::handle_irq() {
            ax_hal::irq::IrqReturn::Handled
        } else {
            ax_hal::irq::IrqReturn::Unhandled
        }
    })
    .share_mode(ax_hal::irq::ShareMode::Shared)
    .auto_enable(ax_hal::irq::AutoEnable::No);
    let handle = match ax_hal::irq::request_irq(irq, request) {
        Ok(handle) => handle,
        Err(error) => {
            warn!("failed to register GPU IRQ handler for {irq:?}: {error:?}");
            return;
        }
    };
    ax_gpu::enable_irq();
    if let Err(error) = ax_hal::irq::enable_irq(handle) {
        ax_gpu::disable_irq();
        if let Err(free_error) = ax_hal::irq::free_irq(handle) {
            warn!("failed to release GPU IRQ after enable failure: {free_error:?}");
        }
        warn!("failed to enable GPU IRQ handler for {irq:?}: {error:?}");
        return;
    }
    GPU_IRQ_HANDLE.call_once(|| handle);
}

fn notify() {
    GPU_IRQ_WORK.notify();
}
