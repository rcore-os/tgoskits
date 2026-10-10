// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Task-side owner of the deferred run-signal worker.
//!
//! Hard IRQ handlers reach only [`RunSignals`]. This object owns the worker join
//! handle as an ordinary owner value: it is neither embedded in [`RunSignals`]
//! nor guarded by a lock, so no guest-entry or hard-IRQ path can reach it.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{AxVmError, AxVmResult, ThreadHandle, host::task, services::RunSignals};

const SIGNAL_WORKER_STACK_SIZE: usize = 0x10000;

/// The sole task-context worker for one run's deferred IRQ kicks.
pub(crate) struct RunSignalWorker {
    signals: Arc<RunSignals>,
    stopping: Arc<AtomicBool>,
    worker: Option<ThreadHandle>,
}

impl RunSignalWorker {
    /// Starts the fixed worker for `signals`.
    ///
    /// The returned object is an ordinary owner value. Its only shared state
    /// with the worker is the stop flag, so no lock is reachable from either the
    /// hard-IRQ path or the guest-entry path.
    pub(crate) fn start(signals: Arc<RunSignals>) -> AxVmResult<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_signals = Arc::clone(&signals);
        let worker_stopping = Arc::clone(&stopping);
        let worker = task::builder(std::format!(
            "VM[{}:run:{}]-signal-worker",
            signals.run_id().vm().vm_id(),
            signals.run_id().generation()
        ))
        .stack_size(SIGNAL_WORKER_STACK_SIZE)
        .spawn(move || run_worker(worker_signals, worker_stopping))
        .map_err(|error| AxVmError::host("start run signal worker", error))?;
        Ok(Self {
            signals,
            stopping,
            worker: Some(worker),
        })
    }

    /// Requests exit and joins the worker outside every lock.
    ///
    /// The join runs on a clone of the owned handle, so a failed join keeps the
    /// only owned handle for a later retry instead of consuming it. Only a
    /// successful join retires the stored handle.
    pub(crate) fn stop(&mut self) -> AxVmResult {
        self.stopping.store(true, Ordering::Release);
        self.signals.notify_irq_worker();
        let Some(worker) = self.worker.as_ref() else {
            self.signals.stop_irq_pending_publication();
            return Ok(());
        };
        match worker.clone().join() {
            Ok(_) => {
                self.worker = None;
                self.signals.stop_irq_pending_publication();
                Ok(())
            }
            // Keep `self.worker` so the run owner may call `stop` again.
            Err(error) => Err(AxVmError::host("join run signal worker", error)),
        }
    }
}

fn run_worker(signals: Arc<RunSignals>, stopping: Arc<AtomicBool>) {
    loop {
        signals.wait_for_irq_notification();
        if stopping.load(Ordering::Acquire) {
            break;
        }
        signals.post_controller_events();
        signals.kick_pending_for_worker();
    }
}
