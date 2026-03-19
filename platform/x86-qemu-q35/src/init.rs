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

use axplat::init::InitIf;

struct InitIfImpl;

#[impl_plat_interface]
impl InitIf for InitIfImpl {
    /// Initializes the platform at the early stage for the primary core.
    ///
    /// This function should be called immediately after the kernel has booted,
    /// and performed earliest platform configuration and initialization (e.g.,
    /// early console, clocking).
    fn init_early(_cpu_id: usize, mbi: usize) {
        axcpu::init::init_trap();
        crate::console::init();
        crate::time::init_early();
        crate::mem::init(mbi);
    }

    /// Initializes the platform at the early stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_early_secondary(_cpu_id: usize) {
        axcpu::init::init_trap();
    }

    /// Initializes the platform at the later stage for the primary core.
    ///
    /// This function should be called after the kernel has done part of its
    /// initialization (e.g, logging, memory management), and finalized the rest of
    /// platform configuration and initialization.
    fn init_later(_cpu_id: usize, _arg: usize) {
        crate::apic::init_primary();
        crate::time::init_primary();
    }

    /// Initializes the platform at the later stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_later_secondary(_cpu_id: usize) {
        crate::apic::init_secondary();
        crate::time::init_secondary();
    }
}
