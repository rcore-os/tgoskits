use alloc::{collections::VecDeque, sync::Arc};

use ax_task::{IrqNotify, WaitQueue};
use rdif_serial::Config;

use crate::{RuntimeError, RuntimeResult, sync::SpinLock};

pub(super) const CONTROL_QUEUE_CAPACITY: usize = 32;

pub(super) enum ControlOp {
    Start(Config),
    AdoptFirmwareConsole,
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
    requests: SpinLock<VecDeque<ControlRequest>>,
}

impl ControlQueue {
    pub(super) fn new() -> Self {
        Self {
            requests: SpinLock::new(VecDeque::with_capacity(CONTROL_QUEUE_CAPACITY)),
        }
    }

    pub(super) fn submit(&self, op: ControlOp, notify: &IrqNotify) -> RuntimeResult {
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
        notify.notify();
        completion.wait()
    }

    pub(super) fn submit_drain(&self, notify: &IrqNotify) -> RuntimeResult {
        let completion = Arc::new(CommandCompletion::new());
        {
            let mut requests = self.requests.lock_irqsave();
            if requests.len() == CONTROL_QUEUE_CAPACITY {
                return Err(RuntimeError::SerialControlBusy);
            }
            requests.push_back(ControlRequest::DrainTx(DrainCompletion {
                completion: completion.clone(),
            }));
        }
        notify.notify();
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
    result: SpinLock<Option<RuntimeResult>>,
    wait: WaitQueue,
}

impl CommandCompletion {
    fn new() -> Self {
        Self {
            result: SpinLock::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: RuntimeResult) {
        *self.result.lock_irqsave() = Some(result);
        self.wait.notify_all(true);
    }

    fn wait(&self) -> RuntimeResult {
        self.wait
            .wait_until(|| self.result.lock_irqsave().is_some());
        self.result
            .lock_irqsave()
            .take()
            .expect("serial command completion was published without a result")
    }
}
