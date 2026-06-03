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

//! Host IRQ management APIs for the AxVisor hypervisor.

/// The host IRQ callback type.
pub type IrqHandler = fn(usize);

/// The API trait for host IRQ dispatch and registration.
#[crate::api_def]
pub trait IrqIf {
    /// Dispatch a host IRQ or VM-exit delivered interrupt vector through the
    /// underlying runtime's IRQ handling path.
    fn handle_irq(vector: usize) -> bool;

    /// Register a host IRQ handler for `vector`.
    ///
    /// Returns `true` if the handler was registered, `false` otherwise.
    fn register_irq_handler(vector: usize, handler: IrqHandler) -> bool;
}
