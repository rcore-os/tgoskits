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

use std::{
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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
pub(crate) struct WaitReason {
    /// Optional ABI result committed before parking; ordinary WFI/HLT preserves registers.
    pub(crate) return_value: Option<usize>,
}

/// All values used while hardware is loaded are prepared in task context.
pub(crate) struct ExecutionEntry<A: crate::architecture::ArchOps> {
    pub(crate) root: axvm_types::NestedPagingConfig,
    pub(crate) revision: crate::guest_memory::MemoryRevision,
    pub(crate) decode: Arc<crate::guest_memory::DecodeMemory>,
    pub(crate) architecture: Arc<A::Entry>,
    pub(crate) signals: Arc<crate::services::RunSignals>,
    pub(crate) admission: Arc<AtomicBool>,
}

/// The only generic hardware entry and exit implementation.
pub(crate) struct OwnedVcpuEngine<A: crate::architecture::ArchOps> {
    vcpu: crate::vcpu::AxVCpu<A::VCpu>,
}

impl<A: crate::architecture::ArchOps> OwnedVcpuEngine<A> {
    pub(crate) fn new(vcpu: crate::vcpu::AxVCpu<A::VCpu>) -> Self {
        Self { vcpu }
    }

    pub(crate) fn vcpu_mut(&mut self) -> &mut crate::vcpu::AxVCpu<A::VCpu> {
        &mut self.vcpu
    }

    pub(crate) fn into_backend(mut self) -> (crate::vcpu::AxVCpu<A::VCpu>, AxVmResult) {
        let result = self
            .vcpu
            .with_backend(A::quiet_vcpu)
            .and_then(|()| self.vcpu.unbind());
        (self.vcpu, result)
    }

    pub(crate) fn install_root(
        &mut self,
        entry: &mut ExecutionEntry<A>,
        root: axvm_types::NestedPagingConfig,
        revision: crate::guest_memory::MemoryRevision,
        decode: Arc<crate::guest_memory::DecodeMemory>,
    ) -> AxVmResult {
        // Root changes are committed through the normal backend binding. This
        // keeps architecture backends that mirror translation state in their
        // CPU-local control image synchronized before the owner publishes the
        // new revision.
        self.vcpu
            .with_engine_scope(&entry.decode, &entry.signals, |vcpu| {
                vcpu.set_nested_page_table(root)
            })?;
        entry.root = root;
        entry.revision = revision;
        entry.decode = decode;
        Ok(())
    }

    pub(crate) fn commit_only(
        &mut self,
        entry: &ExecutionEntry<A>,
        completion: A::Completion,
    ) -> AxVmResult {
        self.vcpu
            .with_engine_scope(&entry.decode, &entry.signals, |vcpu| {
                vcpu.with_backend(|backend| A::complete(backend, &entry.architecture, completion))
            })
    }
}

impl<A: crate::architecture::ArchOps> VcpuEngine for OwnedVcpuEngine<A> {
    type Entry = ExecutionEntry<A>;
    type Exit = A::Exit;
    type Completion = A::Completion;

    fn run_once(
        &mut self,
        entry: &Self::Entry,
        completion: Option<Self::Completion>,
        signals: &VcpuSignals,
    ) -> VmBackendResult<EngineOutcome<Self::Exit>> {
        use ax_std::os::arceos::guard::IrqSaveGuard;

        use crate::vcpu::VcpuRunResult;
        let attempt = (|| {
            debug_assert!(std::ptr::eq(signals, &*self.vcpu.run_state()));
            self.vcpu
                .with_backend(|backend| A::prepare_vcpu(backend, &entry.architecture))?;
            // Draining allocates its return buffer before CPU binding. Producers
            // publish canonical pending before a lock-free final recheck.
            let pending = entry.signals.drain(
                self.vcpu.id(),
                crate::task::current_task_context()
                    .expect("vCPU engine runs on its owning task")
                    .instance
                    .activation,
            );
            let outcome = self
                .vcpu
                .with_engine_scope(&entry.decode, &entry.signals, |vcpu| {
                    let vcpu_id = vcpu.id();
                    if let Some(completion) = completion {
                        vcpu.with_backend(|backend| {
                            A::complete(backend, &entry.architecture, completion)
                        })?;
                    }
                    for &interrupt in &pending {
                        match interrupt.into_virtual() {
                            Ok(interrupt) => vcpu.with_backend(|backend| {
                                A::inject_vcpu_interrupt(backend, interrupt)
                            })?,
                            Err(interrupt) => vcpu.with_backend(|backend| {
                                A::inject_arch_interrupt(
                                    backend,
                                    vcpu_id,
                                    &entry.architecture,
                                    interrupt,
                                )
                            })?,
                        }
                    }
                    // Completion and drained interrupts are committed to owned
                    // backend/canonical state even when migration cancels entry.
                    // Retrying must not lose either publication.
                    if !vcpu.with_backend(A::entry_cpu_is_ready) {
                        return Ok(EngineOutcome::Interrupted);
                    }
                    vcpu.with_backend(|backend| {
                        A::before_guest(backend, vcpu_id, &entry.architecture)
                    })?;
                    let irq = IrqSaveGuard::new();
                    let result = vcpu.run_loaded(|| {
                        !entry.admission.load(Ordering::Acquire)
                            || entry.signals.has_pending(vcpu_id)
                    });
                    drop(irq);
                    match result? {
                        VcpuRunResult::Retry | VcpuRunResult::ExitRequested => {
                            Ok(EngineOutcome::Interrupted)
                        }
                        VcpuRunResult::VmExit(exit) => vcpu.with_backend(|backend| {
                            A::capture_exit(backend, &entry.architecture, exit)
                                .map(EngineOutcome::Exit)
                        }),
                    }
                })?;
            match outcome {
                EngineOutcome::Exit(exit) => self
                    .vcpu
                    .with_backend(|backend| A::finish_exit(backend, &entry.architecture, exit))
                    .map(EngineOutcome::Exit),
                EngineOutcome::Interrupted => Ok(EngineOutcome::Interrupted),
            }
        })();
        attempt.map_err(|error| {
            error!("vCPU hardware attempt failed after context retirement: {error}");
            match error {
                crate::AxVmError::OutOfMemory { .. } => axvm_types::VmBackendError::OutOfMemory,
                _ => axvm_types::VmBackendError::InvalidState,
            }
        })
    }
}

pub(crate) struct ArchitectureExitHandler<A: crate::architecture::ArchOps> {
    vcpu_id: usize,
    architecture: PhantomData<fn() -> A>,
}

impl<A: crate::architecture::ArchOps> ArchitectureExitHandler<A> {
    pub(crate) const fn new(vcpu_id: usize) -> Self {
        Self {
            vcpu_id,
            architecture: PhantomData,
        }
    }
}

impl<A: crate::architecture::ArchOps> ExitHandler<OwnedVcpuEngine<A>>
    for ArchitectureExitHandler<A>
{
    type Request = crate::runtime::hvc::GuestRequest;

    fn handle(
        &mut self,
        exit: A::Exit,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<A::Completion, Self::Request>> {
        A::handle_exit(exit, self.vcpu_id, services)
    }
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
    pub(crate) engine: E,
    pub(crate) entry: E::Entry,
    pub(crate) completion: Option<E::Completion>,
    pub(crate) exits: H,
    pub(crate) services: Arc<RunServices>,
    signals: Arc<VcpuSignals>,
}

impl<E: VcpuEngine, H: ExitHandler<E>> VcpuTask<E, H> {
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

    pub(crate) fn run_once(&mut self) -> VmBackendResult<EngineOutcome<E::Exit>> {
        self.engine
            .run_once(&self.entry, self.completion.take(), &self.signals)
    }
}
