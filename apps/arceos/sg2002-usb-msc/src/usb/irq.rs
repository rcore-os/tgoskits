//! Application-owned IRQ acknowledgement and task-context USB event service.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_driver::usb::UsbHostIrqHandler;
use ax_runtime::{
    hal::irq::{
        AutoEnable, IrqHandle, IrqRequest, IrqReturn, ShareMode, enable_irq, free_irq,
        in_irq_context, request_irq,
    },
    irq::{FixedIrqWorkerSignal, resolve_binding_irq},
};
use crab_usb::Event;

use super::{DeviceGuard, PlatformUsbHost, SmokeError, std};

type WorkerResult = Result<(), ax_runtime::task::thread::TaskError>;

struct Work {
    signal: FixedIrqWorkerSignal,
    stopping: AtomicBool,
    acknowledged: AtomicUsize,
    batches: AtomicUsize,
}

pub(super) struct UsbIrqRegistration {
    registration: Option<IrqHandle>,
    work: Arc<Work>,
    worker: Option<std::thread::JoinHandle<WorkerResult>>,
}

impl UsbIrqRegistration {
    pub(super) fn install(host: &mut DeviceGuard<PlatformUsbHost>) -> Result<Self, SmokeError> {
        host.disable_irq()?;
        let (binding, handler) = host
            .take_binding_irq_handler()
            .ok_or(SmokeError::MissingIrqBinding)?;
        let irq = resolve_binding_irq(binding).map_err(SmokeError::IrqResolve)?;
        let handler = Arc::new(handler);
        let work = Arc::new(Work {
            signal: FixedIrqWorkerSignal::new(),
            stopping: AtomicBool::new(false),
            acknowledged: AtomicUsize::new(0),
            batches: AtomicUsize::new(0),
        });
        let worker_handler = handler.clone();
        let worker_work = work.clone();
        let worker = std::thread::Builder::new()
            .name("sg2002-usb-events".into())
            .spawn(move || service(&worker_handler, &worker_work))
            .map_err(SmokeError::IrqWorker)?;
        let mut guard = Self {
            registration: None,
            work,
            worker: Some(worker),
        };
        let irq_work = guard.work.clone();
        let request = IrqRequest::new(move |_| {
            if handler.acknowledge_irq() {
                irq_work.acknowledged.fetch_add(1, Ordering::Relaxed);
                irq_work.signal.notify();
                IrqReturn::Handled
            } else {
                IrqReturn::Unhandled
            }
        })
        .share_mode(ShareMode::Shared)
        .auto_enable(AutoEnable::No);
        guard.registration = Some(request_irq(irq, request).map_err(SmokeError::IrqRegister)?);
        std::println!("SG2002_DWC2_IRQ_MODE mode=deferred irq={irq:?}");
        Ok(guard)
    }

    pub(super) fn enable(&self) -> Result<(), SmokeError> {
        enable_irq(self.registration.expect("registered USB IRQ")).map_err(SmokeError::IrqRegister)
    }

    pub(super) fn stop(&mut self) -> Result<(), SmokeError> {
        // free_irq removes the action and synchronizes all in-flight callbacks.
        if let Some(handle) = self.registration {
            free_irq(handle).map_err(SmokeError::IrqRegister)?;
            self.registration = None;
        }
        self.work.stopping.store(true, Ordering::Release);
        self.work.signal.notify();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(SmokeError::IrqWorker)?
                .map_err(SmokeError::IrqWorkerWait)?;
            std::println!(
                "SG2002_DWC2_IRQ_DONE acknowledged={} task_batches={}",
                self.work.acknowledged.load(Ordering::Relaxed),
                self.work.batches.load(Ordering::Relaxed),
            );
        }
        Ok(())
    }
}

impl Drop for UsbIrqRegistration {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            std::println!("SG2002_DWC2_MSC_FAIL IRQ cleanup failed: {error}");
        }
    }
}

fn service(handler: &UsbHostIrqHandler, work: &Work) -> WorkerResult {
    loop {
        work.signal.wait()?;
        if work.stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        assert!(
            !in_irq_context(),
            "USB event draining requires task context"
        );
        work.batches.fetch_add(1, Ordering::Relaxed);
        // drain_event consumes one event class at a time. Keep the source
        // masked until every latched class has been consumed; a budget leaves
        // it masked and reschedules the same worker without hardware polling.
        let mut exhausted = true;
        for _ in 0..32 {
            if matches!(handler.drain_event(), Event::Nothing) {
                exhausted = false;
                break;
            }
        }
        if exhausted {
            work.signal.notify();
            std::thread::yield_now();
        } else if !work.stopping.load(Ordering::Acquire) {
            handler.rearm_irq();
        }
    }
}
