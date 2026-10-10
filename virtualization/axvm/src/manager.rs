//! Instance-owned VM registry and single-owner control frontend.
//!
//! [`VmManager`] owns one registry per manager instance. There is no
//! process-wide registry and no global "current VM" lookup: every command and
//! every internal observation is delivered to the control task that uniquely
//! owns one instance's lifecycle, resource transitions and current operation.
//!
//! This module contains only the management frontend: the public handle, the
//! per-instance registry, the mailbox and message vocabulary, the observation
//! snapshot, and the control-task exit observation. The control owner itself
//! lives in [`crate::control`], which receives one [`VmCreatePlan`] as its
//! initial argument and drives creation, lifecycle and teardown.
//!
//! Resource creation and image loading run on the control owner, never on the
//! caller. `VmManager::create` only reserves the instance generation and
//! registers the reservation; the owner initializes the machine and publishes
//! the `Ready` snapshot before its create operation completes with a handle.
//!
//! A successful `destroy` is witnessed by control-task exit. The owner hands
//! destroy completions to a [`ControlExit`] extension; the scheduler invokes the
//! extension's `on_exit` callback in ordinary task-work context after the owner
//! entry has returned and the machine resources have been released. `on_exit`
//! removes the exact `VmKey` from the registry and then publishes every pending
//! completion. Because the callback runs before the scheduler marks the thread
//! exited, `on_exit` must never `join`/`wait` its own thread.
//!
//! Each reservation carries an explicit task publication state: `Preparing`
//! until the control task is staged, `Published(handle)` once it is joinable, or
//! `Failed(error)` when staging never produced a task. Join waiters and
//! `shutdown` block on that state instead of guessing that a reservation is
//! joined. A staging failure closes the exact entry, rejects every queued
//! command and guest request, completes every destroy observer with the explicit
//! failure through the exit observation, and only then removes the exact
//! reservation; a destroy is never left hanging or reported as a success.

use std::{
    collections::{BTreeMap, VecDeque},
    string::{String, ToString},
    sync::{
        Arc, Condvar, Mutex, OnceLock, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use axvm_types::VMId;

use crate::{
    AxVmError, AxVmResult, GuestPhysAddr, HostPhysAddr, OperationId, RunId, StopReason,
    ThreadHandle, VmKey, VmOperation, VmStatus,
    boot::{BootImageProvider, PreparedGuestBoot},
    config::{
        AddressSpacePolicy, AxVMConfig, HostAddressAssignment, HostDeviceAssignment,
        PassthroughInterrupt, VMImageConfig,
    },
    guest_memory::{GuestMemoryPort, MemoryRevision, MemoryUpdate},
    host::{
        HostPlatform, default_host,
        task::{SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadId},
    },
    identity::VcpuInstance,
    operation::OperationCompletion,
    runtime::{
        hvc::GuestRequest,
        vcpus::{VcpuEvent, VcpuProgress},
    },
    services::{RunServices, VcpuInterruptPort},
    sync::MutexExt,
    vm::{VMMemoryRegion, VcpuSnapshot},
};

/// Everything needed to create one VM instance.
///
/// The caller prepares the guest boot description and image provider. `create`
/// reserves the instance identifier; the control owner then initializes
/// resources and loads images before publishing a `Ready` handle.
pub struct VmCreatePlan {
    /// Architecture construction configuration.
    pub config: AxVMConfig,
    /// Architecture-prepared guest boot description for `config`.
    pub boot: PreparedGuestBoot,
    /// Shared boot-image provider used to load the configured guest images.
    pub images: Arc<dyn BootImageProvider + Send + Sync>,
    /// Immutable host scheduling policy applied to every vCPU owner task.
    pub vcpu_schedule_policy: crate::SchedulePolicy,
}

/// Owned observation of the static VM configuration.
///
/// The values are copied once by the control owner; a consumer never receives
/// the whole VM, a configuration-mutex closure, or a getter that can reach live
/// state. Addresses are observation values only and hold no reference.
#[derive(Clone, Debug)]
pub struct VmConfigSnapshot {
    /// Bootstrap-processor entry address in guest physical memory.
    pub bsp_entry: GuestPhysAddr,
    /// Application-processor entry address in guest physical memory.
    pub ap_entry: GuestPhysAddr,
    /// Guest image load configuration.
    pub image: VMImageConfig,
    /// Guest physical address-space population policy.
    pub address_space_policy: AddressSpacePolicy,
    /// Per-vCPU physical-placement tuples `(vcpu_id, affinity_mask, phys_id)`.
    pub vcpu_affinities: Vec<(usize, Option<usize>, usize)>,
    /// Passthrough host devices.
    pub passthrough_devices: Vec<HostDeviceAssignment>,
    /// Passthrough host address assignments.
    pub passthrough_addresses: Vec<HostAddressAssignment>,
    /// Physical interrupt sources forwarded to the guest.
    pub passthrough_irqs: Vec<PassthroughInterrupt>,
}

/// One strongly-typed observation of a VM instance.
///
/// A snapshot is a point-in-time copy of the owner's observations. It never
/// borrows the machine, a backend, or a device, so a consumer in another
/// context can read it without reaching the owner.
#[derive(Clone, Debug)]
pub struct VmSnapshot {
    /// Instance identity (VM ID plus instance generation).
    pub key: VmKey,
    /// Numeric identifier used by configuration and the guest ABI.
    pub vm_id: VMId,
    /// Configured VM name.
    pub name: String,
    /// Current lifecycle status.
    pub state: VmStatus,
    /// Current execution generation, when the VM has entered a run at least once.
    pub run: Option<RunId>,
    /// Operation currently owned by the control task, if any.
    pub current_operation: Option<OperationId>,
    /// Last failure observed by the control task, retained for diagnosis.
    pub last_failure: Option<AxVmError>,
    /// Reason recorded by the owner for the most recent stop.
    pub last_stop_reason: Option<StopReason>,
    /// CPU observation.
    pub cpu: CpuObservation,
    /// Memory observation.
    pub memory: MemoryObservation,
    /// Device observation.
    pub device: DeviceObservation,
    /// Per-vCPU observation.
    pub vcpu: Vec<VcpuSnapshot>,
    /// Static configuration observation.
    pub description: VmConfigSnapshot,
    /// Aggregate guest (re-)entry count for the current run.
    pub entry_count: u64,
    /// Aggregate guest-park count for the current run.
    pub park_count: u64,
}

/// Lifecycle and its exact activation counters share one publication boundary.
/// Counters have no command, task or backend capability; an old observation
/// cannot sample a later run that reused the same numeric VM identifier.
#[derive(Clone)]
struct PublishedSnapshot {
    value: VmSnapshot,
    progress: Vec<Arc<VcpuProgress>>,
}

impl PublishedSnapshot {
    fn sample(self) -> VmSnapshot {
        let mut value = self.value;
        for progress in self.progress {
            value.entry_count = value
                .entry_count
                .saturating_add(progress.entries.load(Ordering::Relaxed));
            value.park_count = value
                .park_count
                .saturating_add(progress.parks.load(Ordering::Relaxed));
        }
        value
    }
}

/// CPU observation values for one VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuObservation {
    /// Number of configured vCPUs.
    pub vcpu_num: usize,
    /// Number of initialized vCPU owners that have not exited.
    pub running_vcpu_count: usize,
}

/// Memory observation values for one VM.
#[derive(Clone, Debug)]
pub struct MemoryObservation {
    /// Current nested page-table root, when the VM has a backing address space.
    pub nested_page_table_root: Option<HostPhysAddr>,
    /// Total size of the guest memory regions in bytes.
    pub total_bytes: usize,
    /// Owned copies of the current guest memory regions.
    pub regions: Vec<VMMemoryRegion>,
}

/// Device observation values for one VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceObservation {
    /// Number of prepared emulated devices.
    pub device_count: usize,
}

/// An owned handle to one registered VM instance.
///
/// Cloning a handle shares the same control endpoints. A handle can observe and
/// command the instance; it cannot pierce the lifecycle, reach a backend, or
/// release resources directly. Dropping a handle does not destroy the VM.
#[derive(Clone)]
pub struct VmHandle {
    shared: Arc<ControlShared>,
}

impl VmHandle {
    /// Returns the exact instance identity (VM ID plus instance generation).
    pub fn key(&self) -> VmKey {
        self.shared.key()
    }

    /// Returns the numeric VM identifier.
    pub fn vm_id(&self) -> VMId {
        self.shared.key().vm_id()
    }

    /// Returns owner-published lifecycle data with freshly sampled run progress.
    ///
    /// A handle is only handed out after the owner published its first snapshot,
    /// so this always observes a fully initialized instance.
    pub fn snapshot(&self) -> VmSnapshot {
        let published = self
            .shared
            .snapshot
            .lock_unpoisoned()
            .clone()
            .expect("a ready handle always retains a published snapshot");
        published.sample()
    }

    /// Starts the guest and returns the new execution generation on success.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed or the operation
    /// sequence is exhausted.
    pub fn start(&self) -> AxVmResult<VmOperation<RunId>> {
        let (operation, completion) = self.shared.new_operation()?;
        self.shared.dispatch_command(Command::Start(completion))?;
        Ok(operation)
    }

    /// Pauses the running guest, or returns immediately when already paused.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed or the operation
    /// sequence is exhausted.
    pub fn pause(&self) -> AxVmResult<VmOperation<()>> {
        let (operation, completion) = self.shared.new_operation()?;
        self.shared.dispatch_command(Command::Pause(completion))?;
        Ok(operation)
    }

    /// Resumes a paused guest, or returns immediately when already running.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed or the operation
    /// sequence is exhausted.
    pub fn resume(&self) -> AxVmResult<VmOperation<()>> {
        let (operation, completion) = self.shared.new_operation()?;
        self.shared.dispatch_command(Command::Resume(completion))?;
        Ok(operation)
    }

    /// Requests a stop with the given reason.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed or the operation
    /// sequence is exhausted.
    pub fn stop(&self, reason: StopReason) -> AxVmResult<VmOperation<()>> {
        let (operation, completion) = self.shared.new_operation()?;
        self.shared
            .dispatch_command(Command::Stop(reason, completion))?;
        Ok(operation)
    }

    /// Resets the guest to a fresh execution generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed or the operation
    /// sequence is exhausted.
    pub fn reset(&self) -> AxVmResult<VmOperation<RunId>> {
        let (operation, completion) = self.shared.new_operation()?;
        self.shared.dispatch_command(Command::Reset(completion))?;
        Ok(operation)
    }

    /// Destroys the instance and unregisters it on success.
    ///
    /// The destroy completion is published by the control task's exit callback,
    /// so a successful result is a witness that the control task returned and
    /// the machine resources were released. Repeated destroy observations of the
    /// same instance merge into that single exit observation, including the
    /// window between closing the entry and running the exit callback. The
    /// caller joins the control task through [`Self::join_control_task`], never
    /// from the owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed (for a command other
    /// than destroy) or the operation sequence is exhausted.
    pub fn destroy(&self) -> AxVmResult<VmOperation<()>> {
        // Every observer goes through the exit observation. The owner's final
        // snapshot may already say Destroyed while its task still holds resources.
        let (operation, completion) = self.shared.new_operation()?;
        self.shared.dispatch_command(Command::Destroy(completion))?;
        Ok(operation)
    }

    /// Wakes device polling for the current run.
    ///
    /// The call binds to the run-owned [`DeviceWorkPort`]; it never resolves a VM
    /// by a bare numeric ID. The port is only available while a run exists.
    ///
    /// # Errors
    ///
    /// Returns an error when no run currently owns a device-notification port,
    /// or when that run's port cannot signal its poll owner.
    pub fn notify_devices(&self) -> AxVmResult<()> {
        let services = self
            .shared
            .run_services
            .lock_unpoisoned()
            .clone()
            .ok_or_else(|| {
                AxVmError::resource_unavailable("VM device notification", "no active run")
            })?;
        services.signals().notify_work().map_err(Into::into)
    }

    /// Returns a copy-only RAM port for an exact run. Retired ports remain
    /// closed; each access pins one mapping revision until the copy completes.
    pub fn guest_memory(&self, expected_run: RunId) -> AxVmResult<GuestMemoryPort> {
        Ok(self.bound_services(expected_run)?.memory())
    }

    /// Binds a virtual edge source without publishing a mutable vCPU backend.
    pub fn interrupt_port(
        &self,
        expected_run: RunId,
        vcpu_id: usize,
        vector: u32,
    ) -> AxVmResult<VcpuInterruptPort> {
        let services = self.bound_services(expected_run)?;
        VcpuInterruptPort::new(services.signals().clone(), vcpu_id, vector)
    }

    fn bound_services(&self, expected_run: RunId) -> AxVmResult<Arc<RunServices>> {
        let services = self.shared.run_services.lock_unpoisoned().clone();
        match services {
            Some(services) if services.run_id() == expected_run => Ok(services),
            other => Err(AxVmError::StaleRun {
                expected: expected_run,
                current: other.map(|services| services.run_id()),
            }),
        }
    }

    /// Applies one guest-memory translation update to an exact run.
    ///
    /// The owner validates that the current run matches `expected_run`; a stale
    /// request is rejected by the owner, not by the dispatch path.
    pub fn update_memory(
        &self,
        expected_run: RunId,
        update: MemoryUpdate,
    ) -> AxVmResult<VmOperation<MemoryRevision>> {
        let (operation, completion) = self.shared.new_operation()?;
        self.shared.dispatch_command(Command::UpdateMemory {
            expected_run,
            update,
            completion,
        })?;
        Ok(operation)
    }

    /// Joins the control task after destroy, if it has not been joined.
    ///
    /// Concurrent observers share one join: the first caller claims the join and
    /// releases the state mutex before blocking, and every waiter observes the
    /// same final result. While the reservation is still `Preparing` this waits
    /// for the staging result instead of reporting a task that may stage later.
    /// It must never run inside the control task itself.
    ///
    /// # Errors
    ///
    /// Returns the exact creation failure when the control task never staged, or
    /// the underlying join error when the control task cannot be joined.
    pub fn join_control_task(&self) -> AxVmResult<()> {
        let shared = &self.shared;
        let mut state = shared.task.lock_unpoisoned();
        loop {
            match &state.join {
                JoinProgress::Completed => return state.result.clone().unwrap_or(Ok(())),
                JoinProgress::Joining(attempt) => {
                    let attempt = attempt.clone();
                    drop(state);
                    return attempt.wait();
                }
                JoinProgress::Idle => {}
            }
            match &state.publication {
                TaskPublication::Failed(error) => return Err(error.clone()),
                TaskPublication::Preparing => {
                    // The task has not staged yet: wait for its publication
                    // rather than reporting an already-joined task. The waiter
                    // releases the mutex here.
                    state = shared
                        .task_ready
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                    continue;
                }
                TaskPublication::Published(_) => {}
                TaskPublication::Retired => unreachable!("retired task has a completed join"),
            }

            // Claim the single join and release the mutex before blocking. The
            // `join` state now records the claim, so the publication only needs
            // to remember that the task existed.
            let attempt = Arc::new(JoinAttempt::new());
            state.join = JoinProgress::Joining(attempt.clone());
            let TaskPublication::Published(handle) = &state.publication else {
                unreachable!("publication is Published while holding the task mutex")
            };
            let handle = handle.clone();
            drop(state);

            // Join with the state mutex released; the owner never waits on it.
            let result = handle
                .join()
                .map(|_| ())
                .map_err(|error| AxVmError::host("join VM control task", error));

            let mut state = shared.task.lock_unpoisoned();
            let retired = result
                .is_ok()
                .then(|| std::mem::replace(&mut state.publication, TaskPublication::Retired));
            state.join = if result.is_ok() {
                JoinProgress::Completed
            } else {
                // Preserve the original management lease for a later retry.
                JoinProgress::Idle
            };
            state.result = Some(result.clone());
            drop(state);
            drop(retired);
            attempt.finish(result.clone());
            return result;
        }
    }
}

/// Domain state of one manager's instance registry.
///
/// Identifier reservation, duplicate detection, the create-entry close flag and
/// the per-identifier generation counter live here so `create` and `shutdown`
/// are each single transactions under one mutex. There is no separate close flag
/// that could race with a snapshot of the entries.
struct RegistryState {
    entries: BTreeMap<VMId, Instance>,
    generations: BTreeMap<VMId, u64>,
    closed: bool,
}

/// One registered instance, keyed by the VM identifier used by the guest ABI.
struct Instance {
    key: VmKey,
    shared: Arc<ControlShared>,
}

/// AxVM-owned registry and control plane for one hypervisor instance.
pub struct VmManager {
    /// One std sleepable mutex guards the whole registry domain state.
    registry: Arc<std::sync::Mutex<RegistryState>>,
    ivc: Arc<crate::runtime::ivc::IvcManager>,
}

// Identifiers must also distinguish independently instantiated managers, because
// the host's hardware IRQ source slots are shared even though registries are not.
static VM_INSTANCE_GENERATION: AtomicU64 = AtomicU64::new(0);
static HOST_VIRTUALIZATION: OnceLock<AxVmResult> = OnceLock::new();

impl VmManager {
    /// Creates a manager backed by the default ArceOS host.
    ///
    /// # Errors
    ///
    /// Returns an error when the host lacks hardware virtualization support or
    /// host virtualization cannot be enabled on every CPU.
    pub fn new() -> AxVmResult<Self> {
        let host = default_host();
        if !host.has_hardware_support() {
            return Err(AxVmError::Unsupported {
                operation: "create VM manager",
                detail: "hardware virtualization is not supported".to_string(),
            });
        }
        HOST_VIRTUALIZATION
            .get_or_init(|| host.enable_virtualization_on_all_cpus())
            .clone()?;
        Ok(Self {
            ivc: Arc::new(crate::runtime::ivc::IvcManager::new()),
            registry: Arc::new(std::sync::Mutex::new(RegistryState {
                entries: BTreeMap::new(),
                generations: BTreeMap::new(),
                closed: false,
            })),
        })
    }

    /// Creates a VM instance and returns an operation that resolves to a
    /// `Ready` handle.
    ///
    /// The instance identifier and generation are reserved in one registry
    /// transaction that also rejects a closed create entry and a duplicate
    /// identifier. Resource creation, image loading and the `Ready` publication
    /// run on the control owner; the returned operation completes with the
    /// handle only after the owner has published the `Ready` snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is already reserved, the create
    /// entry is closed, the instance generation is exhausted, or the control
    /// task cannot be spawned. Failures during resource initialization are
    /// reported by the returned operation.
    pub fn create(&self, plan: VmCreatePlan) -> AxVmResult<VmOperation<VmHandle>> {
        let vm_id = plan.config.id();

        let (shared, operation, completion) = {
            let mut registry = self.registry.lock_unpoisoned();
            if registry.closed {
                let vm = VmKey::new(
                    vm_id,
                    registry.generations.get(&vm_id).copied().unwrap_or(0),
                );
                return Err(AxVmError::EntryClosed { vm });
            }
            if registry.entries.contains_key(&vm_id) {
                return Err(AxVmError::resource_conflict(
                    "VM instance identifier",
                    std::format!("VM {vm_id} is already registered"),
                ));
            }
            // The atomic only allocates a unique number; registry publication
            // establishes visibility of the corresponding instance state.
            let generation = VM_INSTANCE_GENERATION
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| {
                    AxVmError::resource_unavailable(
                        "VM instance generation",
                        "identity generation exhausted",
                    )
                })?
                + 1;
            registry.generations.insert(vm_id, generation);
            let key = VmKey::new(vm_id, generation);
            let exit = Arc::new(ControlExit::new(key, Arc::downgrade(&self.registry)));
            let shared = ControlShared::new(key, exit, self.ivc.clone());
            let (operation, completion) = shared.new_operation()?;
            registry.entries.insert(
                vm_id,
                Instance {
                    key,
                    shared: shared.clone(),
                },
            );
            (shared, operation, completion)
        };

        if let Err(error) = Self::spawn_control_task(&shared, plan, completion) {
            // The reservation never produced a control task. Publish the
            // explicit creation failure: close the exact command entry, reject
            // every queued command and guest request, complete every destroy
            // observer with this failure instead of hanging or reporting a
            // fabricated success, and only then remove the exact reservation.
            // The teardown runs outside the registry lock.
            shared.fail_creation(&error);
            return Err(error);
        }

        Ok(operation)
    }

    /// Returns the handle for one numeric VM identifier, if it is registered and
    /// has reached `Ready`.
    pub fn get(&self, vm_id: VMId) -> Option<VmHandle> {
        // Read the snapshot outside the registry lock.
        let shared = {
            self.registry
                .lock_unpoisoned()
                .entries
                .get(&vm_id)
                .map(|instance| instance.shared.clone())
        };
        shared.and_then(|shared| shared.ready_handle())
    }

    /// Returns handles for every `Ready` VM, ordered by VM identifier.
    pub fn list(&self) -> Vec<VmHandle> {
        let shareds: Vec<Arc<ControlShared>> = {
            self.registry
                .lock_unpoisoned()
                .entries
                .values()
                .map(|instance| instance.shared.clone())
                .collect()
        };
        shareds
            .iter()
            .filter_map(|shared| shared.ready_handle())
            .collect()
    }

    /// Stops the create entry, destroys every registered VM, and joins their
    /// control tasks.
    ///
    /// Closing the create entry and snapshotting every instance (including a
    /// reservation whose create is still in flight) happen in one registry
    /// transaction. All destroy requests are dispatched before any wait, and no
    /// registry, mailbox, or snapshot lock is held while waiting or joining. A VM
    /// whose destroy failed keeps its resources and registration so the caller
    /// can retry; the first failure is returned after every request has been
    /// observed.
    pub fn shutdown(&self) -> AxVmResult<()> {
        let handles: Vec<VmHandle> = {
            let mut registry = self.registry.lock_unpoisoned();
            registry.closed = true;
            registry
                .entries
                .values()
                .map(|instance| instance.shared.handle())
                .collect()
        };

        let mut pending = Vec::new();
        let mut first_error = None;
        for handle in handles {
            match handle.destroy() {
                Ok(operation) => pending.push((handle, operation)),
                Err(error) => {
                    // The destroy command could not be accepted. That is only
                    // acceptable when the exact instance already reached a
                    // terminal state; the evidence is the task publication or a
                    // completed join, never an ignored error. A failed creation
                    // leaves no registration, while a still-live resource keeps
                    // its registration and is reported so the caller can retry.
                    let terminal = handle.shared.terminal_outcome();
                    let recorded = match terminal {
                        Some(Ok(())) => None,
                        Some(Err(terminal)) => Some(terminal),
                        None => Some(error),
                    };
                    if first_error.is_none() {
                        first_error = recorded;
                    }
                }
            }
        }

        for (handle, operation) in pending {
            match operation.wait() {
                Ok(()) => {
                    if let Err(error) = handle.join_control_task()
                        && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn spawn_control_task(
        shared: &Arc<ControlShared>,
        plan: VmCreatePlan,
        completion: OperationCompletion<VmHandle>,
    ) -> AxVmResult<()> {
        let name = std::format!("VM[{}]-control", shared.key().vm_id());

        // The extension owns a strong reference to the exit state; the task entry
        // holds its own. The extension data is one uniquely owned
        // `Box<Arc<ControlExit>>`, released by the extension drop callback.
        let exit_for_task = shared.exit.clone();
        let exit_data = Box::into_raw(Box::new(shared.exit.clone())) as usize;
        // SAFETY: `exit_data` is exactly one `Box<Arc<ControlExit>>`, and the
        // extension's `on_exit`/`drop` callbacks interpret it as such.
        let extension = unsafe { ThreadExtension::new(exit_data, &CONTROL_EXIT_OPS) };

        // The task entry takes its own strong reference; the scheduling thread
        // keeps the helper reference so the join slot can be published before
        // activation.
        let shared_for_task = shared.clone();
        let builder = crate::host::task::builder(name)
            .stack_size(CONTROL_STACK_SIZE)
            .extension(extension);
        let prepared = builder
            .prepare(move || crate::control::run(shared_for_task, exit_for_task, plan, completion))
            .map_err(|error| AxVmError::host("prepare VM control task", error))?;
        let staged = prepared
            .stage()
            .map_err(|error| AxVmError::host("stage VM control task", error))?;

        // Publish the joinable handle before activation so a concurrent observer
        // never observes an unpublished task, then commit the first activation.
        // Waiters are woken outside the publication mutex by `publish_task`.
        shared.publish_task(staged.thread_handle());
        staged.activate().detach();
        Ok(())
    }
}

const CONTROL_STACK_SIZE: usize = 0x40000;

/// One management message accepted by the control owner.
///
/// Commands, internal execution events and guest requests all travel through the
/// same FIFO mailbox. The owner drains them in acceptance order and never
/// handles an operation or callback while the mailbox mutex is held.
pub(crate) enum ControlMessage {
    /// A management command from a [`VmHandle`].
    Command(Command),
    /// An internal execution event posted by a vCPU task or device worker.
    Event(VcpuEvent),
    RunStop {
        run: RunId,
        reason: StopReason,
    },
    /// A guest request awaiting one owned reply.
    Guest {
        /// The exact execution instance that issued the request.
        instance: VcpuInstance,
        /// The owned guest request to service.
        request: GuestRequest,
        /// Completes with the reply value.
        completion: OperationCompletion<usize>,
    },
}

/// Management commands delivered to one instance's control owner.
///
/// `create` is not a command: it is passed as the owner's initial argument.
pub(crate) enum Command {
    /// Start the guest.
    Start(OperationCompletion<RunId>),
    /// Pause the running guest.
    Pause(OperationCompletion<()>),
    /// Resume a paused guest.
    Resume(OperationCompletion<()>),
    /// Stop the guest with a reason.
    Stop(StopReason, OperationCompletion<()>),
    /// Reset the guest to a fresh execution generation.
    Reset(OperationCompletion<RunId>),
    /// A guest reset remains valid only for the run that requested it.
    GuestReset {
        run: RunId,
        completion: OperationCompletion<usize>,
    },
    /// Destroy the instance and unregister it.
    Destroy(OperationCompletion<()>),
    /// Apply one guest-memory translation update to an exact run.
    UpdateMemory {
        /// The run the update is expected to observe.
        expected_run: RunId,
        /// The prepared mapping update.
        update: MemoryUpdate,
        /// Completes with the newly published revision.
        completion: OperationCompletion<MemoryRevision>,
    },
}

impl Command {
    /// Returns the identity of the observation this command carries.
    pub(crate) fn id(&self) -> OperationId {
        match self {
            Command::Start(completion) => completion.id(),
            Command::Pause(completion) => completion.id(),
            Command::Resume(completion) => completion.id(),
            Command::Stop(_, completion) => completion.id(),
            Command::Reset(completion) => completion.id(),
            Command::GuestReset { completion, .. } => completion.id(),
            Command::Destroy(completion) => completion.id(),
            Command::UpdateMemory { completion, .. } => completion.id(),
        }
    }

    /// Rejects this command's observation with `error`.
    ///
    /// The completion state mutex is released before any waker is run, so this
    /// is always safe to call with the mailbox mutex already dropped.
    pub(crate) fn reject(self, error: AxVmError) {
        match self {
            Command::Start(completion) => completion.reject(error),
            Command::Pause(completion) => completion.reject(error),
            Command::Resume(completion) => completion.reject(error),
            Command::Stop(_, completion) => completion.reject(error),
            Command::Reset(completion) => completion.reject(error),
            Command::GuestReset { completion, .. } => completion.reject(error),
            Command::Destroy(completion) => completion.reject(error),
            Command::UpdateMemory { completion, .. } => completion.reject(error),
        }
    }
}

/// The mailbox payload and its close flag, guarded by one sleepable mutex.
struct Mailbox {
    queue: VecDeque<ControlMessage>,
    closed: bool,
}

/// Explicit publication state of the control task.
///
/// A reservation is inserted as `Preparing`; a successful staging publishes the
/// joinable handle; a failed staging publishes the error. Waiters observe the
/// terminal state instead of guessing a task that may still stage.
enum TaskPublication {
    /// The reservation exists but the control task has not been staged yet.
    Preparing,
    /// The control task is staged and joinable.
    Published(ThreadHandle),
    /// Staging failed; the control task will never run.
    Failed(AxVmError),
    /// The task's management lease was released after a successful join.
    Retired,
}

/// One-shot join progress over a published control task.
enum JoinProgress {
    /// No observer has claimed the join.
    Idle,
    /// One observer is performing the join outside the mutex.
    Joining(Arc<JoinAttempt>),
    /// The join completed; `TaskState::result` holds the shared outcome.
    Completed,
}

/// One join attempt remains observable even when a later caller retries it.
struct JoinAttempt {
    result: Mutex<Option<AxVmResult<()>>>,
    ready: Condvar,
}

impl JoinAttempt {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn wait(&self) -> AxVmResult<()> {
        let mut result = self.result.lock_unpoisoned();
        loop {
            if let Some(result) = &*result {
                return result.clone();
            }
            result = self
                .ready
                .wait(result)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn finish(&self, result: AxVmResult<()>) {
        *self.result.lock_unpoisoned() = Some(result);
        self.ready.notify_all();
    }
}

/// Shared state for task publication and the one join of the control task.
///
/// Every waiter releases this mutex through the paired condvar; the first
/// observer claims the join and drops the mutex before blocking, and every
/// other observer waits and observes the same final result. The owner and the
/// exit callback never wait on it.
struct TaskState {
    publication: TaskPublication,
    join: JoinProgress,
    result: Option<AxVmResult<()>>,
}

/// State shared between handles and the control owner.
pub(crate) struct ControlShared {
    key: VmKey,
    sequence: AtomicU64,
    mailbox: Mutex<Mailbox>,
    mailbox_ready: Condvar,
    snapshot: Mutex<Option<PublishedSnapshot>>,
    run_services: Mutex<Option<Arc<RunServices>>>,
    task: Mutex<TaskState>,
    task_ready: Condvar,
    exit: Arc<ControlExit>,
    pub(crate) ivc: Arc<crate::runtime::ivc::IvcManager>,
}

impl ControlShared {
    fn new(
        key: VmKey,
        exit: Arc<ControlExit>,
        ivc: Arc<crate::runtime::ivc::IvcManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            key,
            sequence: AtomicU64::new(0),
            mailbox: Mutex::new(Mailbox {
                queue: VecDeque::new(),
                closed: false,
            }),
            mailbox_ready: Condvar::new(),
            snapshot: Mutex::new(None),
            run_services: Mutex::new(None),
            task: Mutex::new(TaskState {
                publication: TaskPublication::Preparing,
                join: JoinProgress::Idle,
                result: None,
            }),
            task_ready: Condvar::new(),
            exit,
            ivc,
        })
    }

    /// Returns the exact instance identity.
    pub(crate) fn key(&self) -> VmKey {
        self.key
    }

    /// Allocates one observation with a checked, monotonically increasing
    /// operation sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when the instance's operation sequence is exhausted.
    pub(crate) fn new_operation<T>(&self) -> AxVmResult<(VmOperation<T>, OperationCompletion<T>)> {
        let mut current = self.sequence.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(1).ok_or_else(|| {
                AxVmError::resource_unavailable(
                    "VM operation sequence",
                    "operation sequence exhausted",
                )
            })?;
            match self.sequence.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(VmOperation::new(OperationId::new(self.key, next))),
                Err(observed) => current = observed,
            }
        }
    }

    /// Publishes the owner's latest observation snapshot.
    pub(crate) fn publish(&self, snapshot: VmSnapshot, progress: Vec<Arc<VcpuProgress>>) {
        let retired = self.snapshot.lock_unpoisoned().replace(PublishedSnapshot {
            value: snapshot,
            progress,
        });
        drop(retired);
    }

    /// Returns a handle to this instance.
    pub(crate) fn handle(self: &Arc<Self>) -> VmHandle {
        VmHandle {
            shared: self.clone(),
        }
    }

    /// Publishes task-side service ports; destructors run after the mutex ends.
    pub(crate) fn publish_run_services(&self, services: Option<Arc<RunServices>>) {
        let retired = std::mem::replace(&mut *self.run_services.lock_unpoisoned(), services);
        drop(retired);
    }

    /// Publishes the staged control task as joinable.
    ///
    /// The waiters are woken after the publication mutex is released.
    fn publish_task(&self, handle: ThreadHandle) {
        {
            let mut state = self.task.lock_unpoisoned();
            state.publication = TaskPublication::Published(handle);
        }
        self.task_ready.notify_all();
    }

    /// Publishes a staging failure so join waiters stop waiting.
    ///
    /// The waiters are woken after the publication mutex is released.
    fn publish_task_failure(&self, error: &AxVmError) {
        {
            let mut state = self.task.lock_unpoisoned();
            state.publication = TaskPublication::Failed(error.clone());
        }
        self.task_ready.notify_all();
    }

    /// Finalizes a reservation whose control task could not be staged.
    ///
    /// The reservation never produced a control task, so its terminal state is
    /// the explicit creation failure. This closes the exact command entry,
    /// rejects every queued command and guest request, completes every destroy
    /// observer with the failure through the exit observation, and only then
    /// removes the exact reservation. It must run outside the registry lock.
    pub(crate) fn fail_creation(&self, error: &AxVmError) {
        self.publish_task_failure(error);
        self.close_for_destroy(&self.exit);
        self.exit.fail_creation(error);
    }

    /// Returns the terminal task outcome without blocking, if there is one.
    ///
    /// A staging failure, or a completed join, is the evidence that the exact
    /// instance can no longer hold live resources.
    fn terminal_outcome(&self) -> Option<AxVmResult<()>> {
        let state = self.task.lock_unpoisoned();
        if let TaskPublication::Failed(error) = &state.publication {
            return Some(Err(error.clone()));
        }
        matches!(state.join, JoinProgress::Completed)
            .then(|| state.result.clone().unwrap_or(Ok(())))
    }

    /// Blocks until the next message is available, or returns `None` once the
    /// entry is closed and drained.
    ///
    /// The emptiness check and the waiter registration happen under the same
    /// mailbox mutex, so a publisher cannot wake between them.
    pub(crate) fn next_message(&self) -> Option<ControlMessage> {
        let mut mailbox = self.mailbox.lock_unpoisoned();
        loop {
            if let Some(message) = mailbox.queue.pop_front() {
                return Some(message);
            }
            if mailbox.closed {
                return None;
            }
            mailbox = self
                .mailbox_ready
                .wait(mailbox)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Publishes one internal execution event posted by a vCPU task or device
    /// worker.
    ///
    /// The event and the close check share the mailbox mutex. The wake is
    /// performed after the mutex is released so the control owner cannot observe
    /// a half-published event.
    pub(crate) fn post_event(&self, event: VcpuEvent) {
        let mut mailbox = self.mailbox.lock_unpoisoned();
        if mailbox.closed {
            return;
        }
        mailbox.queue.push_back(ControlMessage::Event(event));
        drop(mailbox);
        self.mailbox_ready.notify_one();
    }

    pub(crate) fn request_run_stop(&self, run: RunId, reason: StopReason) -> AxVmResult {
        let mut mailbox = self.mailbox.lock_unpoisoned();
        if mailbox.closed {
            return Err(AxVmError::EntryClosed { vm: self.key });
        }
        mailbox
            .queue
            .push_back(ControlMessage::RunStop { run, reason });
        drop(mailbox);
        self.mailbox_ready.notify_one();
        Ok(())
    }

    /// Posts one owned guest request and returns the observation of its reply.
    ///
    /// # Errors
    ///
    /// Returns an error when the command entry is closed or the operation
    /// sequence is exhausted.
    pub(crate) fn request_guest(
        &self,
        instance: VcpuInstance,
        request: GuestRequest,
    ) -> AxVmResult<VmOperation<usize>> {
        let (operation, completion) = self.new_operation::<usize>()?;
        let mut mailbox = self.mailbox.lock_unpoisoned();
        if mailbox.closed {
            drop(mailbox);
            completion.reject(AxVmError::EntryClosed { vm: self.key });
            return Err(AxVmError::EntryClosed { vm: self.key });
        }
        mailbox.queue.push_back(ControlMessage::Guest {
            instance,
            request,
            completion,
        });
        drop(mailbox);
        self.mailbox_ready.notify_one();
        Ok(operation)
    }

    /// Closes the entry for a successful destroy and merges every observation.
    ///
    /// Queued destroy observations are handed to the exit callback so every
    /// repeated observer completes with the same exit result; every other queued
    /// command or guest request is rejected. The mailbox mutex is released before
    /// any completion is published.
    pub(crate) fn close_for_destroy(&self, exit: &ControlExit) {
        for message in self.take_all_and_close() {
            match message {
                ControlMessage::Command(Command::Destroy(completion)) => {
                    exit.handoff(completion);
                }
                ControlMessage::Command(command) => {
                    command.reject(AxVmError::EntryClosed { vm: self.key });
                }
                ControlMessage::Event(_) | ControlMessage::RunStop { .. } => {}
                ControlMessage::Guest { completion, .. } => {
                    completion.reject(AxVmError::EntryClosed { vm: self.key });
                }
            }
        }
        self.mailbox_ready.notify_all();
    }

    fn take_all_and_close(&self) -> Vec<ControlMessage> {
        let mut mailbox = self.mailbox.lock_unpoisoned();
        mailbox.closed = true;
        mailbox.queue.drain(..).collect()
    }

    fn is_ready(&self) -> bool {
        self.snapshot.lock_unpoisoned().is_some()
    }

    fn ready_handle(self: &Arc<Self>) -> Option<VmHandle> {
        self.is_ready().then(|| self.handle())
    }

    pub(crate) fn dispatch_command(&self, command: Command) -> AxVmResult<()> {
        let mut mailbox = self.mailbox.lock_unpoisoned();
        if !mailbox.closed {
            mailbox.queue.push_back(ControlMessage::Command(command));
            drop(mailbox);
            self.mailbox_ready.notify_one();
            return Ok(());
        }
        drop(mailbox);

        // The entry is closed. A destroy observation still merges with the exit
        // so a repeated destroy never observes `EntryClosed`; every other command
        // is rejected.
        match command {
            Command::Destroy(completion) => {
                self.exit.handoff(completion);
                Ok(())
            }
            command => {
                let vm = self.key;
                command.reject(AxVmError::EntryClosed { vm });
                Err(AxVmError::EntryClosed { vm })
            }
        }
    }
}

/// Registry/unregistration state handed to the control-task exit callback.
///
/// It holds only a `Weak` registry and the pending destroy completions, so it
/// never forms a strong reference cycle with the control task's `ThreadHandle`.
pub(crate) struct ControlExit {
    key: VmKey,
    registry: Weak<std::sync::Mutex<RegistryState>>,
    state: Mutex<ExitState>,
}

struct ExitState {
    pending: Vec<OperationCompletion<()>>,
    /// Terminal outcome once the exit is finalized; `None` until then.
    outcome: Option<AxVmResult<()>>,
}

impl ControlExit {
    fn new(key: VmKey, registry: Weak<std::sync::Mutex<RegistryState>>) -> Self {
        Self {
            key,
            registry,
            state: Mutex::new(ExitState {
                pending: Vec::new(),
                outcome: None,
            }),
        }
    }

    /// Hands a destroy observation to the exit callback.
    ///
    /// When the exit is already finalized the observation completes immediately
    /// with that outcome, so a repeated destroy never observes `EntryClosed` and
    /// a failed creation never fabricates a success. The completion's state mutex
    /// is released before any waker runs.
    pub(crate) fn handoff(&self, completion: OperationCompletion<()>) {
        let outcome = {
            let mut state = self.state.lock_unpoisoned();
            match &state.outcome {
                Some(outcome) => Some(outcome.clone()),
                None => {
                    state.pending.push(completion);
                    return;
                }
            }
        };
        if let Some(outcome) = outcome {
            completion.finish(outcome);
        }
    }

    /// Finalizes the exit after the control task returned and publishes every
    /// pending destroy completion.
    ///
    /// Runs in the exit callback on ordinary task-work context, after the owner
    /// entry returned and the machine resources were released. It never joins or
    /// waits its own thread.
    fn on_task_exit(&self) {
        let outcome = if self.remove_exact() {
            Ok(())
        } else {
            Err(AxVmError::resource_unavailable(
                "unregister destroyed VM",
                "instance registry entry did not match the exiting owner",
            ))
        };
        self.finalize(outcome);
    }

    pub(crate) fn record_creation_failure(&self, error: AxVmError) {
        self.finalize(Err(error));
    }

    /// Finalizes the exit when the control task could not be staged.
    ///
    /// The creation failure is the explicit exit observation: every pending and
    /// later destroy observer observes it instead of a fabricated success. The
    /// exact reservation is removed after the observers are completed.
    fn fail_creation(&self, error: &AxVmError) {
        self.finalize(Err(error.clone()));
        let _removed = self.remove_exact();
    }

    /// Publishes `outcome` once and finishes every pending destroy completion.
    ///
    /// Completions are finished after the state mutex is released.
    fn finalize(&self, outcome: AxVmResult<()>) {
        let pending = {
            let mut state = self.state.lock_unpoisoned();
            if state.outcome.is_some() {
                return;
            }
            state.outcome = Some(outcome.clone());
            std::mem::take(&mut state.pending)
        };
        for completion in pending {
            completion.finish(outcome.clone());
        }
    }

    fn remove_exact(&self) -> bool {
        let Some(registry) = self.registry.upgrade() else {
            return false;
        };
        // Remove under the registry lock, then drop the instance (and any
        // control/shared handles it owns) after the lock is released.
        let removed = {
            let mut registry = registry.lock_unpoisoned();
            match registry.entries.get(&self.key.vm_id()) {
                Some(instance) if instance.key == self.key => {
                    registry.entries.remove(&self.key.vm_id())
                }
                _ => None,
            }
        };
        let removed_any = removed.is_some();
        drop(removed);
        removed_any
    }
}

static CONTROL_EXIT_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: control_exit_switch_in,
    on_switch_out: control_exit_switch_out,
    on_exit: control_exit_on_exit,
    on_deadline_overrun: control_exit_deadline_overrun,
    drop: control_exit_drop,
};

unsafe extern "Rust" fn control_exit_switch_in(
    _data: usize,
    _thread: ThreadId,
    _policy: SchedulePolicy,
    _charged_runtime_ns: u64,
) {
}

unsafe extern "Rust" fn control_exit_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: SwitchReason,
) {
}

unsafe extern "Rust" fn control_exit_deadline_overrun(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn control_exit_on_exit(data: usize, _thread: ThreadId) {
    // SAFETY: `data` is the unique `Box<Arc<ControlExit>>` installed when the
    // control task was staged; the extension drop callback frees it afterwards.
    let exit = unsafe { &*(data as *const Arc<ControlExit>) };
    exit.on_task_exit();
}

unsafe extern "Rust" fn control_exit_drop(data: usize) {
    // SAFETY: `data` is the same unique box installed for this extension.
    drop(unsafe { Box::from_raw(data as *mut Arc<ControlExit>) });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the real frontend components: one registry with a single
    /// reservation, its shared state and its exit observation.
    fn scenario() -> (
        Arc<std::sync::Mutex<RegistryState>>,
        Arc<ControlShared>,
        Arc<ControlExit>,
    ) {
        let registry = Arc::new(std::sync::Mutex::new(RegistryState {
            entries: BTreeMap::new(),
            generations: BTreeMap::new(),
            closed: false,
        }));
        let key = VmKey::new(7, 1);
        let exit = Arc::new(ControlExit::new(key, Arc::downgrade(&registry)));
        let shared = ControlShared::new(
            key,
            exit.clone(),
            Arc::new(crate::runtime::ivc::IvcManager::new()),
        );
        registry.lock_unpoisoned().entries.insert(
            key.vm_id(),
            Instance {
                key,
                shared: shared.clone(),
            },
        );
        (registry, shared, exit)
    }

    fn staging_failure() -> AxVmError {
        AxVmError::resource_unavailable("prepare VM control task", "staging failed")
    }

    #[test]
    fn closed_entry_merges_destroy_and_rejects_other_commands() {
        let (_registry, shared, exit) = scenario();

        let (start_operation, start_completion) = shared.new_operation::<RunId>().unwrap();
        shared
            .dispatch_command(Command::Start(start_completion))
            .unwrap();

        let (destroy_operation, destroy_completion) = shared.new_operation::<()>().unwrap();
        shared
            .dispatch_command(Command::Destroy(destroy_completion))
            .unwrap();

        // Closing for destroy merges the queued destroy and rejects the rest.
        shared.close_for_destroy(&exit);
        assert!(matches!(
            start_operation.wait(),
            Err(AxVmError::EntryClosed { .. })
        ));

        // The merged destroy only completes once the exit callback runs.
        exit.on_task_exit();
        assert_eq!(destroy_operation.wait(), Ok(()));
    }

    #[test]
    fn repeated_destroy_after_close_merges_with_the_same_exit() {
        let (_registry, shared, exit) = scenario();

        shared.close_for_destroy(&exit);
        // A destroy accepted after the entry closed but before the exit callback
        // must merge, not observe `EntryClosed`.
        // The owner publishes its final state before the exit callback. That
        // observation cannot prove resource release or finish another destroy.
        shared.publish(
            VmSnapshot {
                key: shared.key(),
                vm_id: shared.key().vm_id(),
                name: "test instance".into(),
                state: VmStatus::Destroyed,
                run: None,
                current_operation: None,
                last_failure: None,
                last_stop_reason: None,
                cpu: CpuObservation {
                    vcpu_num: 0,
                    running_vcpu_count: 0,
                },
                memory: MemoryObservation {
                    nested_page_table_root: None,
                    total_bytes: 0,
                    regions: vec![],
                },
                device: DeviceObservation { device_count: 0 },
                vcpu: vec![],
                description: VmConfigSnapshot {
                    bsp_entry: GuestPhysAddr::from(0),
                    ap_entry: GuestPhysAddr::from(0),
                    image: VMImageConfig::default(),
                    address_space_policy: AddressSpacePolicy::default(),
                    vcpu_affinities: vec![],
                    passthrough_devices: vec![],
                    passthrough_addresses: vec![],
                    passthrough_irqs: vec![],
                },
                entry_count: 0,
                park_count: 0,
            },
            Vec::new(),
        );
        let mut operation = shared.handle().destroy().unwrap();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            std::future::Future::poll(std::pin::Pin::new(&mut operation), &mut context)
                .is_pending()
        );
        exit.on_task_exit();
        assert_eq!(operation.wait(), Ok(()));
        assert_eq!(shared.handle().destroy().unwrap().wait(), Ok(()));
    }

    #[test]
    fn mailbox_drains_fifo_and_next_message_stops_when_closed() {
        let (_registry, shared, _exit) = scenario();

        let (_start_operation, start_completion) = shared.new_operation::<RunId>().unwrap();
        shared
            .dispatch_command(Command::Start(start_completion))
            .unwrap();
        let (_stop_operation, stop_completion) = shared.new_operation::<()>().unwrap();
        shared
            .dispatch_command(Command::Stop(StopReason::Forced, stop_completion))
            .unwrap();

        assert!(matches!(
            shared.next_message(),
            Some(ControlMessage::Command(Command::Start(_)))
        ));
        assert!(matches!(
            shared.next_message(),
            Some(ControlMessage::Command(Command::Stop(..)))
        ));
        shared.close_for_destroy(&_exit);
        assert!(shared.next_message().is_none());
    }

    /// `shutdown` closes the create entry and captures an in-flight reservation
    /// before its control task has staged; the staging failure must complete the
    /// queued destroy observer with the explicit failure instead of hanging or
    /// reporting a fabricated success, and it must remove the exact reservation.
    #[test]
    fn create_failure_completes_reservation_destroy_observers() {
        let (registry, shared, _exit) = scenario();

        let handle = {
            let mut registry = registry.lock_unpoisoned();
            registry.closed = true;
            registry
                .entries
                .values()
                .next()
                .expect("reservation is registered")
                .shared
                .handle()
        };

        // A destroy arriving while the reservation is still preparing is queued
        // behind the pending control owner.
        let destroy = handle.destroy().unwrap();

        // The control task never staged.
        let error = staging_failure();
        shared.fail_creation(&error);

        assert_eq!(destroy.wait(), Err(error.clone()));
        // A repeated destroy after the failure observes the same failure, and is
        // never reported as a successful destroy.
        assert_eq!(handle.destroy().unwrap().wait(), Err(error.clone()));
        // Joining reports the creation failure as the terminal evidence.
        assert_eq!(handle.join_control_task(), Err(error));
        // The exact reservation was removed so the identifier can be reused.
        assert!(!registry.lock_unpoisoned().entries.contains_key(&7));
    }

    /// A join on a reservation that has not staged must wait for the staging
    /// result instead of reporting an already-joined task; a staging failure is
    /// then observed as the join outcome.
    #[test]
    fn join_defers_until_publication_and_reports_creation_failure() {
        let (_registry, shared, _exit) = scenario();
        let joined = shared.handle();
        let error = staging_failure();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            // Signal that the joiner is about to wait so the publication is
            // observed after the join has started.
            ready_tx.send(()).unwrap();
            result_tx.send(joined.join_control_task()).unwrap();
        });
        ready_rx.recv().unwrap();

        // Publish the failure only after the joiner is running.
        shared.fail_creation(&error);
        worker.join().unwrap();

        // A join that reported a not-yet-published task would return `Ok(())`.
        assert_eq!(result_rx.recv().unwrap(), Err(error));
    }

    /// A stale exit observation from an older generation must not unregister a
    /// newer instance that reused the numeric identifier, and it must not
    /// fabricate a destroy success.
    #[test]
    fn stale_instance_exit_does_not_unregister_the_reused_identifier() {
        let registry = Arc::new(std::sync::Mutex::new(RegistryState {
            entries: BTreeMap::new(),
            generations: BTreeMap::new(),
            closed: false,
        }));

        // A newer generation reuses the numeric VM identifier.
        let new_key = VmKey::new(7, 2);
        let new_exit = Arc::new(ControlExit::new(new_key, Arc::downgrade(&registry)));
        let new_shared = ControlShared::new(
            new_key,
            new_exit,
            Arc::new(crate::runtime::ivc::IvcManager::new()),
        );
        registry.lock_unpoisoned().entries.insert(
            new_key.vm_id(),
            Instance {
                key: new_key,
                shared: new_shared,
            },
        );

        // A stale exit from generation 1 must not touch the newer instance.
        let stale_key = VmKey::new(7, 1);
        let stale_exit = ControlExit::new(stale_key, Arc::downgrade(&registry));
        let (operation, completion) = VmOperation::<()>::new(OperationId::new(stale_key, 1));
        stale_exit.handoff(completion);
        stale_exit.on_task_exit();

        assert!(
            operation.wait().is_err(),
            "a stale exit cannot fabricate a destroy success"
        );
        let registry = registry.lock_unpoisoned();
        let entry = registry
            .entries
            .get(&7)
            .expect("the newer instance stays registered");
        assert_eq!(entry.key, new_key);
    }
}
