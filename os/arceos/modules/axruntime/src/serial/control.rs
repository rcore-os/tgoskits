use alloc::{collections::VecDeque, sync::Arc};

use ax_errno::{AxError, AxResult};
use ax_sync::SpinLock;
use ax_task::{IrqNotify, WaitQueue};
use rdif_serial::Config;

pub(super) const CONTROL_QUEUE_CAPACITY: usize = 32;

pub(super) enum ControlOp {
    Start(Config),
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
    pub(super) fn complete(self, result: AxResult) {
        self.completion.complete(result);
    }
}

pub(super) struct ControlQueue {
    commands: SpinLock<VecDeque<ControlCommand>>,
}

impl ControlQueue {
    pub(super) fn new() -> Self {
        Self {
            commands: SpinLock::new(VecDeque::with_capacity(CONTROL_QUEUE_CAPACITY)),
        }
    }

    pub(super) fn submit(&self, op: ControlOp, notify: &IrqNotify) -> AxResult {
        let completion = Arc::new(CommandCompletion::new());
        {
            let mut commands = self.commands.lock_irqsave();
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
        self.commands.lock_irqsave().pop_front()
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.commands.lock_irqsave().is_empty()
    }
}

struct CommandCompletion {
    result: SpinLock<Option<AxResult>>,
    wait: WaitQueue,
}

impl CommandCompletion {
    fn new() -> Self {
        Self {
            result: SpinLock::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: AxResult) {
        *self.result.lock_irqsave() = Some(result);
        self.wait.notify_all(true);
    }

    fn wait(&self) -> AxResult {
        self.wait
            .wait_until(|| self.result.lock_irqsave().is_some());
        self.result
            .lock_irqsave()
            .take()
            .expect("serial command completion was published without a result")
    }
}
