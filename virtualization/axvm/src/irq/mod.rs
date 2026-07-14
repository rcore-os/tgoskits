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

use axdevice::{DeviceManagerResult, IrqResolver};
use axdevice_base::{InterruptTriggerMode, IrqError, IrqLine, IrqLineId, IrqResult, IrqSink};
use axvm_types::VMInterruptMode;

use crate::{AxVmResult, ax_err};

/// Opaque claim for one physical PLIC source completed under a host IRQ-off
/// capture and kept masked until its guest completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RiscvPhysicalIrqClaim {
    source: u32,
    generation: u64,
}

impl RiscvPhysicalIrqClaim {
    const MAX_GENERATION: u64 = u64::MAX >> 2;

    /// Creates a claim from the embedding platform's canonical generation.
    pub const fn try_new(source: u32, generation: u64) -> Option<Self> {
        if source == 0 || generation == 0 || generation > Self::MAX_GENERATION {
            None
        } else {
            Some(Self { source, generation })
        }
    }

    /// Returns the PLIC source ID delivered to the software vPLIC.
    pub const fn source(self) -> u32 {
        self.source
    }

    /// Returns the platform generation that rejects stale guest completions.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Opaque callback capability whose construction acknowledges the hard-IRQ
/// execution contract.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct RiscvHardIrqSink(unsafe extern "C" fn(u32, u64) -> bool);

impl RiscvHardIrqSink {
    /// Wraps a shutdown-stable hard-IRQ callback.
    ///
    /// # Safety
    ///
    /// The callback and all referenced state must remain valid until shutdown.
    /// It must not allocate, free, block, acquire a lock, invoke guest code, or
    /// unwind. It may only publish preallocated atomic state and perform a
    /// hard-IRQ-safe direct thread wake.
    pub const unsafe fn new(callback: unsafe extern "C" fn(u32, u64) -> bool) -> Self {
        Self(callback)
    }

    /// Returns the callback after its safety contract was acknowledged.
    pub const fn callback(self) -> unsafe extern "C" fn(u32, u64) -> bool {
        self.0
    }
}

/// Result category for VM-wide physical PLIC route installation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RiscvPlatformIrqRouteStatus {
    /// Every source is leased and published while still masked.
    Prepared          = 0,
    /// Every source was activated after route publication.
    Activated         = 1,
    /// A source was invalid or duplicated.
    InvalidSource     = 2,
    /// A different host CPU already owns the monitor-wide route.
    ConflictingTarget = 3,
    /// The physical PLIC domain is unavailable.
    DomainUnavailable = 4,
    /// The platform failed to lease one physical endpoint.
    LeaseFailed       = 5,
    /// Immutable endpoint ownership conflicts with an existing route.
    EndpointConflict  = 6,
    /// The same canonical route is in another transaction phase.
    TransactionBusy   = 7,
    /// A different canonical VM/CPU/source owner is reserved or active.
    RouteConflict     = 8,
}

/// Typed route transaction result returned across the platform interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RiscvPlatformIrqRouteResult {
    /// Route result category.
    pub status: RiscvPlatformIrqRouteStatus,
    /// First failing PLIC source, or zero for a route-wide failure.
    pub source: u32,
}

impl RiscvPlatformIrqRouteResult {
    /// Returns whether all sources are published while still masked.
    pub const fn is_prepared(self) -> bool {
        matches!(self.status, RiscvPlatformIrqRouteStatus::Prepared)
    }

    /// Returns whether every requested source was activated.
    pub const fn is_activated(self) -> bool {
        matches!(self.status, RiscvPlatformIrqRouteStatus::Activated)
    }
}

/// Host platform capability for RISC-V physical IRQ ownership transfer.
#[ax_crate_interface::def_interface]
pub trait RiscvPlatformIrqIf {
    /// Installs the fixed hard-IRQ publication sink used while the owner vCPU
    /// is not currently bound to a host CPU.
    fn register_sink(sink: RiscvHardIrqSink) -> bool;

    /// Claims, masks, and completes the current physical interrupt while host
    /// IRQ delivery is still disabled. Host-owned sources are handled inside
    /// the platform and return `None`.
    fn claim_and_mask(vector: usize) -> Option<RiscvPhysicalIrqClaim>;

    /// Unmasks a source after the guest completed its software PLIC claim.
    /// The caller must be pinned to `current_cpu` with local IRQ delivery
    /// disabled; the platform rejects a CPU different from the leased target.
    fn unmask(claim: RiscvPhysicalIrqClaim, current_cpu: usize) -> bool;

    /// Routes physical PLIC IRQs that may be forwarded to a guest toward the vCPU CPU.
    fn prepare_virtual_irq_targets(
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &ax_cpu_local::CpuPin,
    ) -> RiscvPlatformIrqRouteResult;

    /// Activates a previously prepared batch after the owner route and wake
    /// target are globally visible.
    fn activate_virtual_irq_targets(
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &ax_cpu_local::CpuPin,
    ) -> RiscvPlatformIrqRouteResult;
}

/// RISC-V host-IRQ routing capability supplied by the embedding monitor.
///
/// Keeping the crate-interface invocation behind this object prevents
/// architecture backends from depending on macro-generated ABI symbols.
pub struct RiscvPlatformIrq;

impl RiscvPlatformIrq {
    /// Installs the monitor-wide hard-IRQ publication sink.
    /// # Safety
    ///
    /// `sink` must remain valid until shutdown and obey the allocation-free,
    /// lock-free, non-blocking, non-unwinding hard-IRQ contract.
    pub unsafe fn register_sink(sink: unsafe extern "C" fn(u32, u64) -> bool) -> bool {
        // SAFETY: the caller owns the callback contract stated above.
        let sink = unsafe { RiscvHardIrqSink::new(sink) };
        ax_crate_interface::call_interface!(RiscvPlatformIrqIf::register_sink(sink))
    }

    /// Captures the current guest-owned PLIC source before host IRQ restore.
    pub fn claim_and_mask(vector: usize) -> Option<RiscvPhysicalIrqClaim> {
        ax_crate_interface::call_interface!(RiscvPlatformIrqIf::claim_and_mask(vector))
    }

    /// Releases a masked physical source after guest completion.
    pub fn unmask(claim: RiscvPhysicalIrqClaim, current_cpu: usize) -> bool {
        ax_crate_interface::call_interface!(RiscvPlatformIrqIf::unmask(claim, current_cpu))
    }

    /// Routes guest-owned physical PLIC sources toward one host CPU.
    pub fn prepare_virtual_irq_targets(
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &ax_cpu_local::CpuPin,
    ) -> RiscvPlatformIrqRouteResult {
        ax_crate_interface::call_interface!(RiscvPlatformIrqIf::prepare_virtual_irq_targets(
            cpu_id,
            irq_sources,
            cpu_pin
        ))
    }

    /// Activates all prepared physical PLIC endpoints.
    pub fn activate_virtual_irq_targets(
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &ax_cpu_local::CpuPin,
    ) -> RiscvPlatformIrqRouteResult {
        ax_crate_interface::call_interface!(RiscvPlatformIrqIf::activate_virtual_irq_targets(
            cpu_id,
            irq_sources,
            cpu_pin
        ))
    }
}

/// Resolves device interrupt lines against one VM's interrupt backend.
///
/// The fabric owns only the backend capability. It never owns or references the
/// containing VM, so devices can retain [`IrqLine`] objects without creating an
/// `AxVM -> device -> IRQ -> AxVM` reference cycle.
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
    pub fn with_sink(mode: VMInterruptMode, sink: Arc<dyn IrqSink>) -> AxVmResult<Self> {
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

    fn sink_for_line(&self, line: usize) -> IrqResult<&Arc<dyn IrqSink>> {
        let Some(sink) = &self.sink else {
            if self.mode == VMInterruptMode::NoIrq {
                return Err(IrqError::InvalidLine {
                    line: IrqLineId(line),
                    operation: "resolve",
                    detail: "the VM is configured without interrupt delivery".into(),
                });
            }
            return Err(IrqError::Unsupported {
                line: IrqLineId(line),
                operation: "resolve",
                detail: "no interrupt backend is installed".into(),
            });
        };
        Ok(sink)
    }

    /// Sets the asserted state of a VM-local interrupt line.
    pub fn set_level(&self, line: usize, asserted: bool) -> AxVmResult {
        self.sink_for_line(line)?
            .set_level(IrqLineId(line), asserted)?;
        Ok(())
    }

    /// Delivers one pulse on a VM-local interrupt line.
    pub fn pulse(&self, line: usize) -> AxVmResult {
        self.sink_for_line(line)?.pulse(IrqLineId(line))?;
        Ok(())
    }

    pub(crate) fn validate_mode(&self, mode: VMInterruptMode) -> AxVmResult {
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
    fn resolve_irq(
        &self,
        line: usize,
        trigger: InterruptTriggerMode,
    ) -> DeviceManagerResult<IrqLine> {
        Ok(IrqLine::new(
            IrqLineId(line),
            trigger,
            self.sink_for_line(line)?.clone(),
        ))
    }
}
