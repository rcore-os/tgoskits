use alloc::{collections::VecDeque, sync::Arc};

use rdif_serial::Config;

use crate::{RuntimeError, RuntimeResult, sync::PiMutex, task::WaitQueue};

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
    pub(super) fn complete(self, result: RuntimeResult) {
        self.completion.complete(result);
    }
}

pub(super) struct ControlQueue {
    commands: PiMutex<VecDeque<ControlCommand>>,
}

impl ControlQueue {
    pub(super) fn new() -> Self {
        Self {
            commands: PiMutex::new(VecDeque::with_capacity(CONTROL_QUEUE_CAPACITY)),
        }
    }

    pub(super) fn submit(&self, op: ControlOp, notify: impl FnOnce()) -> RuntimeResult {
        let completion = Arc::new(CommandCompletion::new());
        {
            let mut commands = self.commands.lock();
            if commands.len() == CONTROL_QUEUE_CAPACITY {
                return Err(RuntimeError::SerialControlBusy);
            }
            commands.push_back(ControlCommand {
                op,
                completion: completion.clone(),
            });
        }
        notify();
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
    result: PiMutex<Option<RuntimeResult>>,
    wait: WaitQueue,
}

impl CommandCompletion {
    fn new() -> Self {
        Self {
            result: PiMutex::new(None),
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
