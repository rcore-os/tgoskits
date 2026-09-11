//! Architecture-neutral PCI root-owned config and BAR decode state.
//!
//! Frontends pass already-decoded BDF/config accesses into this object. The
//! root owns only conventional config bytes and BAR routes; endpoint objects,
//! runtime identities, and lifecycle callbacks are introduced by later
//! integration layers.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{fmt, ops::Range};

use ax_sync::SpinLock;
use axdevice_base::DeviceId;

use super::{
    EndpointRouteToken, FOUR_GIB, PciBarIndex, PciBdf, PciCommandState, PciConfigReadEffect,
    PciConfigWriteEffect, PciError, PciResult, ResolvedPciTopology,
    config::{BarWriteAction, FunctionState},
    config_layout::{
        CONFIG_COMMAND_OFFSET, CONFIG_COMMAND_SIZE, CONFIG_SPACE_SIZE, CONFIG_STATUS_OFFSET,
        STATUS_INTERRUPT_PENDING,
    },
};
use crate::{AccessWidth, ConfigOffset};

pub(crate) enum PciConfigReadOutcome {
    Value(u64),
    DynamicStatus {
        token: EndpointRouteToken,
        command: PciCommandState,
        value: u64,
        interrupt_status_mask: u64,
    },
    Effect {
        token: EndpointRouteToken,
        command: PciCommandState,
        effect: Box<PciConfigReadEffect>,
    },
}

pub(crate) enum PciConfigWriteOutcome {
    Complete,
    Effect {
        token: EndpointRouteToken,
        command: PciCommandState,
        effect: Box<PciConfigWriteEffect>,
    },
    CommandChanged {
        token: Option<EndpointRouteToken>,
        command: PciCommandState,
    },
}

/// Shared root state for one frozen PCI topology.
pub struct PciRootState {
    topology: Arc<ResolvedPciTopology>,
    state: SpinLock<RootState>,
}

/// Owns a pending endpoint binding until it is either published or dropped.
///
/// The reservation keeps command writes blocked between the command snapshot
/// and root publication. Its destructor removes that marker on every failure
/// path, so callers cannot leave a function permanently in the provisional
/// binding state by forgetting a manual cancellation.
pub(crate) struct EndpointBindingReservation<'a> {
    root: &'a PciRootState,
    function: String,
    bdf: PciBdf,
    command: PciCommandState,
    committed: bool,
}

impl EndpointBindingReservation<'_> {
    pub(crate) const fn command(&self) -> PciCommandState {
        self.command
    }

    /// Publishes the endpoint route and consumes this reservation.
    pub(crate) fn commit(mut self, token: EndpointRouteToken) -> PciResult {
        let mut state = self.root.state.lock_irqsave();
        if state.bindings.contains_key(&self.bdf) {
            return Err(PciError::FunctionAlreadyBound {
                function: self.function.clone(),
            });
        }
        if !state.pending_bindings.remove(&self.bdf) {
            return Err(PciError::BindingReservationExpired {
                function: self.function.clone(),
            });
        }
        state.bindings.insert(self.bdf, token);
        self.committed = true;
        Ok(())
    }
}

impl Drop for EndpointBindingReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.root
                .state
                .lock_irqsave()
                .pending_bindings
                .remove(&self.bdf);
        }
    }
}

impl PciRootState {
    /// Creates power-on config and BAR decode state from a frozen topology.
    pub fn new(topology: Arc<ResolvedPciTopology>) -> Self {
        let functions = topology
            .function_plans()
            .iter()
            .map(|function| {
                FunctionState::new(
                    function.bdf(),
                    function.power_on.clone(),
                    function.bars(),
                    function.intx().is_some(),
                )
            })
            .collect();
        Self {
            state: SpinLock::new(RootState {
                functions,
                bindings: BTreeMap::new(),
                pending_bindings: BTreeSet::new(),
            }),
            topology,
        }
    }

    /// Returns the immutable topology that produced this root state.
    pub fn topology(&self) -> &ResolvedPciTopology {
        &self.topology
    }

    pub(crate) fn topology_arc(&self) -> &Arc<ResolvedPciTopology> {
        &self.topology
    }

    /// Reads one conventional config access.
    ///
    /// An absent BDF reads as all ones for the requested width.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidConfigAccess`] for qword, misaligned, or
    /// out-of-range accesses.
    pub fn read_config(
        &self,
        bdf: PciBdf,
        offset: ConfigOffset,
        width: AccessWidth,
    ) -> PciResult<u64> {
        match self.prepare_read_config(bdf, offset, width)? {
            PciConfigReadOutcome::Value(value) => Ok(value),
            PciConfigReadOutcome::DynamicStatus { .. } => Err(PciError::ConfigEffectUnavailable {
                detail: "an endpoint binding is required for dynamic interrupt status",
            }),
            PciConfigReadOutcome::Effect { .. } => Err(PciError::ConfigEffectUnavailable {
                detail: "an endpoint binding is required for this config read",
            }),
        }
    }

    /// Applies one conventional config write.
    ///
    /// Writes to absent functions or read-only fields have no effect. BAR
    /// probe and relocation writes are classified after merging the complete
    /// dword; invalid relocations preserve both config readback and decode.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidConfigAccess`] under the same conditions as
    /// [`PciRootState::read_config`].
    pub fn write_config(
        &self,
        bdf: PciBdf,
        offset: ConfigOffset,
        width: AccessWidth,
        value: u64,
    ) -> PciResult {
        match self.prepare_write_config(bdf, offset, width, value)? {
            PciConfigWriteOutcome::Complete | PciConfigWriteOutcome::CommandChanged { .. } => {
                Ok(())
            }
            PciConfigWriteOutcome::Effect { .. } => Err(PciError::ConfigEffectUnavailable {
                detail: "an endpoint binding is required for this config write",
            }),
        }
    }

    pub(crate) fn prepare_read_config(
        &self,
        bdf: PciBdf,
        offset: ConfigOffset,
        width: AccessWidth,
    ) -> PciResult<PciConfigReadOutcome> {
        let (offset, size) = offset.validate_access(width)?;
        let (token, command, capability, effect, relative, snapshot) = {
            let state = self.state.lock_irqsave();
            let Some(function_index) = state.function_index(bdf) else {
                return Ok(PciConfigReadOutcome::Value(all_ones(size)));
            };
            let function = &state.functions[function_index];
            let Some((capability, effect, relative, snapshot)) =
                function.config_effect(offset, size, width, false)?
            else {
                let value = function.read(offset, size);
                let Some(interrupt_status_mask) = function
                    .has_intx()
                    .then(|| interrupt_status_mask(offset, size))
                    .flatten()
                else {
                    return Ok(PciConfigReadOutcome::Value(value));
                };
                let Some(token) = state
                    .bindings
                    .get(&bdf)
                    .and_then(EndpointRouteToken::snapshot_if_admitted)
                else {
                    return Ok(PciConfigReadOutcome::Value(value));
                };
                return Ok(PciConfigReadOutcome::DynamicStatus {
                    token,
                    command: function.command_state(),
                    value,
                    interrupt_status_mask,
                });
            };
            let token = state
                .bindings
                .get(&bdf)
                .and_then(EndpointRouteToken::snapshot_if_admitted)
                .ok_or(PciError::ConfigEffectUnavailable {
                    detail: "an admitted endpoint binding is required for this config read",
                })?;
            (
                token,
                function.command_state(),
                capability,
                effect,
                relative,
                snapshot,
            )
        };
        Ok(PciConfigReadOutcome::Effect {
            token,
            command,
            effect: Box::new(PciConfigReadEffect::new(
                capability,
                effect.effect(),
                relative,
                width,
                snapshot,
                command,
            )),
        })
    }

    pub(crate) fn config_access_intersects_effect(
        &self,
        bdf: PciBdf,
        offset: ConfigOffset,
        width: AccessWidth,
    ) -> PciResult<bool> {
        let size = width.size();
        let offset = usize::from(offset.value());
        let end = offset
            .checked_add(size)
            .ok_or(PciError::InvalidConfigAccess {
                offset: offset as u16,
                width,
                detail: "config access range overflows",
            })?;
        if end > CONFIG_SPACE_SIZE {
            return Err(PciError::InvalidConfigAccess {
                offset: offset as u16,
                width,
                detail: "config access leaves the function boundary",
            });
        }
        let state = self.state.lock_irqsave();
        Ok(state
            .function_index(bdf)
            .is_some_and(|index| state.functions[index].intersects_config_effect(offset, size)))
    }

    pub(crate) fn prepare_write_config(
        &self,
        bdf: PciBdf,
        offset: ConfigOffset,
        width: AccessWidth,
        value: u64,
    ) -> PciResult<PciConfigWriteOutcome> {
        let (offset, size) = offset.validate_access(width)?;
        let (token, command, capability, effect, relative, snapshot) = {
            let mut state = self.state.lock_irqsave();
            let Some(function_index) = state.function_index(bdf) else {
                return Ok(PciConfigWriteOutcome::Complete);
            };
            let access_end = offset + size;
            let command_end = CONFIG_COMMAND_OFFSET + CONFIG_COMMAND_SIZE;
            if state.pending_bindings.contains(&bdf)
                && offset < command_end
                && access_end > CONFIG_COMMAND_OFFSET
            {
                let function = self
                    .topology
                    .functions()
                    .find(|function| function.bdf() == bdf)
                    .ok_or_else(|| PciError::UnknownFunction {
                        function: bdf.to_string(),
                    })?;
                return Err(PciError::BindingInProgress {
                    function: function.id().to_string(),
                });
            }
            if let Some((capability, effect, relative, snapshot)) =
                state.functions[function_index].config_effect(offset, size, width, true)?
            {
                let token = state
                    .bindings
                    .get(&bdf)
                    .and_then(EndpointRouteToken::snapshot_if_admitted)
                    .ok_or(PciError::ConfigEffectUnavailable {
                        detail: "an admitted endpoint binding is required for this config write",
                    })?;
                (
                    token,
                    state.functions[function_index].command_state(),
                    capability,
                    effect,
                    relative,
                    snapshot,
                )
            } else {
                let bar_action =
                    state.functions[function_index].prepare_bar_write(offset, size, value);
                if let Some(action) = bar_action {
                    match action {
                        BarWriteAction::Probe { bar } => {
                            state.functions[function_index].apply_probe(bar)
                        }
                        BarWriteAction::Relocate { bar, candidate } => {
                            let accepted = state.bar_address_available(
                                self.topology.memory_aperture(),
                                function_index,
                                bar,
                                candidate,
                            );
                            state.functions[function_index]
                                .finish_relocation(bar, accepted.then_some(candidate));
                        }
                    }
                    return Ok(PciConfigWriteOutcome::Complete);
                }
                let previous = state.functions[function_index].command_state();
                let command_changes =
                    state.functions[function_index].command_write_changes(offset, size, value);
                if command_changes {
                    // Check the revision before mutating the config image so
                    // exhaustion cannot leave a guest-visible partial write.
                    state.functions[function_index]
                        .command_state()
                        .revision()
                        .next()?;
                }
                state.functions[function_index].write_non_bar(offset, size, value);
                if command_changes {
                    state.functions[function_index].bump_command_revision()?;
                }
                let command = state.functions[function_index].command_state();
                let command_changed = previous.bus_master_enable() != command.bus_master_enable()
                    || previous.interrupt_disable() != command.interrupt_disable();
                if command_changed {
                    return Ok(PciConfigWriteOutcome::CommandChanged {
                        token: state
                            .bindings
                            .get(&bdf)
                            .and_then(EndpointRouteToken::snapshot_if_admitted),
                        command,
                    });
                }
                return Ok(PciConfigWriteOutcome::Complete);
            }
        };
        Ok(PciConfigWriteOutcome::Effect {
            token,
            command,
            effect: Box::new(PciConfigWriteEffect::new(
                PciConfigReadEffect::new(
                    capability,
                    effect.effect(),
                    relative,
                    width,
                    snapshot,
                    command,
                ),
                value,
            )),
        })
    }

    /// Resolves one complete memory access against the current enabled BARs.
    ///
    /// Returns `None` for overflow, disabled decode, or an unmapped address.
    pub fn resolve_bar(&self, address: u64, width: AccessWidth) -> Option<PciBarRoute> {
        let access_end = address.checked_add(width.size() as u64)?;
        let state = self.state.lock_irqsave();
        resolve_route(&state.functions, address, access_end, width).map(|(_, route)| route)
    }

    pub(crate) fn resolve_bound_bar(
        &self,
        address: u64,
        width: AccessWidth,
    ) -> Option<(EndpointRouteToken, PciBarRoute, PciCommandState)> {
        let access_end = address.checked_add(width.size() as u64)?;
        let state = self.state.lock_irqsave();
        let (bdf, route) = resolve_route(&state.functions, address, access_end, width)?;
        let function = state
            .functions
            .iter()
            .find(|function| function.bdf() == bdf)?;
        Some((
            state.bindings.get(&bdf)?.snapshot_if_admitted()?,
            route,
            function.command_state(),
        ))
    }

    pub(crate) fn reserve_endpoint_binding(
        &self,
        function_id: &crate::DeviceNodeId,
    ) -> PciResult<EndpointBindingReservation<'_>> {
        let function_name = function_id.to_string();
        let function =
            self.topology
                .function(function_id)
                .ok_or_else(|| PciError::UnknownFunction {
                    function: function_name.clone(),
                })?;
        let bdf = function.bdf();
        let mut state = self.state.lock_irqsave();
        let function_index =
            state
                .function_index(bdf)
                .ok_or_else(|| PciError::UnknownFunction {
                    function: function_name.clone(),
                })?;
        if state.bindings.contains_key(&bdf) {
            return Err(PciError::FunctionAlreadyBound {
                function: function_name,
            });
        }
        if state.pending_bindings.contains(&bdf) {
            return Err(PciError::BindingInProgress {
                function: function_name,
            });
        }
        // This marker linearizes the command snapshot with final binding
        // publication while leaving endpoint initialization outside the root
        // state lock.
        let command = state.functions[function_index].command_state();
        state.pending_bindings.insert(bdf);
        Ok(EndpointBindingReservation {
            root: self,
            function: function_name,
            bdf,
            command,
            committed: false,
        })
    }

    pub(crate) fn replace_endpoint_tokens(
        &self,
        replacements: &[(EndpointRouteToken, EndpointRouteToken)],
    ) {
        let mut state = self.state.lock_irqsave();
        for token in state.bindings.values_mut() {
            if let Some((_, replacement)) = replacements.iter().find(|(old, _)| old == token) {
                *token = replacement.clone();
            }
        }
    }

    /// Revokes the root route for one stable endpoint binding identity.
    ///
    /// The admission epoch is intentionally not part of this match: a full
    /// lifecycle reset replaces the epoch while preserving the binding
    /// generation.  Matching the generation as well as the device prevents a
    /// stale lease from removing a later binding for the same device.
    pub(crate) fn unbind_route_for_binding(&self, route: &EndpointRouteToken) {
        let device = route.device_id();
        let binding_generation = route.binding_generation();
        self.state.lock_irqsave().bindings.retain(|_, token| {
            token.device_id() != device || token.binding_generation() != binding_generation
        });
    }

    /// Revokes every root route before router teardown starts.
    pub(crate) fn unbind_all_routes(&self) {
        self.state.lock_irqsave().bindings.clear();
    }

    /// Restores every function's root-owned power-on config and BAR route.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::CommandRevisionExhausted`] if a command snapshot
    /// revision cannot advance without wrapping.
    pub fn reset(&self) -> PciResult {
        let mut state = self.state.lock_irqsave();
        for function in &state.functions {
            function.command_state().revision().next()?;
        }
        state.pending_bindings.clear();
        for function in &mut state.functions {
            function.reset()?
        }
        Ok(())
    }

    /// Resets root-owned state and snapshots the fresh command state for all
    /// currently bound endpoint device identities.
    pub(crate) fn reset_and_snapshot_commands(
        &self,
    ) -> PciResult<Vec<(DeviceId, PciCommandState)>> {
        let mut state = self.state.lock_irqsave();
        for function in &state.functions {
            function.command_state().revision().next()?;
        }
        state.pending_bindings.clear();
        for function in &mut state.functions {
            function.reset()?
        }
        Ok(state
            .bindings
            .iter()
            .filter_map(|(bdf, token)| {
                state
                    .functions
                    .iter()
                    .find(|function| function.bdf() == *bdf)
                    .map(|function| (token.device_id(), function.command_state()))
            })
            .collect())
    }
}

fn resolve_route(
    functions: &[FunctionState],
    address: u64,
    access_end: u64,
    width: AccessWidth,
) -> Option<(PciBdf, PciBarRoute)> {
    for function in functions {
        if !function.memory_decode_enabled() {
            continue;
        }
        for bar in function.bars() {
            let Some(range) = bar.range() else { continue };
            if range.start <= address && access_end <= range.end {
                let bdf = function.bdf();
                return Some((
                    bdf,
                    PciBarRoute {
                        bdf,
                        bar: bar.index(),
                        offset: address - range.start,
                        width,
                    },
                ));
            }
        }
    }
    None
}

fn interrupt_status_mask(offset: usize, size: usize) -> Option<u64> {
    let status_offset = CONFIG_STATUS_OFFSET;
    if !(offset..offset + size).contains(&status_offset) {
        return None;
    }
    Some(u64::from(STATUS_INTERRUPT_PENDING) << ((status_offset - offset) * 8))
}

impl fmt::Debug for PciRootState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PciRootState")
            .field("topology", &self.topology)
            .finish_non_exhaustive()
    }
}

struct RootState {
    functions: alloc::vec::Vec<FunctionState>,
    bindings: BTreeMap<PciBdf, EndpointRouteToken>,
    pending_bindings: BTreeSet<PciBdf>,
}

impl RootState {
    fn function_index(&self, bdf: PciBdf) -> Option<usize> {
        self.functions
            .binary_search_by_key(&bdf, FunctionState::bdf)
            .ok()
    }

    fn bar_address_available(
        &self,
        memory_aperture: &Range<u64>,
        owner: usize,
        owner_bar: usize,
        address: u64,
    ) -> bool {
        let bar = &self.functions[owner].bars()[owner_bar];
        let Some(end) = address.checked_add(bar.size()) else {
            return false;
        };
        if address & (bar.size() - 1) != 0
            || address < memory_aperture.start
            || end > memory_aperture.end
            || end > FOUR_GIB
        {
            return false;
        }
        !self
            .functions
            .iter()
            .enumerate()
            .any(|(function_index, function)| {
                function
                    .bars()
                    .iter()
                    .enumerate()
                    .any(|(bar_index, existing)| {
                        if function_index == owner && bar_index == owner_bar {
                            return false;
                        }
                        existing
                            .range()
                            .is_some_and(|range| address < range.end && range.start < end)
                    })
            })
    }
}

/// One current BAR route resolved without entering an endpoint callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciBarRoute {
    bdf: PciBdf,
    bar: PciBarIndex,
    offset: u64,
    width: AccessWidth,
}

impl PciBarRoute {
    /// Returns the selected function.
    pub const fn bdf(self) -> PciBdf {
        self.bdf
    }

    /// Returns the selected BAR.
    pub const fn bar(self) -> PciBarIndex {
        self.bar
    }

    /// Returns the function-relative BAR offset.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the complete access width.
    pub const fn width(self) -> AccessWidth {
        self.width
    }
}

pub(crate) fn all_ones(size: usize) -> u64 {
    u64::MAX >> ((8 - size) * 8)
}

#[cfg(test)]
mod tests;
