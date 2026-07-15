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

//! VM-owned interrupt line routing.

use alloc::sync::Arc;

use ax_errno::{AxResult, ax_err};
use axdevice::IrqResolver;
use axdevice_base::{InterruptTriggerMode, IrqLine, IrqLineId, IrqSink};
use axvm_types::VMInterruptMode;

/// Host platform hook for registering the RISC-V physical IRQ injector.
#[ax_crate_interface::def_interface]
pub trait RiscvPlatformIrqInjectorIf {
    /// Registers a callback that forwards a physical IRQ line into the current guest.
    fn register_virtual_irq_injector(injector: fn(usize) -> bool);

    /// Routes physical PLIC IRQs that may be forwarded to a guest toward the vCPU CPU.
    fn set_virtual_irq_targets(cpu_id: usize, irq_sources: &[u32]);
}

#[expect(
    dead_code,
    reason = "the RISC-V architecture backend is not compiled for this target"
)]
pub(crate) fn register_riscv_virtual_irq_injector(injector: fn(usize) -> bool) {
    ax_crate_interface::call_interface!(RiscvPlatformIrqInjectorIf::register_virtual_irq_injector(
        injector
    ));
}

#[expect(
    dead_code,
    reason = "the RISC-V architecture backend is not compiled for this target"
)]
pub(crate) fn set_riscv_virtual_irq_targets(cpu_id: usize, irq_sources: &[u32]) {
    ax_crate_interface::call_interface!(RiscvPlatformIrqInjectorIf::set_virtual_irq_targets(
        cpu_id,
        irq_sources
    ));
}

/// Resolves device interrupt lines against one VM's interrupt backend.
///
/// The fabric owns only the backend capability. It never owns or references the
/// containing VM, so devices can retain [`IrqLine`] objects without creating an
/// `AxVM -> device -> IRQ -> AxVM` reference cycle.
#[derive(Clone)]
pub struct InterruptFabric {
    mode: VMInterruptMode,
    sink: Option<Arc<dyn IrqSink>>,
}

impl InterruptFabric {
    /// Creates a fabric without an interrupt backend.
    pub const fn new(mode: VMInterruptMode) -> Self {
        Self { mode, sink: None }
    }

    /// Creates a fabric that routes lines to `sink`.
    pub fn with_sink(mode: VMInterruptMode, sink: Arc<dyn IrqSink>) -> AxResult<Self> {
        if mode == VMInterruptMode::NoIrq {
            return ax_err!(
                InvalidInput,
                "a VM configured with interrupt_mode=no_irq cannot install an IRQ backend"
            );
        }
        Ok(Self {
            mode,
            sink: Some(sink),
        })
    }

    /// Returns the VM interrupt mode associated with this fabric.
    pub const fn mode(&self) -> VMInterruptMode {
        self.mode
    }

    /// Returns whether this fabric has an interrupt backend.
    pub const fn has_backend(&self) -> bool {
        self.sink.is_some()
    }

    fn sink_for_line(&self, line: usize) -> AxResult<&Arc<dyn IrqSink>> {
        let Some(sink) = &self.sink else {
            if self.mode == VMInterruptMode::NoIrq {
                return ax_err!(
                    InvalidInput,
                    format_args!("cannot signal IRQ line {line}: the VM interrupt mode is NoIrq")
                );
            }
            return ax_err!(
                Unsupported,
                format_args!("cannot signal IRQ line {line}: no VM interrupt backend is installed")
            );
        };
        Ok(sink)
    }

    /// Sets the asserted state of a VM-local interrupt line.
    pub fn set_level(&self, line: usize, asserted: bool) -> AxResult {
        self.sink_for_line(line)?
            .set_level(IrqLineId(line), asserted)
    }

    /// Delivers one pulse on a VM-local interrupt line.
    pub fn pulse(&self, line: usize) -> AxResult {
        self.sink_for_line(line)?.pulse(IrqLineId(line))
    }

    pub(crate) fn validate_mode(&self, mode: VMInterruptMode) -> AxResult {
        if self.mode != mode {
            return ax_err!(
                InvalidInput,
                format_args!(
                    "interrupt fabric mode {:?} does not match VM interrupt mode {:?}",
                    self.mode, mode
                )
            );
        }
        Ok(())
    }
}

impl Default for InterruptFabric {
    fn default() -> Self {
        Self::new(VMInterruptMode::NoIrq)
    }
}

impl IrqResolver for InterruptFabric {
    fn resolve_irq(&self, line: usize, trigger: InterruptTriggerMode) -> AxResult<IrqLine> {
        Ok(IrqLine::new(
            IrqLineId(line),
            trigger,
            self.sink_for_line(line)?.clone(),
        ))
    }
}
