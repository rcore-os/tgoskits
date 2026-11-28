#[cfg(axtest)]
use alloc::boxed::Box;
use alloc::{collections::VecDeque, sync::Arc};

#[cfg(axtest)]
use ax_lazyinit::OnceLock;
use rdif_serial::Config;

use crate::{
    RuntimeError, RuntimeResult,
    task::sync::{Mutex, RawSpinLock, WaitQueue},
};

pub(super) const CONTROL_QUEUE_CAPACITY: usize = 32;

pub(super) enum ControlOp {
    Start(Config),
    StartHandoffConsole,
    Shutdown,
    SetConfig(Config),
    DiscardRx,
    DiscardTx,
}

pub(super) struct ControlCommand {
    pub(super) op: ControlOp,
    completion: Arc<CommandCompletion>,
}

impl ControlCommand {
    pub(super) fn complete(self, result: RuntimeResult) {
        self.completion.complete(result);
    }
}

pub(super) struct DrainCompletion {
    completion: Arc<CommandCompletion>,
    pub(super) wait_for_hardware: bool,
}

impl DrainCompletion {
    pub(super) fn complete(self, result: RuntimeResult) {
        self.completion.complete(result);
    }
}

pub(super) enum ControlRequest {
    Command(ControlCommand),
    DrainTx(DrainCompletion),
}

pub(super) struct ControlQueue {
    requests: RawSpinLock<VecDeque<ControlRequest>>,
    #[cfg(axtest)]
    service_for_test: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl ControlQueue {
    pub(super) fn new() -> Self {
        Self {
            requests: RawSpinLock::new(VecDeque::with_capacity(CONTROL_QUEUE_CAPACITY)),
            #[cfg(axtest)]
            service_for_test: OnceLock::new(),
        }
    }

    /// Drives a fixture's worker synchronously once a request is queued.
    #[cfg(axtest)]
    pub(super) fn set_test_service(&self, service: impl Fn() + Send + Sync + 'static) {
        self.service_for_test.call_once(|| Box::new(service));
    }

    fn notify_worker(&self, notify: impl FnOnce()) {
        notify();
        #[cfg(axtest)]
        if let Some(service) = self.service_for_test.get() {
            service();
        }
    }

    pub(super) fn submit(&self, op: ControlOp, notify: impl FnOnce()) -> RuntimeResult {
        let completion = Arc::new(CommandCompletion::new());
        {
            let mut requests = self.requests.lock_irqsave();
            if requests.len() == CONTROL_QUEUE_CAPACITY {
                return Err(RuntimeError::SerialControlBusy);
            }
            requests.push_back(ControlRequest::Command(ControlCommand {
                op,
                completion: completion.clone(),
            }));
        }
        self.notify_worker(notify);
        completion.wait()
    }

    pub(super) fn submit_drain(
        &self,
        wait_for_hardware: bool,
        notify: impl FnOnce(),
    ) -> RuntimeResult {
        let completion = Arc::new(CommandCompletion::new());
        {
            let mut requests = self.requests.lock_irqsave();
            if requests.len() == CONTROL_QUEUE_CAPACITY {
                return Err(RuntimeError::SerialControlBusy);
            }
            requests.push_back(ControlRequest::DrainTx(DrainCompletion {
                completion: completion.clone(),
                wait_for_hardware,
            }));
        }
        self.notify_worker(notify);
        completion.wait()
    }

    pub(super) fn try_pop(&self) -> Option<ControlRequest> {
        self.requests.lock_irqsave().pop_front()
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.requests.lock_irqsave().is_empty()
    }
}

struct CommandCompletion {
    result: Mutex<Option<RuntimeResult>>,
    wait: WaitQueue,
}

impl CommandCompletion {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: RuntimeResult) {
        *self.result.lock() = Some(result);
        self.wait.notify_all();
    }

    fn wait(&self) -> RuntimeResult {
        self.wait.wait_until(|| self.result.lock().is_some());
        self.result
            .lock()
            .take()
            .expect("serial command completion was published without a result")
    }
}
