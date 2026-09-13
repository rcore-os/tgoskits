//! Host-console multiplexing for mandatory guest virtual serial devices.

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    vec::Vec,
};

use anyhow::{Result, bail};
use ax_std::os::arceos::sync::{NoPreemptMutex, NoPreemptMutexGuard};
use axvm::{SerialBackend, SerialBackendFactory, VMId, VmStatus};
use core::ops::Bound::{Excluded, Unbounded};
use log::warn;
use std::sync::LazyLock;

use super::host::{submit_host_bytes, submit_host_transaction};

use axvisor::console_mux::{GuestOutputMux, HostLogBacklog};

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

/// Result of opening a guest console from the management shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleAttachment {
    /// The running guest now owns interactive console input and output.
    Interactive,
    /// A stopped guest's buffered output was replayed without taking console input.
    Replayed,
}

#[derive(Debug)]
struct RoutedInput {
    event: ConsoleInputEvent,
    wake_vm: Option<VMId>,
    host_output: Vec<u8>,
    input_overflow: Option<VMId>,
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
    /// Task and vCPU callbacks use this lock; hard IRQ handlers never do.
    /// No caller may enter a sleepable API while it is held.
    state: NoPreemptMutex<ConsoleState>,
    /// Serializes host writes with backend replacement and invalidation.
    ///
    /// Code that needs both locks must acquire `output_lock` before `state`.
    /// The guest callback additionally acquires the fixed host transport before
    /// `state`, so no physical output or sleepable lock is reachable here.
    output_lock: NoPreemptMutex<()>,
}

#[derive(Debug, Default)]
struct ConsoleState {
    guests: BTreeMap<VMId, GuestState>,
    running: BTreeSet<VMId>,
    output_active: BTreeMap<VMId, BackendGeneration>,
    attached: Option<VMId>,
    last_attached: Option<VMId>,
    shortcut_prefix_pending: bool,
    output: GuestOutputMux,
    host_logs: HostLogBacklog,
    next_backend_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackendGeneration(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuestBackendIdentity {
    vm_id: VMId,
    generation: BackendGeneration,
}

#[derive(Debug, Default)]
struct GuestState {
    /// Stable identity of this VM incarnation, retained after output closes.
    backend_identity: Option<BackendGeneration>,
    /// Generation currently admitted for guest input and output.
    backend_generation: Option<BackendGeneration>,
    input: VecDeque<u8>,
    input_overflow_reported: bool,
}

impl GuestState {
    fn invalidate_backend(&mut self) {
        self.backend_generation = None;
        self.input.clear();
        self.input_overflow_reported = false;
    }

    fn invalidate_backend_generation(&mut self, generation: BackendGeneration) {
        if self.backend_generation == Some(generation) {
            self.invalidate_backend();
        }
    }
}

#[derive(Debug)]
struct GuestSerialBackend {
    vm_id: VMId,
    generation: BackendGeneration,
    core: Arc<ConsoleCore>,
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
                state: NoPreemptMutex::new(ConsoleState::default()),
                output_lock: NoPreemptMutex::new(()),
            }),
        }
    }

    fn set_running(&self, running: impl IntoIterator<Item = VMId>) -> Option<VMId> {
        let generations = self.core.backend_generations();
        let running = running
            .into_iter()
            .filter_map(|vm_id| {
                generations
                    .get(&vm_id)
                    .map(|generation| (vm_id, *generation))
            })
            .collect::<Vec<_>>();
        self.set_vm_states(running.clone(), running, [])
    }

    fn set_vm_states(
        &self,
        running: impl IntoIterator<Item = (VMId, BackendGeneration)>,
        output_active: impl IntoIterator<Item = (VMId, BackendGeneration)>,
        terminal: impl IntoIterator<Item = (VMId, BackendGeneration)>,
    ) -> Option<VMId> {
        let running = running.into_iter().collect::<BTreeMap<_, _>>();
        let mut output_active = output_active.into_iter().collect::<BTreeMap<_, _>>();
        output_active.extend(
            running
                .iter()
                .map(|(vm_id, generation)| (*vm_id, *generation)),
        );
        let terminal = terminal.into_iter().collect::<BTreeMap<_, _>>();

        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        let generation_is_current = |vm_id: &VMId, generation: &BackendGeneration| {
            state
                .guests
                .get(vm_id)
                .is_some_and(|guest| guest.backend_generation == Some(*generation))
        };
        let running = running
            .into_iter()
            .filter(|(vm_id, generation)| generation_is_current(vm_id, generation))
            .map(|(vm_id, _)| vm_id)
            .collect::<BTreeSet<_>>();
        let output_active = output_active
            .into_iter()
            .filter(|(vm_id, generation)| generation_is_current(vm_id, generation))
            .collect::<BTreeMap<_, _>>();

        let inactive = state
            .output_active
            .iter()
            .filter(|(vm_id, generation)| output_active.get(vm_id) != Some(generation))
            .map(|(vm_id, generation)| (*vm_id, *generation))
            .collect::<Vec<_>>();
        for (vm_id, generation) in inactive {
            if let Some(guest) = state.guests.get_mut(&vm_id) {
                guest.invalidate_backend_generation(generation);
            }
        }
        for (vm_id, generation) in terminal {
            if let Some(guest) = state.guests.get_mut(&vm_id) {
                guest.invalidate_backend_generation(generation);
            }
        }
        state.running = running;
        state.output_active = output_active;
        for vm_id in state.output_active.keys().copied().collect::<Vec<_>>() {
            state.guests.entry(vm_id).or_default();
            state.output.register_guest(vm_id);
        }

        let detached = state
            .attached
            .filter(|vm_id| !state.running.contains(vm_id));
        let host_output = if detached.is_some() {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            let mut output = state.output.buffer_all();
            append_host_log_replay(&mut state, &mut output);
            output
        } else {
            Vec::new()
        };
        let output_active = state.output_active.keys().copied().collect::<BTreeSet<_>>();
        state.output.reconcile_running(&output_active);
        drop(state);
        submit_host_bytes(&host_output);
        detached
    }

    fn mark_running(&self, vm_id: VMId) {
        let mut state = self.core.lock_state();
        state.running.insert(vm_id);
        state.guests.entry(vm_id).or_default();
        if let Some(generation) = state.guests[&vm_id].backend_generation {
            state.output_active.insert(vm_id, generation);
        }
        state.output.register_guest(vm_id);
    }

    fn mark_stopped(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        state.running.remove(&vm_id);
        state.output_active.remove(&vm_id);
        if let Some(guest) = state.guests.get_mut(&vm_id) {
            guest.invalidate_backend();
        }
        let detached = state.attached == Some(vm_id);
        let host_output = if detached {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            let mut output = state.output.buffer_all();
            append_host_log_replay(&mut state, &mut output);
            output
        } else {
            Vec::new()
        };
        drop(state);
        submit_host_bytes(&host_output);
        detached
    }

    #[cfg(any(test, axtest))]
    fn remove(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        let (detached, host_output) = remove_guest_state(&mut state, vm_id);
        drop(state);
        submit_host_bytes(&host_output);
        detached
    }

    fn backend_identity(&self, vm_id: VMId) -> Option<GuestBackendIdentity> {
        self.core
            .lock_state()
            .guests
            .get(&vm_id)
            .and_then(|guest| guest.backend_identity)
            .map(|generation| GuestBackendIdentity { vm_id, generation })
    }

    fn remove_if_backend(&self, identity: GuestBackendIdentity) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        if !state
            .guests
            .get(&identity.vm_id)
            .is_some_and(|guest| guest.backend_identity == Some(identity.generation))
        {
            return false;
        }
        let (_, host_output) = remove_guest_state(&mut state, identity.vm_id);
        drop(state);
        submit_host_bytes(&host_output);
        true
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
        drop(state);
        submit_host_bytes(&host_output);
        true
    }

    fn replay_stopped(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        if state.running.contains(&vm_id) || !state.guests.contains_key(&vm_id) {
            return false;
        }
        let Some(host_output) = state.output.replay_buffered(vm_id) else {
            return false;
        };
        drop(state);
        submit_host_bytes(&host_output);
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
        drop(state);
        submit_host_bytes(&replay);
        Some(replay)
    }

    fn route_host_byte(&self, byte: u8) -> RoutedInput {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        let routed = if state.shortcut_prefix_pending {
            state.shortcut_prefix_pending = false;
            match byte {
                b'h' => match state.attached.take() {
                    Some(vm_id) => {
                        let mut host_output = state.output.buffer_all();
                        append_host_log_replay(&mut state, &mut host_output);
                        RoutedInput {
                            event: ConsoleInputEvent::Detached(vm_id),
                            wake_vm: None,
                            host_output,
                            input_overflow: None,
                        }
                    }
                    None => RoutedInput {
                        event: ConsoleInputEvent::Consumed,
                        wake_vm: None,
                        host_output: Vec::new(),
                        input_overflow: None,
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
                input_overflow: None,
            }
        } else {
            route_literal_input(&mut state, &[byte], ConsoleInputEvent::ShellByte(byte))
        };
        drop(state);
        submit_host_bytes(&routed.host_output);
        if let Some(vm_id) = routed.input_overflow {
            warn!(
                "VM[{vm_id}] console input queue is full; dropping input until the guest drains it"
            );
        }
        routed
    }

    fn route_host_log(
        &self,
        record: &[u8],
        dropped_records: usize,
        dropped_bytes: usize,
    ) -> Option<Vec<u8>> {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        state.host_logs.add_drops(dropped_records, dropped_bytes);
        if state.output.foreground_is_interactive() {
            state.host_logs.push(record);
            return None;
        }

        let mut host_output = Vec::new();
        append_host_log_replay(&mut state, &mut host_output);
        if !record.is_empty() {
            host_output.extend(state.output.format_host_record(record));
        }
        Some(host_output)
    }
}

fn remove_guest_state(state: &mut ConsoleState, vm_id: VMId) -> (bool, Vec<u8>) {
    state.running.remove(&vm_id);
    state.output_active.remove(&vm_id);
    state.guests.remove(&vm_id);
    state.output.reset_guest(vm_id);
    if state.last_attached == Some(vm_id) {
        state.last_attached = None;
    }
    let detached = state.attached == Some(vm_id);
    let host_output = if detached {
        state.attached = None;
        state.shortcut_prefix_pending = false;
        let mut output = state.output.buffer_all();
        append_host_log_replay(state, &mut output);
        output
    } else {
        Vec::new()
    };
    (detached, host_output)
}

fn append_host_log_replay(state: &mut ConsoleState, output: &mut Vec<u8>) {
    for record in state.host_logs.drain() {
        output.extend(state.output.format_host_record(&record));
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
            let input_overflow = enqueue_guest_input(state, vm_id, guest_bytes).then_some(vm_id);
            RoutedInput {
                event: ConsoleInputEvent::Consumed,
                wake_vm: Some(vm_id),
                host_output,
                input_overflow,
            }
        }
        None => RoutedInput {
            event: shell_event,
            wake_vm: None,
            host_output: Vec::new(),
            input_overflow: None,
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
            input_overflow: None,
        };
    };

    state.attached = Some(vm_id);
    state.last_attached = Some(vm_id);
    RoutedInput {
        event: ConsoleInputEvent::Attached(vm_id),
        wake_vm: None,
        host_output: state.output.buffer_all(),
        input_overflow: None,
    }
}

#[cfg(any(feature = "browser-console", test, axtest))]
impl GuestConsoleMux {
    fn route_network_input(&self, vm_id: VMId, bytes: &[u8]) -> Option<bool> {
        self.core.route_network_input(vm_id, bytes)
    }
}

impl ConsoleCore {
    fn lock_state(&self) -> NoPreemptMutexGuard<'_, ConsoleState> {
        self.state.lock()
    }

    fn lock_output(&self) -> NoPreemptMutexGuard<'_, ()> {
        self.output_lock.lock()
    }

    fn backend_generations(&self) -> BTreeMap<VMId, BackendGeneration> {
        self.lock_state()
            .guests
            .iter()
            .filter_map(|(vm_id, guest)| {
                guest
                    .backend_generation
                    .map(|generation| (*vm_id, generation))
            })
            .collect()
    }

    fn create_serial_backend(self: &Arc<Self>, vm_id: VMId) -> Arc<GuestSerialBackend> {
        let _output_guard = self.lock_output();
        let generation = {
            let mut state = self.lock_state();
            state.next_backend_generation = state
                .next_backend_generation
                .checked_add(1)
                .expect("guest serial backend generation exhausted");
            let generation = BackendGeneration(state.next_backend_generation);
            let guest = GuestState {
                backend_identity: Some(generation),
                backend_generation: Some(generation),
                ..GuestState::default()
            };
            state.guests.insert(vm_id, guest);
            state.output_active.remove(&vm_id);
            state.output.reset_guest(vm_id);
            state.output.register_guest(vm_id);
            generation
        };
        Arc::new(GuestSerialBackend {
            vm_id,
            generation,
            core: self.clone(),
        })
    }

    fn read_guest_input(
        &self,
        vm_id: VMId,
        generation: BackendGeneration,
        buffer: &mut [u8],
    ) -> usize {
        let mut state = self.lock_state();
        let Some(guest) = state
            .guests
            .get_mut(&vm_id)
            .filter(|guest| guest.backend_generation == Some(generation))
        else {
            return 0;
        };
        let read_len = buffer.len().min(guest.input.len());
        for byte in &mut buffer[..read_len] {
            *byte = guest
                .input
                .pop_front()
                .expect("guest input queue length was checked");
        }
        if read_len != 0 {
            guest.input_overflow_reported = false;
        }
        read_len
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

    fn write_guest_output(&self, vm_id: VMId, generation: BackendGeneration, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let guard = self.lock_output();
        if !self
            .lock_state()
            .guests
            .get(&vm_id)
            .is_some_and(|guest| guest.backend_generation == Some(generation))
        {
            return false;
        }
        let tag = ((vm_id as u128) << 64) | generation.0 as u128;
        if super::host::queue_guest_output(tag, bytes) {
            return true;
        }
        drop(guard);
        self.replay_guest_output(vm_id, generation, bytes)
    }

    fn replay_guest_output(
        &self,
        vm_id: VMId,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> bool {
        if bytes.is_empty() {
            return false;
        }

        let _output_guard = self.lock_output();
        {
            let mut state = self.lock_state();
            let Some(guest) = state.guests.get(&vm_id) else {
                return false;
            };
            if guest.backend_generation != Some(generation) {
                return false;
            }
            // Reconciliation may discard formatting state while a live
            // backend still has queued output. Register in task context before
            // entering the allocation-free host-output transaction.
            state.output.register_guest(vm_id);
        }
        submit_host_transaction(|emit| {
            let mut state = self.lock_state();
            let multiple_running = state.running.len() > 1;
            let formatted =
                state
                    .output
                    .format_registered_into(vm_id, multiple_running, bytes, emit);
            debug_assert!(formatted, "validated backend output must be registered");
        });
        drop(_output_guard);
        true
    }

    #[cfg(any(feature = "browser-console", test, axtest))]
    fn route_network_input(&self, vm_id: VMId, bytes: &[u8]) -> Option<bool> {
        let mut state = self.lock_state();
        if !state.running.contains(&vm_id)
            || !state
                .guests
                .get(&vm_id)
                .is_some_and(|guest| guest.backend_generation.is_some())
        {
            return None;
        }
        Some(enqueue_guest_input(&mut state, vm_id, bytes))
    }
}

impl SerialBackend for GuestSerialBackend {
    fn write(&self, bytes: &[u8]) {
        if !self
            .core
            .write_guest_output(self.vm_id, self.generation, bytes)
        {
            return;
        }
        #[cfg(any(feature = "browser-console", all(test, axtest)))]
        if crate::network_console::guest_output_connected(self.vm_id) {
            crate::network_console::submit_guest_output(self.vm_id, bytes);
        }
    }

    fn read(&self, buffer: &mut [u8]) -> usize {
        self.core
            .read_guest_input(self.vm_id, self.generation, buffer)
    }
}

pub(crate) fn replay_guest_output(tag: u128, bytes: &[u8]) {
    let vm_id = (tag >> 64) as usize;
    let generation = BackendGeneration(tag as u64);
    GUEST_CONSOLE_MUX
        .core
        .replay_guest_output(vm_id, generation, bytes);
}

impl SerialBackendFactory for GuestSerialBackendFactory {
    fn create(&self) -> Arc<dyn SerialBackend> {
        self.core.create_serial_backend(self.vm_id)
    }
}

fn enqueue_guest_input(state: &mut ConsoleState, vm_id: VMId, bytes: &[u8]) -> bool {
    let guest = state.guests.entry(vm_id).or_default();
    let available = INPUT_QUEUE_CAPACITY.saturating_sub(guest.input.len());
    let accepted = bytes.len().min(available);
    guest.input.extend(bytes.iter().copied().take(accepted));
    if accepted == bytes.len() || guest.input_overflow_reported {
        return false;
    }
    guest.input_overflow_reported = true;
    true
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

/// Routes bytes from a VM-specific network endpoint without changing the
/// physical console foreground.
#[cfg(feature = "browser-console")]
pub(crate) fn route_network_input(vm_id: VMId, bytes: &[u8]) -> bool {
    let Some(overflowed) = GUEST_CONSOLE_MUX.route_network_input(vm_id, bytes) else {
        return false;
    };
    if overflowed {
        warn!("VM[{vm_id}] network console input queue overflowed; dropping bytes");
    }
    if !bytes.is_empty()
        && let Err(error) = crate::manager::AxvmManager::notify_vm(vm_id)
    {
        warn!("failed to wake VM[{vm_id}] for network console input: {error:#}");
    }
    true
}

/// Routes a complete host log record without exposing it to a guest UART.
///
/// Returns the line-safe bytes to display, or `None` when the record was
/// buffered behind an interactive guest until detach.
pub fn route_host_log(
    record: &[u8],
    dropped_records: usize,
    dropped_bytes: usize,
) -> Option<Vec<u8>> {
    GUEST_CONSOLE_MUX.route_host_log(record, dropped_records, dropped_bytes)
}

/// Opens a running VM interactively or replays a stopped VM's buffered output.
pub fn attach(vm_id: VMId) -> Result<ConsoleAttachment> {
    let Some(vm) = crate::manager::AxvmManager::vm_by_id(vm_id) else {
        bail!("VM[{vm_id}] not found");
    };
    match vm.status() {
        VmStatus::Running => {
            GUEST_CONSOLE_MUX.mark_running(vm_id);
            if !GUEST_CONSOLE_MUX.attach(vm_id) {
                bail!("VM[{vm_id}] is not available for console attachment");
            }
            Ok(ConsoleAttachment::Interactive)
        }
        VmStatus::Stopped => {
            if !GUEST_CONSOLE_MUX.replay_stopped(vm_id) {
                bail!("VM[{vm_id}] has no buffered console output");
            }
            Ok(ConsoleAttachment::Replayed)
        }
        status => bail!("VM[{vm_id}] console is unavailable while the VM is {status:?}"),
    }
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

/// Capture the backend identity that belongs to the current VM incarnation.
pub(crate) fn backend_identity(vm_id: VMId) -> Option<GuestBackendIdentity> {
    GUEST_CONSOLE_MUX.backend_identity(vm_id)
}

/// Remove console state only if it still belongs to the captured backend.
pub(crate) fn remove_if_backend(identity: GuestBackendIdentity) -> bool {
    GUEST_CONSOLE_MUX.remove_if_backend(identity)
}

/// Reconcile console attachment and prefixing against the actual VM registry.
pub fn reconcile_vm_states() -> Option<VMId> {
    // Capture backend identity first so a stale VM-registry snapshot cannot
    // invalidate a newer backend created with the same VM ID.
    let generations = GUEST_CONSOLE_MUX.core.backend_generations();
    let vm_states = crate::manager::AxvmManager::vm_list()
        .into_iter()
        .map(|vm| (vm.id(), vm.status()))
        .collect::<Vec<_>>();
    let running = vm_states
        .iter()
        .filter(|(_, status)| *status == VmStatus::Running)
        .filter_map(|(vm_id, _)| {
            generations
                .get(vm_id)
                .map(|generation| (*vm_id, *generation))
        });
    let output_active = vm_states
        .iter()
        .filter(|(_, status)| {
            matches!(
                status,
                VmStatus::Running | VmStatus::Pausing | VmStatus::Paused | VmStatus::Stopping
            )
        })
        .filter_map(|(vm_id, _)| {
            generations
                .get(vm_id)
                .map(|generation| (*vm_id, *generation))
        });
    let terminal = vm_states
        .iter()
        .filter(|(_, status)| {
            matches!(
                status,
                VmStatus::Stopped | VmStatus::Destroying | VmStatus::Destroyed | VmStatus::Failed
            )
        })
        .filter_map(|(vm_id, _)| {
            generations
                .get(vm_id)
                .map(|generation| (*vm_id, *generation))
        });
    GUEST_CONSOLE_MUX.set_vm_states(running, output_active, terminal)
}

/// Return the currently attached guest, if any.
pub fn attached_vm() -> Option<VMId> {
    GUEST_CONSOLE_MUX.attached_vm()
}

#[cfg(any(test, axtest))]
mod tests;
