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

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Reads one host byte per emulated UART interrupt delivery.
pub const CONSOLE_INPUT_READ_SIZE: usize = 1;

const DISCONNECTED: u64 = 0;
#[cfg(any(test, target_arch = "aarch64"))]
const CTRL_RIGHT_BRACKET: u8 = 0x1d;

/// An exact snapshot of one VM console connection generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionToken {
    pub vm_id: usize,
    pub generation: u32,
}

/// Failure to publish a new console connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectError {
    AlreadyConnected,
    VmIdOutOfRange,
}

/// The event owned by the caller that successfully detached a connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetachEvent {
    Detached { vm_id: usize },
}

/// Generation-safe ownership state for one attached VM console.
pub struct ConsoleConnectionState {
    current: AtomicU64,
    next_generation: AtomicU32,
}

impl ConsoleConnectionState {
    pub const fn new() -> Self {
        Self {
            current: AtomicU64::new(DISCONNECTED),
            next_generation: AtomicU32::new(0),
        }
    }

    pub fn connect(&self, vm_id: usize) -> Result<ConnectionToken, ConnectError> {
        let encoded_vm_id = vm_id
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(ConnectError::VmIdOutOfRange)?;
        let token = ConnectionToken {
            vm_id,
            generation: self.allocate_generation(),
        };
        let packed = pack_token(encoded_vm_id, token.generation);
        self.current
            .compare_exchange(DISCONNECTED, packed, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| token)
            .map_err(|_| ConnectError::AlreadyConnected)
    }

    pub fn current(&self) -> Option<ConnectionToken> {
        unpack_token(self.current.load(Ordering::Acquire))
    }

    pub fn detach(&self, token: ConnectionToken) -> Option<DetachEvent> {
        let encoded_vm_id = u32::try_from(token.vm_id.checked_add(1)?).ok()?;
        let packed = pack_token(encoded_vm_id, token.generation);
        self.current
            .compare_exchange(packed, DISCONNECTED, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| DetachEvent::Detached { vm_id: token.vm_id })
    }

    fn allocate_generation(&self) -> u32 {
        // Generation uniqueness does not publish data; `current` provides Acquire/Release ordering.
        let mut current = self.next_generation.load(Ordering::Relaxed);
        loop {
            let next = if current == u32::MAX { 1 } else { current + 1 };
            match self.next_generation.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }
}

impl Default for ConsoleConnectionState {
    fn default() -> Self {
        Self::new()
    }
}

fn pack_token(encoded_vm_id: u32, generation: u32) -> u64 {
    (u64::from(generation) << 32) | u64::from(encoded_vm_id)
}

fn unpack_token(packed: u64) -> Option<ConnectionToken> {
    if packed == DISCONNECTED {
        return None;
    }
    let encoded_vm_id = packed as u32;
    Some(ConnectionToken {
        vm_id: encoded_vm_id.checked_sub(1)? as usize,
        generation: (packed >> 32) as u32,
    })
}

/// Splits input before the first Ctrl+] detach character.
#[cfg(any(test, target_arch = "aarch64"))]
pub fn split_console_input(input: &[u8]) -> (&[u8], bool) {
    match input.iter().position(|byte| *byte == CTRL_RIGHT_BRACKET) {
        Some(index) => (&input[..index], true),
        None => (input, false),
    }
}
