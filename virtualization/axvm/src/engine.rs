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

//! Architecture-independent vCPU execution engine boundary.
//!
//! One [`VcpuTask`] exclusively owns one [`VcpuEngine`]. The engine performs a
//! single guest-entry attempt and returns either a backend exit or an
//! interruption that cancelled the attempt. The task's [`ExitHandler`]
//! interprets that exit outside the guest-entry window and selects the next
//! [`VcpuAction`]. Shared runtime services and the fixed [`VcpuSignals`] target
//! are borrowed by the task; the architecture backend is never published.

use std::sync::Arc;

use axvm_types::VmBackendResult;

use crate::{AxVmResult, StopReason, services::RunServices, vcpu::VcpuSignals};

/// Result of one guest-entry attempt.
pub(crate) enum EngineOutcome<E> {
    /// The guest ran and the backend produced one VM exit.
    Exit(E),
    /// An entry request cancelled the attempt before the guest ran.
    Interrupted,
}

/// Why a vCPU task leaves the guest-entry loop and parks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitReason {
    /// No guest work is currently pending; park until an external event.
    Idle,
    /// The vCPU must wait for a lifecycle transition to complete.
    Lifecycle,
}

/// Next action selected after one backend exit has been interpreted.
pub(crate) enum VcpuAction<C, R> {
    /// Commit `C` as the next completion and run the guest again.
    Reenter(C),
    /// Leave the guest-entry loop and park the task.
    Wait(WaitReason),
    /// Return a control-plane value to the task owner.
    Control(R),
    /// The guest powered this vCPU off (PSCI CPU_OFF).
    CpuOff,
    /// The whole VM must stop.
    Stop(StopReason),
}

/// Owns the architecture entry/exit translation for one vCPU task.
///
/// `run_once` performs exactly one guest-entry attempt: it publishes the guest
/// mode through `signals`, performs the final entry-request check, and returns
/// either a backend exit or an interruption that cancelled the entry before the
/// guest ran. The producer-publish/paired-barrier/final-check protocol lives in
/// this method and must not be reimplemented by callers.
pub(crate) trait VcpuEngine: Send {
    /// Per-entry guest payload consumed before the guest runs.
    type Entry: Send;
    /// Backend VM exit produced by one completed guest entry.
    type Exit: Send;
    /// Backend completion committed before the next guest entry.
    type Completion: Send;

    fn run_once(
        &mut self,
        entry: &Self::Entry,
        completion: Option<Self::Completion>,
        signals: &VcpuSignals,
    ) -> VmBackendResult<EngineOutcome<Self::Exit>>;
}

/// Interprets one backend exit outside the guest-entry window.
///
/// The handler runs after the backend has released its per-entry binding and
/// may therefore use sleepable runtime and device services. It never owns the
/// architecture backend; it only selects the next [`VcpuAction`].
pub(crate) trait ExitHandler<E: VcpuEngine> {
    /// Upper-layer control value returned by [`VcpuAction::Control`].
    type Request: Send;

    fn handle(
        &mut self,
        exit: E::Exit,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<E::Completion, Self::Request>>;
}

/// Task-owned vCPU execution state.
///
/// The task exclusively owns the engine together with the entry payload, the
/// pending completion, and the exit interpreter. Shared runtime services and
/// the fixed signal target are held as `Arc`s. The engine is never published
/// through CPU-local state; only the identity-only execution context is.
pub(crate) struct VcpuTask<E: VcpuEngine, H: ExitHandler<E>> {
    engine: E,
    entry: E::Entry,
    completion: Option<E::Completion>,
    exits: H,
    services: Arc<RunServices>,
    signals: Arc<VcpuSignals>,
}

impl<E: VcpuEngine, H: ExitHandler<E>> VcpuTask<E, H> {
    /// Creates a task that owns `engine` and interprets its exits with `exits`.
    pub(crate) fn new(
        engine: E,
        entry: E::Entry,
        exits: H,
        services: Arc<RunServices>,
        signals: Arc<VcpuSignals>,
    ) -> Self {
        Self {
            engine,
            entry,
            completion: None,
            exits,
            services,
            signals,
        }
    }

    /// Runs one guest-entry attempt with the task-owned engine and payload.
    pub(crate) fn run_once(&mut self) -> VmBackendResult<EngineOutcome<E::Exit>> {
        let completion = self.completion.take();
        self.engine.run_once(&self.entry, completion, &self.signals)
    }

    /// Returns the task-owned execution engine.
    pub(crate) fn engine(&self) -> &E {
        &self.engine
    }

    /// Returns the task-owned execution engine for mutation.
    pub(crate) fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }

    /// Returns the per-entry guest payload.
    pub(crate) fn entry(&self) -> &E::Entry {
        &self.entry
    }

    /// Returns the completion committed before the next guest entry.
    pub(crate) fn completion(&self) -> Option<&E::Completion> {
        self.completion.as_ref()
    }

    /// Sets the completion committed before the next guest entry.
    pub(crate) fn set_completion(&mut self, completion: E::Completion) {
        self.completion = Some(completion);
    }

    /// Returns the exit interpreter.
    pub(crate) fn exits_mut(&mut self) -> &mut H {
        &mut self.exits
    }

    /// Returns the shared runtime services.
    pub(crate) fn services(&self) -> &Arc<RunServices> {
        &self.services
    }

    /// Returns the fixed signal target of this task.
    pub(crate) fn signals(&self) -> &Arc<VcpuSignals> {
        &self.signals
    }
}
