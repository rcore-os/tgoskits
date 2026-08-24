//! Host-console multiplexing for mandatory guest virtual serial devices.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};

use anyhow::{Result, bail};
use axvm::{SerialBackend, SerialBackendFactory, VMId, VmStatus};
use core::{
    ops::Bound::{Excluded, Unbounded},
    sync::atomic::{AtomicUsize, Ordering},
};
use log::warn;
use std::sync::{LazyLock, Mutex, MutexGuard};

use super::host::write_host_bytes;

mod endpoint;
mod host_output;
mod output;

use endpoint::GuestConsoleEndpoint;
use host_output::HostOutputQueue;
use output::GuestOutputMux;

const CTRL_X: u8 = 0x18;
const INPUT_QUEUE_CAPACITY: usize = 4096;

static GUEST_CONSOLE_MUX: LazyLock<GuestConsoleMux> = LazyLock::new(GuestConsoleMux::new);

/// Result of routing one byte read from the host console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleInputEvent {
    /// The byte belongs to the Axvisor shell.
    ShellByte(u8),
    /// Two bytes belong to the Axvisor shell in the given order.
    ShellSequence(u8, u8),
    /// The byte was consumed by the attached guest or a shortcut prefix.
    Consumed,
    /// A shortcut attached the named guest.
    Attached(VMId),
    /// A shortcut returned from the named guest to the shell.
    Detached(VMId),
    /// No running guest is available for attachment.
    NoRunningGuest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuestConsoleCounts {
    pub vm_id: VMId,
    pub active: bool,
    pub input_enqueued: usize,
    pub input_drained: usize,
    pub input_dropped: usize,
    pub input_pending: usize,
    pub output_enqueued: usize,
    pub output_drained: usize,
    pub output_dropped: usize,
    pub output_pending: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConsoleStatsSnapshot {
    pub attached: Option<VMId>,
    pub flush_calls: usize,
    pub host_write_bytes: usize,
    pub guests: Vec<GuestConsoleCounts>,
}

#[derive(Debug)]
struct RoutedInput {
    event: ConsoleInputEvent,
    wake_vm: Option<VMId>,
    host_output: Vec<u8>,
}

/// Application-owned host console multiplexer.
///
/// The multiplexer is the only reader of the physical host console. Each VM
/// gets a [`SerialBackend`] backed by its own bounded RX queue. Guest output is
/// serialized here before it reaches the host UART.
#[derive(Debug)]
pub struct GuestConsoleMux {
    core: Arc<ConsoleCore>,
}

#[derive(Debug)]
struct ConsoleCore {
    state: Mutex<ConsoleState>,
    /// Serializes physical writes performed by housekeeping paths.
    ///
    /// Guest UART exits never acquire this lock or the global control-state
    /// lock; they only stage output in their endpoint ring, so a slow physical
    /// console or console attachment operation cannot block a vCPU.
    output_lock: Mutex<()>,
    flush_calls: AtomicUsize,
    host_write_bytes: AtomicUsize,
}

#[derive(Debug, Default)]
struct ConsoleState {
    guests: BTreeMap<VMId, GuestState>,
    running: BTreeSet<VMId>,
    attached: Option<VMId>,
    last_attached: Option<VMId>,
    shortcut_prefix_pending: bool,
    output: GuestOutputMux,
    host_output: HostOutputQueue,
    next_backend_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackendGeneration(u64);

#[derive(Debug, Default)]
struct GuestState {
    backend_generation: Option<BackendGeneration>,
    endpoint: Option<Arc<GuestConsoleEndpoint>>,
}

#[derive(Debug)]
struct GuestSerialBackend {
    #[cfg(any(test, axtest))]
    generation: BackendGeneration,
    endpoint: Arc<GuestConsoleEndpoint>,
}

#[derive(Debug)]
struct GuestSerialBackendFactory {
    vm_id: VMId,
    core: Arc<ConsoleCore>,
}

impl GuestConsoleMux {
    fn new() -> Self {
        Self {
            core: Arc::new(ConsoleCore {
                state: Mutex::new(ConsoleState::default()),
                output_lock: Mutex::new(()),
                flush_calls: AtomicUsize::new(0),
                host_write_bytes: AtomicUsize::new(0),
            }),
        }
    }

    fn set_running(&self, running: impl IntoIterator<Item = VMId>) -> Option<VMId> {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        state.running.clear();
        for vm_id in running {
            state.running.insert(vm_id);
            state.guests.entry(vm_id).or_default();
        }

        let detached = state
            .attached
            .filter(|vm_id| !state.running.contains(vm_id));
        let host_output = if detached.is_some() {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            state.output.buffer_all()
        } else {
            Vec::new()
        };
        let ConsoleState {
            running, output, ..
        } = &mut *state;
        output.reconcile_running(running);
        state.host_output.push(&host_output);
        let host_output = state.host_output.take();
        drop(state);
        write_host_bytes(&host_output.into_bytes());
        detached
    }

    fn mark_running(&self, vm_id: VMId) {
        let mut state = self.core.lock_state();
        state.running.insert(vm_id);
        state.guests.entry(vm_id).or_default();
    }

    fn mark_stopped(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        state.running.remove(&vm_id);
        if let Some(guest) = state.guests.get_mut(&vm_id) {
            guest.backend_generation = None;
            if let Some(endpoint) = guest.endpoint.take() {
                endpoint.deactivate();
            }
        }
        state.output.reset_guest(vm_id);
        let detached = state.attached == Some(vm_id);
        let host_output = if detached {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            state.output.buffer_all()
        } else {
            Vec::new()
        };
        state.host_output.push(&host_output);
        let host_output = state.host_output.take();
        drop(state);
        write_host_bytes(&host_output.into_bytes());
        detached
    }

    fn remove(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        state.running.remove(&vm_id);
        if let Some(guest) = state.guests.remove(&vm_id)
            && let Some(endpoint) = guest.endpoint
        {
            endpoint.deactivate();
        }
        state.output.reset_guest(vm_id);
        if state.last_attached == Some(vm_id) {
            state.last_attached = None;
        }
        let detached = state.attached == Some(vm_id);
        let host_output = if detached {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            state.output.buffer_all()
        } else {
            Vec::new()
        };
        state.host_output.push(&host_output);
        let host_output = state.host_output.take();
        drop(state);
        write_host_bytes(&host_output.into_bytes());
        detached
    }

    fn attach_default(&self, running: impl IntoIterator<Item = VMId>) -> Option<VMId> {
        self.set_running(running);
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        let vm_id = state.running.first().copied()?;
        state.attached = Some(vm_id);
        state.last_attached = Some(vm_id);
        state.shortcut_prefix_pending = false;
        state.output.start_boot_multiplex();
        state.output.request_preemption(vm_id);
        Some(vm_id)
    }

    fn attach(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        if !state.running.contains(&vm_id) {
            return false;
        }
        state.attached = Some(vm_id);
        state.last_attached = Some(vm_id);
        state.shortcut_prefix_pending = false;
        let host_output = state.output.buffer_all();
        state.host_output.push(&host_output);
        let host_output = state.host_output.take();
        drop(state);
        write_host_bytes(&host_output.into_bytes());
        true
    }

    fn attached_vm(&self) -> Option<VMId> {
        self.core.lock_state().attached
    }

    fn activate(&self, vm_id: VMId) -> Option<Vec<u8>> {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        (state.attached == Some(vm_id)).then_some(())?;
        let replay = state.output.select_foreground(vm_id);
        state.host_output.push(&replay);
        let host_output = state.host_output.take();
        drop(state);
        write_host_bytes(&host_output.into_bytes());
        Some(replay)
    }

    fn route_host_byte(&self, byte: u8) -> RoutedInput {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        let routed = if state.shortcut_prefix_pending {
            state.shortcut_prefix_pending = false;
            match byte {
                b'h' => match state.attached.take() {
                    Some(vm_id) => RoutedInput {
                        event: ConsoleInputEvent::Detached(vm_id),
                        wake_vm: None,
                        host_output: state.output.buffer_all(),
                    },
                    None => RoutedInput {
                        event: ConsoleInputEvent::Consumed,
                        wake_vm: None,
                        host_output: Vec::new(),
                    },
                },
                b'[' => switch_guest(&mut state, GuestSwitchDirection::Previous),
                b']' => switch_guest(&mut state, GuestSwitchDirection::Next),
                CTRL_X => {
                    route_literal_input(&mut state, &[CTRL_X], ConsoleInputEvent::ShellByte(CTRL_X))
                }
                byte => route_literal_input(
                    &mut state,
                    &[CTRL_X, byte],
                    ConsoleInputEvent::ShellSequence(CTRL_X, byte),
                ),
            }
        } else if byte == CTRL_X {
            state.shortcut_prefix_pending = true;
            RoutedInput {
                event: ConsoleInputEvent::Consumed,
                wake_vm: None,
                host_output: Vec::new(),
            }
        } else {
            route_literal_input(&mut state, &[byte], ConsoleInputEvent::ShellByte(byte))
        };
        state.host_output.push(&routed.host_output);
        let host_output = state.host_output.take();
        drop(state);
        write_host_bytes(&host_output.into_bytes());
        routed
    }
}

fn route_literal_input(
    state: &mut ConsoleState,
    guest_bytes: &[u8],
    shell_event: ConsoleInputEvent,
) -> RoutedInput {
    match state.attached {
        Some(vm_id) => {
            let host_output = state.output.select_foreground_on_input(vm_id);
            enqueue_guest_input(state, vm_id, guest_bytes);
            RoutedInput {
                event: ConsoleInputEvent::Consumed,
                wake_vm: Some(vm_id),
                host_output,
            }
        }
        None => RoutedInput {
            event: shell_event,
            wake_vm: None,
            host_output: Vec::new(),
        },
    }
}

#[derive(Clone, Copy)]
enum GuestSwitchDirection {
    Previous,
    Next,
}

fn switch_guest(state: &mut ConsoleState, direction: GuestSwitchDirection) -> RoutedInput {
    let anchor = state.attached.or(state.last_attached);
    let vm_id = match (direction, anchor) {
        (GuestSwitchDirection::Previous, Some(anchor)) => state
            .running
            .range(..anchor)
            .next_back()
            .copied()
            .or_else(|| state.running.last().copied()),
        (GuestSwitchDirection::Next, Some(anchor)) => state
            .running
            .range((Excluded(anchor), Unbounded))
            .next()
            .copied()
            .or_else(|| state.running.first().copied()),
        (GuestSwitchDirection::Previous, None) => state.running.last().copied(),
        (GuestSwitchDirection::Next, None) => state.running.first().copied(),
    };
    let Some(vm_id) = vm_id else {
        return RoutedInput {
            event: ConsoleInputEvent::NoRunningGuest,
            wake_vm: None,
            host_output: Vec::new(),
        };
    };

    state.attached = Some(vm_id);
    state.last_attached = Some(vm_id);
    RoutedInput {
        event: ConsoleInputEvent::Attached(vm_id),
        wake_vm: None,
        host_output: state.output.buffer_all(),
    }
}

impl ConsoleCore {
    fn lock_state(&self) -> MutexGuard<'_, ConsoleState> {
        self.state
            .lock()
            .expect("guest console state mutex poisoned")
    }

    fn lock_output(&self) -> MutexGuard<'_, ()> {
        self.output_lock
            .lock()
            .expect("guest console output mutex poisoned")
    }

    fn create_serial_backend(self: &Arc<Self>, vm_id: VMId) -> Arc<GuestSerialBackend> {
        let _output_guard = self.lock_output();
        let endpoint = Arc::new(GuestConsoleEndpoint::new());
        let generation = {
            let mut state = self.lock_state();
            if let Some(previous) = state
                .guests
                .get_mut(&vm_id)
                .and_then(|guest| guest.endpoint.take())
            {
                previous.deactivate();
            }
            state.next_backend_generation = state
                .next_backend_generation
                .checked_add(1)
                .expect("guest serial backend generation exhausted");
            let generation = BackendGeneration(state.next_backend_generation);
            state.guests.insert(
                vm_id,
                GuestState {
                    backend_generation: Some(generation),
                    endpoint: Some(endpoint.clone()),
                },
            );
            state.output.reset_guest(vm_id);
            generation
        };
        #[cfg(not(any(test, axtest)))]
        let _ = generation;
        Arc::new(GuestSerialBackend {
            #[cfg(any(test, axtest))]
            generation,
            endpoint,
        })
    }

    #[cfg(any(test, axtest))]
    fn format_guest_output(
        &self,
        vm_id: VMId,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> Option<Vec<u8>> {
        let mut state = self.lock_state();
        let multiple_running = state.running.len() > 1;
        state
            .guests
            .get(&vm_id)
            .filter(|guest| guest.backend_generation == Some(generation))?;
        Some(state.output.format(vm_id, multiple_running, bytes))
    }

    fn flush_host_output(&self) {
        self.flush_calls.fetch_add(1, Ordering::Relaxed);
        let _output_guard = self.lock_output();
        let endpoints = {
            let state = self.lock_state();
            state
                .guests
                .iter()
                .filter_map(|(&vm_id, guest)| {
                    Some((vm_id, guest.backend_generation?, guest.endpoint.clone()?))
                })
                .collect::<Vec<_>>()
        };

        for (vm_id, generation, endpoint) in endpoints {
            let batch = endpoint.take_output();
            if batch.is_empty() {
                continue;
            }
            let bytes = batch.into_bytes();
            let mut state = self.lock_state();
            let current = state.guests.get(&vm_id).is_some_and(|guest| {
                guest.backend_generation == Some(generation)
                    && guest
                        .endpoint
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &endpoint))
            });
            if !current {
                continue;
            }
            let multiple_running = state.running.len() > 1;
            let output = state.output.format(vm_id, multiple_running, &bytes);
            state.host_output.push(&output);
        }

        loop {
            let output = self.lock_state().host_output.take();
            if output.is_empty() {
                break;
            }
            let bytes = output.into_bytes();
            self.host_write_bytes
                .fetch_add(bytes.len(), Ordering::Relaxed);
            write_host_bytes(&bytes);
        }
    }

    fn stats_snapshot(&self) -> ConsoleStatsSnapshot {
        let state = self.lock_state();
        let attached = state.attached;
        let endpoints = state
            .guests
            .iter()
            .filter_map(|(&vm_id, guest)| Some((vm_id, guest.endpoint.clone()?)))
            .collect::<Vec<_>>();
        drop(state);

        ConsoleStatsSnapshot {
            attached,
            flush_calls: self.flush_calls.load(Ordering::Relaxed),
            host_write_bytes: self.host_write_bytes.load(Ordering::Relaxed),
            guests: endpoints
                .into_iter()
                .map(|(vm_id, endpoint)| {
                    let snapshot = endpoint.snapshot();
                    GuestConsoleCounts {
                        vm_id,
                        active: snapshot.active,
                        input_enqueued: snapshot.input_enqueued,
                        input_drained: snapshot.input_drained,
                        input_dropped: snapshot.input_dropped,
                        input_pending: snapshot.input_pending,
                        output_enqueued: snapshot.output_enqueued,
                        output_drained: snapshot.output_drained,
                        output_dropped: snapshot.output_dropped,
                        output_pending: snapshot.output_pending,
                    }
                })
                .collect(),
        }
    }
}

impl SerialBackend for GuestSerialBackend {
    fn write(&self, bytes: &[u8]) {
        self.endpoint.write_output(bytes);
    }

    fn read(&self, buffer: &mut [u8]) -> usize {
        self.endpoint.read_input(buffer)
    }
}

impl SerialBackendFactory for GuestSerialBackendFactory {
    fn create(&self) -> Arc<dyn SerialBackend> {
        self.core.create_serial_backend(self.vm_id)
    }
}

fn enqueue_guest_input(state: &mut ConsoleState, vm_id: VMId, bytes: &[u8]) {
    if let Some(endpoint) = state
        .guests
        .get(&vm_id)
        .and_then(|guest| guest.endpoint.as_ref())
    {
        endpoint.push_input(bytes);
    }
}

/// Returns the factory that provisions one backend per VM device generation.
pub fn serial_backend_factory(vm_id: VMId) -> Arc<dyn SerialBackendFactory> {
    Arc::new(GuestSerialBackendFactory {
        vm_id,
        core: GUEST_CONSOLE_MUX.core.clone(),
    })
}

/// Route one host byte through the console shortcut and attachment state machine.
pub fn route_host_byte(byte: u8) -> ConsoleInputEvent {
    let routed = GUEST_CONSOLE_MUX.route_host_byte(byte);
    if let Some(vm_id) = routed.wake_vm
        && let Err(error) = crate::manager::AxvmManager::notify_vm(vm_id)
    {
        warn!("failed to wake VM[{vm_id}] for console input: {error:#}");
    }
    routed.event
}

/// Attach the lowest-ID member of the default running VM set.
pub fn attach_default(running: impl IntoIterator<Item = VMId>) -> Option<VMId> {
    GUEST_CONSOLE_MUX.attach_default(running)
}

/// Attach one running VM to the host console.
pub fn attach(vm_id: VMId) -> Result<()> {
    let Some(vm) = crate::manager::AxvmManager::vm_by_id(vm_id) else {
        bail!("VM[{vm_id}] not found");
    };
    if vm.status() != VmStatus::Running {
        bail!("VM[{vm_id}] is not running");
    }
    GUEST_CONSOLE_MUX.mark_running(vm_id);
    if !GUEST_CONSOLE_MUX.attach(vm_id) {
        bail!("VM[{vm_id}] is not available for console attachment");
    }
    Ok(())
}

/// Activates direct output after the shell has announced an attachment.
pub fn activate(vm_id: VMId) {
    GUEST_CONSOLE_MUX.activate(vm_id);
}

/// Record a VM transition to Running.
pub fn mark_running(vm_id: VMId) {
    GUEST_CONSOLE_MUX.mark_running(vm_id);
}

/// Record a VM transition away from Running.
pub fn mark_stopped(vm_id: VMId) -> bool {
    GUEST_CONSOLE_MUX.mark_stopped(vm_id)
}

/// Remove all console state associated with a deleted VM.
pub fn remove(vm_id: VMId) -> bool {
    GUEST_CONSOLE_MUX.remove(vm_id)
}

/// Reconcile console attachment and prefixing against the actual VM registry.
pub fn reconcile_vm_states() -> Option<VMId> {
    let running = crate::manager::AxvmManager::vm_list()
        .into_iter()
        .filter(|vm| vm.status() == VmStatus::Running)
        .map(|vm| vm.id());
    GUEST_CONSOLE_MUX.set_running(running)
}

/// Return the currently attached guest, if any.
pub fn attached_vm() -> Option<VMId> {
    GUEST_CONSOLE_MUX.attached_vm()
}

/// Flushes staged guest output from the housekeeping console path.
pub fn flush_host_output() {
    GUEST_CONSOLE_MUX.core.flush_host_output();
}

pub(crate) fn stats_snapshot() -> ConsoleStatsSnapshot {
    GUEST_CONSOLE_MUX.core.stats_snapshot()
}

#[cfg(test)]
mod tests;
