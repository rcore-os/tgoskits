use alloc::{string::ToString, sync::Arc, vec::Vec};

use axdevice_base::{DeviceContext, DeviceError, DeviceId, DeviceResult, RoutedDeviceGrant};

use super::{
    super::{PciBdf, PciError, ResolvedPciTopology},
    DeviceManagerResult, EndpointRouteToken, PciBarAccess, PciRootBinding,
    cleanup::PciBindingLease,
    endpoint::{
        EndpointIrqTransitionPermit, LegacyPciEndpointContext, OwnerPciEndpointContext,
        PciEndpointContext, PciFunction, RoutedPciEndpointContext,
    },
    lifecycle::{
        BindingLifecycleState, PendingIrqWithdrawal, retry_pending_irq_withdrawals,
        transfer_pending_irq_withdrawals,
    },
    pci_config_error,
    routing::ENDPOINT_SUSPEND_ROLLBACK_FAILED,
};
use crate::{
    AccessWidth, DeviceManagerError, DeviceNodeId,
    pci::root::{PciConfigReadOutcome, PciConfigWriteOutcome},
};

impl PciRootBinding {
    /// Runs one runtime-owned endpoint poll while its current IRQ admission is held.
    pub(crate) fn with_endpoint_irq_permit(
        &self,
        device: DeviceId,
        callback: &mut dyn FnMut() -> DeviceManagerResult,
    ) -> DeviceManagerResult {
        let _permit = self
            .router
            .acquire_irq_permit(device)
            .map_err(DeviceManagerError::Device)?;
        // Admit each poll against the root-owned current Command register,
        // just as BAR notification dispatch snapshots BME. A disabled
        // function keeps its deferred work until a later admitted poll.
        if !self.root.endpoint_bus_master_enabled(device) {
            return Ok(());
        }
        callback()
    }

    pub(super) fn queue_irq_withdrawal(&self, withdrawal: PendingIrqWithdrawal) {
        self.pending_irq_withdrawals.lock().push(withdrawal);
    }

    fn has_pending_irq_withdrawal(&self, device: DeviceId) -> bool {
        self.pending_irq_withdrawals
            .lock()
            .iter()
            .any(|withdrawal| withdrawal.device == device)
    }

    fn rollback_unpublished_endpoint(
        &self,
        token: &EndpointRouteToken,
        function: Arc<dyn PciFunction>,
    ) {
        let admission = token.admission.clone();
        drop(self.router.invalidate(token));
        let mut permit = EndpointIrqTransitionPermit { _private: () };
        if let Err(error) = function.withdraw_irq(&mut permit) {
            self.queue_irq_withdrawal(PendingIrqWithdrawal {
                device: token.device_id(),
                function,
                admission,
            });
            warn!(
                "PCI endpoint {} rollback could not withdraw its IRQ source: {}",
                token.device_id().as_u32(),
                error
            );
        }
    }

    pub(crate) fn matches_topology(&self, topology: &Arc<ResolvedPciTopology>) -> bool {
        Arc::ptr_eq(self.root.topology_arc(), topology)
    }

    pub(crate) fn reset_lifecycle(&self) -> DeviceManagerResult {
        let operation = self.begin_reset_operation()?;
        let result = self.reset_routes();
        if result.is_err() {
            if let Err(error) = operation.finish_reset_failure() {
                warn!("PCI reset failure handoff could not complete: {error}");
            }
            return result;
        }
        operation.finish_reset()
    }

    /// Quiesces the PCI root and every bound endpoint for a VM suspend.
    ///
    /// Routes are first closed and in-flight endpoint claims drained, then each
    /// endpoint child is asked to stop its worker/DMA activity. When the
    /// endpoint children restore themselves the root reopens its admissions and
    /// returns to its entry state; when a compensating resume also fails the
    /// root stays fail-closed with the offending endpoint still quiesced.
    pub(crate) fn suspend_lifecycle(&self) -> DeviceManagerResult {
        if let Err(error) = self.router.close_admissions_and_drain() {
            self.reopen_running_admissions();
            return Err(error);
        }
        match self.router.suspend_endpoints() {
            Ok(()) => Ok(()),
            Err(error) => {
                // Reopen admissions only when the endpoints were fully
                // restored. A failed compensation leaves an endpoint quiesced,
                // so the root must stay fail-closed instead of pretending to
                // run with a still-suspended child.
                let endpoints_restored = !matches!(
                    &error,
                    DeviceManagerError::InvalidState { operation, .. }
                        if *operation == ENDPOINT_SUSPEND_ROLLBACK_FAILED
                );
                if endpoints_restored {
                    self.reopen_running_admissions();
                }
                Err(error)
            }
        }
    }

    fn reopen_running_admissions(&self) {
        if self.is_running() {
            self.router.open_admissions();
        }
    }

    /// Re-opens the PCI root and every bound endpoint after a suspend.
    ///
    /// A terminal (stopping or dead) root is not re-opened; only a running
    /// root republishes endpoint admissions.
    pub(crate) fn resume_lifecycle(&self) -> DeviceManagerResult {
        if !self.is_running() {
            return Ok(());
        }
        self.router.resume_endpoints()?;
        self.router.open_admissions();
        Ok(())
    }

    /// Stops the PCI root: closes routes/admissions, waits for in-flight
    /// claims, then releases endpoint owners and their IRQ sources.
    ///
    /// This is the explicit, error-reporting teardown. Every step before
    /// [`LifecycleOperation::finish_stop`] must succeed before the binding is
    /// finished, so a failed stop leaves the endpoint hardware and its owners
    /// reachable for a later retry instead of a false success.
    pub(crate) fn stop_lifecycle(&self) -> DeviceManagerResult {
        // A dead binding that no longer owns endpoint routes already released
        // its hardware, so repeating the stop is an idempotent success. A dead
        // binding that still owns routes failed earlier and must report that
        // failure instead of masking it.
        if self.lifecycle.lock().state == BindingLifecycleState::Dead {
            return if self.router.endpoint_functions().is_empty() {
                Ok(())
            } else {
                Err(DeviceManagerError::InvalidState {
                    operation: "stop PCI root binding",
                    detail: "PCI root teardown previously failed with endpoint owners still held"
                        .into(),
                })
            };
        }
        // Route withdrawal is the first linearization point: no route may stay
        // reachable while the stop owner closes admissions below.
        self.root.unbind_all_routes();
        let mut lifecycle = self.begin_stop_operation();
        lifecycle.wait_for_claim()?;
        // Phase one: close route admissions and wait for in-flight claims. The
        // endpoint owners and their hardware are still held here.
        self.router.close_admissions_and_drain()?;
        self.drain_pending_binding_withdrawals()?;
        // Phase two: join endpoint workers, then release their owners and
        // withdraw their IRQ sources.
        self.router.stop_endpoints()?;
        let (pending, drain_result) = self.router.invalidate_all();
        for withdrawal in pending {
            self.queue_irq_withdrawal(withdrawal);
        }
        drain_result?;
        retry_pending_irq_withdrawals(&self.pending_irq_withdrawals)?;
        self.drain_pending_binding_withdrawals()?;
        transfer_pending_irq_withdrawals(&self.pending_irq_withdrawals);
        lifecycle.finish_stop()
    }

    fn is_running(&self) -> bool {
        self.lifecycle.lock().state == BindingLifecycleState::Running
    }

    fn reset_routes(&self) -> DeviceManagerResult {
        let replacements = self.router.reset_admissions()?;
        let commands = self
            .root
            .reset_and_snapshot_commands()
            .map_err(DeviceManagerError::Pci)?;
        self.router.reset_endpoints(&commands)?;
        self.root.replace_endpoint_tokens(&replacements);
        Ok(())
    }

    pub(crate) fn bind_registered(
        self: &Arc<Self>,
        function_id: &DeviceNodeId,
        device: DeviceId,
        function: Arc<dyn PciFunction>,
        routed_grants: &mut Vec<RoutedDeviceGrant>,
    ) -> DeviceManagerResult<PciBindingLease> {
        self.bind_registered_inner(function_id, device, function, Some(routed_grants))
    }

    fn bind_registered_inner(
        self: &Arc<Self>,
        function_id: &DeviceNodeId,
        device: DeviceId,
        function: Arc<dyn PciFunction>,
        mut routed_grants: Option<&mut Vec<RoutedDeviceGrant>>,
    ) -> DeviceManagerResult<PciBindingLease> {
        self.validate_config_effect_contract(function_id, function.as_ref())?;
        if !function.resources().is_empty() {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "bind PCI endpoint route",
                detail: alloc::format!(
                    "endpoint {} must not publish ordinary device resources",
                    function_id
                ),
            });
        }
        // This logical operation serializes binding with reset and teardown
        // without holding a lifecycle lock across endpoint behavior.
        let lifecycle = self.begin_binding_operation()?;
        if self.has_pending_irq_withdrawal(device) {
            return Err(DeviceManagerError::InvalidState {
                operation: "bind PCI endpoint route",
                detail: "the previous PCI endpoint IRQ withdrawal is still pending".into(),
            });
        }
        let reservation = self
            .root
            .reserve_endpoint_binding(function_id)
            .map_err(DeviceManagerError::Pci)?;
        let command = reservation.command();
        let token = self.router.activate(device, function.clone())?;
        let mut owner_context = OwnerPciEndpointContext { device_id: device };
        if let Err(error) = function.command_changed(command, &mut owner_context) {
            self.rollback_unpublished_endpoint(&token, function.clone());
            return Err(DeviceManagerError::Device(error));
        }
        let registered = routed_grants.is_some();
        if let Some(grants) = routed_grants.as_deref_mut() {
            grants.push(token.grant(false));
        }
        if let Err(error) = reservation.commit(token.clone()) {
            if registered && let Some(grants) = routed_grants {
                grants.pop();
            }
            self.rollback_unpublished_endpoint(&token, function);
            return Err(error.into());
        }
        if let Err(error) = lifecycle.finish_restore() {
            // The route publication itself succeeded. Deferred withdrawals
            // remove their routes before reporting an IRQ cleanup failure, so
            // retain the new binding while the closed IRQ owner is retried.
            warn!("PCI binding completion could not finish deferred cleanup: {error}");
        }
        Ok(PciBindingLease {
            binding: self.clone(),
            token,
        })
    }

    fn validate_config_effect_contract(
        &self,
        function_id: &DeviceNodeId,
        function: &dyn PciFunction,
    ) -> DeviceManagerResult {
        let resolved = self.root.topology().function(function_id).ok_or_else(|| {
            DeviceManagerError::Pci(PciError::UnknownFunction {
                function: function_id.to_string(),
            })
        })?;
        let supported = function.supported_config_effects();

        for (index, effect) in supported.iter().enumerate() {
            if supported[..index].contains(effect) {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "bind PCI endpoint route",
                    detail: alloc::format!(
                        "endpoint {} advertises duplicate PCI config effect {}",
                        function_id,
                        effect.value()
                    ),
                });
            }
            if !resolved.capabilities().any(|capability| {
                capability
                    .effects()
                    .iter()
                    .any(|declared| declared.effect() == *effect)
            }) {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "bind PCI endpoint route",
                    detail: alloc::format!(
                        "endpoint {} advertises undeclared PCI config effect {}",
                        function_id,
                        effect.value()
                    ),
                });
            }
        }

        for capability in resolved.capabilities() {
            for declared in capability.effects() {
                if !supported.contains(&declared.effect()) {
                    return Err(DeviceManagerError::InvalidConfig {
                        operation: "bind PCI endpoint route",
                        detail: alloc::format!(
                            "endpoint {} does not support declared PCI config effect {}",
                            function_id,
                            declared.effect().value()
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn dispatch_legacy<T>(
        &self,
        token: &EndpointRouteToken,
        dma_enabled: bool,
        mut callback: impl FnMut(&Arc<dyn PciFunction>, &mut dyn PciEndpointContext) -> DeviceResult<T>,
    ) -> DeviceResult<T> {
        let lease = self.router.lease(token, dma_enabled)?;
        let mut context = LegacyPciEndpointContext {
            device_id: token.device_id(),
            admission: token.admission.clone(),
        };
        callback(&lease.endpoint, &mut context)
    }

    fn dispatch_with_context<T>(
        &self,
        token: &EndpointRouteToken,
        dma_enabled: bool,
        context: &mut dyn DeviceContext,
        mut callback: impl FnMut(&Arc<dyn PciFunction>, &mut dyn PciEndpointContext) -> DeviceResult<T>,
    ) -> DeviceResult<T> {
        let lease = self.router.lease(token, dma_enabled)?;
        let grant = lease.grant.clone();
        let admission = lease._admission.clone();
        let endpoint = lease.endpoint.clone();
        let mut result = None;
        let mut invoke = |nested: &mut dyn DeviceContext| {
            let mut endpoint_context = RoutedPciEndpointContext {
                inner: nested,
                admission: admission.clone(),
            };
            result = Some(callback(&endpoint, &mut endpoint_context));
            Ok(())
        };
        context.with_routed_device(&grant, &mut invoke)?;
        result.ok_or(DeviceError::InvalidState {
            operation: "dispatch PCI endpoint route",
            detail: "routed context callback did not execute".into(),
        })?
    }

    /// Dispatches a BAR read after root lookup and token validation.
    pub fn read_bar(&self, address: u64, width: AccessWidth) -> DeviceResult<u64> {
        let (token, route, command) = self
            .root
            .resolve_bound_bar(address, width)
            .ok_or(DeviceError::NotFound)?;
        self.dispatch_legacy(&token, command.bus_master_enable(), |endpoint, context| {
            endpoint.read_bar(PciBarAccess::new(route, command), context)
        })
    }

    /// Dispatches a BAR read through an authenticated runtime context.
    pub fn read_bar_with_context(
        &self,
        address: u64,
        width: AccessWidth,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult<u64> {
        let (token, route, command) = self
            .root
            .resolve_bound_bar(address, width)
            .ok_or(DeviceError::NotFound)?;
        self.dispatch_with_context(
            &token,
            command.bus_master_enable(),
            context,
            |endpoint, context| endpoint.read_bar(PciBarAccess::new(route, command), context),
        )
    }

    /// Dispatches a BAR write after root lookup and token validation.
    pub fn write_bar(&self, address: u64, width: AccessWidth, value: u64) -> DeviceResult {
        let (token, route, command) = self
            .root
            .resolve_bound_bar(address, width)
            .ok_or(DeviceError::NotFound)?;
        self.dispatch_legacy(&token, command.bus_master_enable(), |endpoint, context| {
            endpoint.write_bar(PciBarAccess::new(route, command), value, context)
        })
    }

    /// Dispatches a BAR write through an authenticated runtime context.
    pub fn write_bar_with_context(
        &self,
        address: u64,
        width: AccessWidth,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (token, route, command) = self
            .root
            .resolve_bound_bar(address, width)
            .ok_or(DeviceError::NotFound)?;
        self.dispatch_with_context(
            &token,
            command.bus_master_enable(),
            context,
            |endpoint, context| {
                endpoint.write_bar(PciBarAccess::new(route, command), value, context)
            },
        )
    }

    /// Dispatches one complete config-space read.
    pub fn read_config(
        &self,
        bdf: PciBdf,
        offset: crate::ConfigOffset,
        width: AccessWidth,
    ) -> DeviceResult<u64> {
        match self
            .root
            .prepare_read_config(bdf, offset, width)
            .map_err(pci_config_error)?
        {
            PciConfigReadOutcome::Value(value) => Ok(value),
            PciConfigReadOutcome::DynamicStatus {
                token,
                command,
                value,
                interrupt_status_mask,
            } => {
                let pending = self.dispatch_legacy(
                    &token,
                    command.bus_master_enable(),
                    |endpoint, _context| Ok(endpoint.intx_pending()),
                )?;
                Ok(if pending {
                    value | interrupt_status_mask
                } else {
                    value & !interrupt_status_mask
                })
            }
            PciConfigReadOutcome::Effect {
                token,
                command,
                effect,
            } => self.dispatch_legacy(&token, command.bus_master_enable(), |endpoint, context| {
                endpoint.read_config_effect(*effect, context)
            }),
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub(crate) fn config_access_intersects_effect(
        &self,
        bdf: PciBdf,
        offset: crate::ConfigOffset,
        width: AccessWidth,
    ) -> DeviceResult<bool> {
        self.root
            .config_access_intersects_effect(bdf, offset, width)
            .map_err(pci_config_error)
    }

    /// Dispatches one complete config-space write.
    pub fn write_config(
        &self,
        bdf: PciBdf,
        offset: crate::ConfigOffset,
        width: AccessWidth,
        value: u64,
    ) -> DeviceResult {
        match self
            .root
            .prepare_write_config(bdf, offset, width, value)
            .map_err(pci_config_error)?
        {
            PciConfigWriteOutcome::Complete => Ok(()),
            PciConfigWriteOutcome::Effect {
                token,
                command,
                effect,
            } => self.dispatch_legacy(&token, command.bus_master_enable(), |endpoint, context| {
                endpoint.write_config_effect(*effect, context)
            }),
            PciConfigWriteOutcome::CommandChanged { token, command } => {
                let Some(token) = token else {
                    return Ok(());
                };
                self.dispatch_legacy(&token, command.bus_master_enable(), |endpoint, context| {
                    endpoint.command_changed(command, context)
                })
            }
        }
    }

    /// Dispatches a config read effect through an authenticated runtime
    /// context.
    pub fn read_config_with_context(
        &self,
        bdf: PciBdf,
        offset: crate::ConfigOffset,
        width: AccessWidth,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult<u64> {
        match self
            .root
            .prepare_read_config(bdf, offset, width)
            .map_err(pci_config_error)?
        {
            PciConfigReadOutcome::Value(value) => Ok(value),
            PciConfigReadOutcome::DynamicStatus {
                token,
                command,
                value,
                interrupt_status_mask,
            } => {
                let pending = self.dispatch_with_context(
                    &token,
                    command.bus_master_enable(),
                    context,
                    |endpoint, _context| Ok(endpoint.intx_pending()),
                )?;
                Ok(if pending {
                    value | interrupt_status_mask
                } else {
                    value & !interrupt_status_mask
                })
            }
            PciConfigReadOutcome::Effect {
                token,
                command,
                effect,
            } => self.dispatch_with_context(
                &token,
                command.bus_master_enable(),
                context,
                |endpoint, context| endpoint.read_config_effect(*effect, context),
            ),
        }
    }

    /// Dispatches a config write effect through an authenticated runtime
    /// context.
    pub fn write_config_with_context(
        &self,
        bdf: PciBdf,
        offset: crate::ConfigOffset,
        width: AccessWidth,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        match self
            .root
            .prepare_write_config(bdf, offset, width, value)
            .map_err(pci_config_error)?
        {
            PciConfigWriteOutcome::Complete => Ok(()),
            PciConfigWriteOutcome::Effect {
                token,
                command,
                effect,
            } => self.dispatch_with_context(
                &token,
                command.bus_master_enable(),
                context,
                |endpoint, context| endpoint.write_config_effect(*effect, context),
            ),
            PciConfigWriteOutcome::CommandChanged { token, command } => {
                let Some(token) = token else {
                    return Ok(());
                };
                self.dispatch_with_context(
                    &token,
                    command.bus_master_enable(),
                    context,
                    |endpoint, context| endpoint.command_changed(command, context),
                )
            }
        }
    }
}
