use alloc::{collections::VecDeque, sync::Arc};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_task::{IrqNotify, WaitQueue};
use rdif_serial::Config;

pub(super) const CONTROL_QUEUE_CAPACITY: usize = 32;

pub(super) enum ControlOp {
    Start(Config),
    Shutdown,
    SetConfig(Config),
}

pub(super) struct ControlCommand {
    pub(super) op: ControlOp,
    completion: Arc<CommandCompletion>,
}

impl ControlCommand {
    pub(super) fn complete(self, result: AxResult) {
        self.completion.complete(result);
    }
}

pub(super) struct ControlQueue {
    commands: SpinNoIrq<VecDeque<ControlCommand>>,
}

impl ControlQueue {
    pub(super) fn new() -> Self {
        Self {
            commands: SpinNoIrq::new(VecDeque::with_capacity(CONTROL_QUEUE_CAPACITY)),
        }
    }

    pub(super) fn submit(&self, op: ControlOp, notify: &IrqNotify) -> AxResult {
        let completion = Arc::new(CommandCompletion::new());
        {
            let mut commands = self.commands.lock();
            if commands.len() == CONTROL_QUEUE_CAPACITY {
                return Err(AxError::ResourceBusy);
            }
            commands.push_back(ControlCommand {
                op,
                completion: completion.clone(),
            });
        }
        notify.notify();
        completion.wait()
    }

    pub(super) fn try_pop(&self) -> Option<ControlCommand> {
        self.commands.lock().pop_front()
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.commands.lock().is_empty()
    }
}

struct CommandCompletion {
    result: SpinNoIrq<Option<AxResult>>,
    wait: WaitQueue,
}

impl CommandCompletion {
    fn new() -> Self {
        Self {
            result: SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: AxResult) {
        *self.result.lock() = Some(result);
        self.wait.notify_all(true);
    }

    fn wait(&self) -> AxResult {
        self.wait.wait_until(|| self.result.lock().is_some());
        self.result
            .lock()
            .take()
            .expect("serial command completion was published without a result")
    }
}
