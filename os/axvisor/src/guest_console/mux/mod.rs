//! Host-console multiplexing for mandatory guest virtual serial devices.

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    vec::Vec,
};

use anyhow::{Result, bail};
use axvm::{SerialBackend, SerialBackendFactory, VMId, VmStatus};
use core::ops::Bound::{Excluded, Unbounded};
use std::sync::{LazyLock, Mutex, MutexGuard};

use super::host::write_host_bytes;

mod output;

use output::GuestOutputMux;

// Terminals encode Alt as a leading ESC, so Ctrl-Alt-[ is ESC ESC and
// Ctrl-Alt-] is ESC followed by the Ctrl-] byte (group separator).
const ESC: u8 = 0x1b;
const CTRL_H: u8 = 0x08;
const CTRL_RIGHT_BRACKET: u8 = 0x1d;
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

#[derive(Debug)]
struct RoutedInput {
    event: ConsoleInputEvent,
    wake_vm: Option<VMId>,
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
    /// Serializes host writes with backend replacement and invalidation.
    ///
    /// Code that needs both locks must acquire `output_lock` before `state`.
    output_lock: Mutex<()>,
}

#[derive(Debug, Default)]
struct ConsoleState {
    guests: BTreeMap<VMId, GuestState>,
    running: BTreeSet<VMId>,
    attached: Option<VMId>,
    last_attached: Option<VMId>,
    shortcut_prefix_pending: bool,
    output: GuestOutputMux,
    next_backend_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BackendGeneration(u64);

#[derive(Debug, Default)]
struct GuestState {
    backend_generation: Option<BackendGeneration>,
    input: VecDeque<u8>,
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
                state: Mutex::new(ConsoleState::default()),
                output_lock: Mutex::new(()),
            }),
        }
    }

    fn set_running(&self, running: impl IntoIterator<Item = VMId>) -> Option<VMId> {
        let mut state = self.core.lock_state();
        state.running.clear();
        for vm_id in running {
            state.running.insert(vm_id);
            state.guests.entry(vm_id).or_default();
        }

        let detached = state
            .attached
            .filter(|vm_id| !state.running.contains(vm_id));
        if detached.is_some() {
            state.attached = None;
            state.shortcut_prefix_pending = false;
        }
        let ConsoleState {
            running, output, ..
        } = &mut *state;
        output.reconcile_running(running);
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
            guest.input.clear();
        }
        state.output.reset_guest(vm_id);
        if state.attached == Some(vm_id) {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            return true;
        }
        false
    }

    fn remove(&self, vm_id: VMId) -> bool {
        let _output_guard = self.core.lock_output();
        let mut state = self.core.lock_state();
        state.running.remove(&vm_id);
        state.guests.remove(&vm_id);
        state.output.reset_guest(vm_id);
        if state.last_attached == Some(vm_id) {
            state.last_attached = None;
        }
        if state.attached == Some(vm_id) {
            state.attached = None;
            state.shortcut_prefix_pending = false;
            return true;
        }
        false
    }

    fn attach_default(&self, running: impl IntoIterator<Item = VMId>) -> Option<VMId> {
        self.set_running(running);
        let mut state = self.core.lock_state();
        let vm_id = state.running.first().copied()?;
        state.attached = Some(vm_id);
        state.last_attached = Some(vm_id);
        Some(vm_id)
    }

    fn attach(&self, vm_id: VMId) -> bool {
        let mut state = self.core.lock_state();
        if !state.running.contains(&vm_id) {
            return false;
        }
        state.attached = Some(vm_id);
        state.last_attached = Some(vm_id);
        state.shortcut_prefix_pending = false;
        true
    }

    fn attached_vm(&self) -> Option<VMId> {
        self.core.lock_state().attached
    }

    fn route_host_byte(&self, byte: u8) -> RoutedInput {
        let mut state = self.core.lock_state();

        if state.shortcut_prefix_pending {
            state.shortcut_prefix_pending = false;
            return match byte {
                CTRL_H => match state.attached.take() {
                    Some(vm_id) => RoutedInput {
                        event: ConsoleInputEvent::Detached(vm_id),
                        wake_vm: None,
                    },
                    None => RoutedInput {
                        event: ConsoleInputEvent::Consumed,
                        wake_vm: None,
                    },
                },
                ESC => switch_guest(&mut state, GuestSwitchDirection::Previous),
                CTRL_RIGHT_BRACKET => switch_guest(&mut state, GuestSwitchDirection::Next),
                byte => match state.attached {
                    Some(vm_id) => {
                        enqueue_guest_input(&mut state, vm_id, &[ESC, byte]);
                        RoutedInput {
                            event: ConsoleInputEvent::Consumed,
                            wake_vm: Some(vm_id),
                        }
                    }
                    None => RoutedInput {
                        event: ConsoleInputEvent::ShellSequence(ESC, byte),
                        wake_vm: None,
                    },
                },
            };
        }

        if byte == ESC {
            state.shortcut_prefix_pending = true;
            return RoutedInput {
                event: ConsoleInputEvent::Consumed,
                wake_vm: None,
            };
        }

        match state.attached {
            Some(vm_id) => {
                enqueue_guest_input(&mut state, vm_id, &[byte]);
                RoutedInput {
                    event: ConsoleInputEvent::Consumed,
                    wake_vm: Some(vm_id),
                }
            }
            None => RoutedInput {
                event: ConsoleInputEvent::ShellByte(byte),
                wake_vm: None,
            },
        }
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
        };
    };

    state.attached = Some(vm_id);
    state.last_attached = Some(vm_id);
    RoutedInput {
        event: ConsoleInputEvent::Attached(vm_id),
        wake_vm: None,
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
        let generation = {
            let mut state = self.lock_state();
            state.next_backend_generation = state
                .next_backend_generation
                .checked_add(1)
                .expect("guest serial backend generation exhausted");
            let generation = BackendGeneration(state.next_backend_generation);
            state.guests.insert(
                vm_id,
                GuestState {
                    backend_generation: Some(generation),
                    ..GuestState::default()
                },
            );
            state.output.reset_guest(vm_id);
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
        read_len
    }

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

    fn write_guest_output(&self, vm_id: VMId, generation: BackendGeneration, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        let _output_guard = self.lock_output();
        if let Some(output) = self.format_guest_output(vm_id, generation, bytes) {
            write_host_bytes(&output);
        }
    }
}

impl SerialBackend for GuestSerialBackend {
    fn write(&self, bytes: &[u8]) {
        self.core
            .write_guest_output(self.vm_id, self.generation, bytes);
    }

    fn read(&self, buffer: &mut [u8]) -> usize {
        self.core
            .read_guest_input(self.vm_id, self.generation, buffer)
    }
}

impl SerialBackendFactory for GuestSerialBackendFactory {
    fn create(&self) -> Arc<dyn SerialBackend> {
        self.core.create_serial_backend(self.vm_id)
    }
}

fn enqueue_guest_input(state: &mut ConsoleState, vm_id: VMId, bytes: &[u8]) {
    let guest = state.guests.entry(vm_id).or_default();
    let available = INPUT_QUEUE_CAPACITY.saturating_sub(guest.input.len());
    guest.input.extend(bytes.iter().copied().take(available));
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

#[cfg(test)]
mod tests;
