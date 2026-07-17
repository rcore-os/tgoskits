extern crate alloc;

use alloc::{format, vec::Vec};

use log::warn;
use pcie::{Endpoint, MsixTableRegion};
use rdif_msi::{Msi, MsiAllocation, MsiRequest};
use rdrive::{
    DeviceId,
    probe::{OnProbeError, pci::PciInfo},
};

use super::{
    routing::{msi_provider_lookup_error, msi_target_for_endpoint, msix_probe_error},
    transaction::{
        MsixSetupRollbackStep, binding_error, binding_info_from_msi_vectors_with_host_resources,
        disable_provider_vectors, enable_vector_bindings, mask_vector_table_entries,
        provider_access_error, provider_access_fault, provider_vector_fault,
        retain_failed_lease_resources, retain_failed_setup_resources, rollback_msix_setup_steps,
        set_table_masked, table_vector_fault,
    },
};
use crate::{
    BindingInfo, BindingIrqBinding, IrqBindingError, IrqBindingFailure, IrqBindingOperation,
    IrqBindingStage, binding_resolver::binding_info_from_pci_endpoint_resources,
};

pub struct PciIrqLease {
    provider: DeviceId,
    allocation: Option<MsiAllocation>,
    binding: BindingInfo,
    table: MsixTableRegion,
    table_mmio: Option<mmio_api::Mmio>,
    endpoint: Option<Endpoint>,
}

pub type PciMsixAllocation = PciIrqLease;

impl PciIrqLease {
    pub fn allocate(
        endpoint: &mut Endpoint,
        info: PciInfo,
        vector_count: u16,
    ) -> Result<Self, OnProbeError> {
        let host_resources = binding_info_from_pci_endpoint_resources(info, endpoint)?;
        let target = msi_target_for_endpoint(info)?;
        let table_info = endpoint.msix_table_info().map_err(msix_probe_error)?;
        let table_range = endpoint.msix_table_range().map_err(msix_probe_error)?;

        if vector_count == 0 || vector_count > table_info.entries {
            return Err(OnProbeError::other(format!(
                "PCI endpoint {} requested {vector_count} MSI-X vectors, table has {}",
                info.address, table_info.entries
            )));
        }

        let provider = rdrive::get::<Msi>(target.provider)
            .map_err(|err| msi_provider_lookup_error(info.address, target.provider, err))?;
        let mut provider = provider
            .lock()
            .map_err(|_| OnProbeError::other("failed to lock MSI provider"))?;
        let mut allocation = Some(
            provider
                .allocate(MsiRequest::new(target.device, vector_count))
                .map_err(|err| {
                    OnProbeError::other(format!(
                        "failed to allocate {vector_count} MSI-X vectors for {}: {err:?}",
                        info.address
                    ))
                })?,
        );
        let binding = binding_info_from_msi_vectors_with_host_resources(
            allocation
                .as_ref()
                .ok_or_else(|| OnProbeError::other("MSI-X allocation was not retained"))?
                .vectors(),
            &host_resources,
        );

        let mut table_mmio =
            match axklib::mmio::ioremap(table_range.start.into(), table_range.len()) {
                Ok(mapping) => Some(mapping),
                Err(error) => {
                    if let Some(allocation) = allocation.take()
                        && let Err(free_error) = provider.free(allocation)
                    {
                        warn!(
                            "failed to free MSI-X allocation for {} after table-map failure: \
                             {free_error:?}",
                            info.address
                        );
                    }
                    return Err(OnProbeError::other(format!(
                        "failed to map MSI-X table: {error}"
                    )));
                }
            };
        let table = unsafe {
            MsixTableRegion::new(
                table_mmio
                    .as_ref()
                    .expect("MSI-X table mapping was just installed")
                    .as_nonnull_ptr(),
                table_info.entries,
            )
        };

        let setup = (|| -> Result<(), OnProbeError> {
            endpoint
                .set_msix_function_mask(true)
                .map_err(msix_probe_error)?;
            {
                let allocation_ref = allocation
                    .as_ref()
                    .ok_or_else(|| OnProbeError::other("MSI-X allocation was already consumed"))?;
                for vector in allocation_ref.vectors() {
                    let message = provider.compose_message(vector).map_err(|err| {
                        OnProbeError::other(format!(
                            "failed to compose MSI-X message for {} vector {:?}: {err:?}",
                            info.address, vector.index
                        ))
                    })?;
                    table
                        .program_masked(vector.index.0, message)
                        .map_err(msix_probe_error)?;
                    provider.set_vector_enabled(vector, false).map_err(|err| {
                        OnProbeError::other(format!("failed to disable MSI vector: {err:?}"))
                    })?;
                }
            }
            endpoint.set_msix_enabled(true).map_err(msix_probe_error)?;
            // Every entry is still masked and every provider vector disabled.
            // Clear the temporary function-wide setup barrier so the lease can
            // later publish vectors solely through its transactional path.
            endpoint
                .set_msix_function_mask(false)
                .map_err(msix_probe_error)?;
            Ok(())
        })();

        if let Err(setup_error) = setup {
            let rollback_complete = {
                let vectors = allocation
                    .as_ref()
                    .expect("MSI-X allocation remains owned during setup rollback")
                    .vectors();
                rollback_msix_setup_steps(vectors, |step| {
                    let result = match step {
                        MsixSetupRollbackStep::FunctionMask => endpoint
                            .set_msix_function_mask(true)
                            .map_err(|_| "function mask"),
                        MsixSetupRollbackStep::TableEntry(vector) => {
                            table.mask(vector.index.0).map_err(|_| "table entry mask")
                        }
                        MsixSetupRollbackStep::ProviderVector(vector) => provider
                            .set_vector_enabled(vector, false)
                            .map_err(|_| "provider vector disable"),
                        MsixSetupRollbackStep::DisableCapability => endpoint
                            .set_msix_enabled(false)
                            .map_err(|_| "capability disable"),
                    };
                    if let Err(stage) = result {
                        warn!(
                            "MSI-X setup rollback for {} failed at {stage}",
                            info.address
                        );
                    }
                    result
                })
            };

            if rollback_complete {
                if let Some(allocation) = allocation.take()
                    && let Err(error) = provider.free(allocation)
                {
                    warn!(
                        "failed to free MSI-X allocation for {} after setup rollback: {error:?}",
                        info.address
                    );
                }
            } else {
                retain_failed_setup_resources(&mut allocation, &mut table_mmio);
                warn!(
                    "MSI-X setup rollback for {} was incomplete; retaining vector allocation and \
                     table mapping",
                    info.address
                );
            }
            return Err(setup_error);
        }

        Ok(Self {
            provider: target.provider,
            allocation,
            binding,
            table,
            table_mmio,
            endpoint: None,
        })
    }

    pub fn binding_info(&self) -> BindingInfo {
        self.binding.clone()
    }

    pub fn irq_bindings(&self) -> Vec<BindingIrqBinding> {
        self.binding_info().irq_sources().to_vec()
    }

    pub fn vector_indices(&self) -> Vec<u16> {
        self.vectors().iter().map(|vector| vector.index.0).collect()
    }

    /// Binds the successfully discovered PCI function to this IRQ lease.
    ///
    /// The endpoint remains exclusively owned until IRQ shutdown succeeds. A
    /// failed shutdown quarantines it with the vector and table resources.
    #[cfg(feature = "nvme")]
    pub(crate) fn retain_endpoint(mut self, endpoint: Endpoint) -> Self {
        debug_assert!(self.endpoint.is_none());
        self.endpoint = Some(endpoint);
        self
    }

    /// Enables every allocated MSI-X vector as one transaction.
    ///
    /// # Errors
    ///
    /// Returns the first provider or table failure. Every vector enabled
    /// before that failure is masked and disabled before this method returns.
    pub fn enable(&self) -> Result<(), IrqBindingError> {
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            binding_error(
                IrqBindingOperation::Enable,
                IrqBindingStage::Allocation,
                None,
                IrqBindingFailure::MissingAllocation,
            )
        })?;
        let provider = rdrive::get::<Msi>(self.provider).map_err(|error| {
            provider_access_error(
                IrqBindingOperation::Enable,
                IrqBindingStage::ProviderLookup,
                error,
            )
        })?;
        let mut provider = provider.lock().map_err(|error| {
            provider_access_error(
                IrqBindingOperation::Enable,
                IrqBindingStage::ProviderLock,
                error,
            )
        })?;

        enable_vector_bindings(
            allocation.vectors(),
            &mut |vector, enabled| {
                provider
                    .set_vector_enabled(vector, enabled)
                    .map_err(|error| provider_vector_fault(vector, error))
            },
            &mut |vector, masked| {
                set_table_masked(&self.table, vector, masked)
                    .map_err(|error| table_vector_fault(vector, error))
            },
        )
    }

    /// Masks every MSI-X table entry and disables every provider vector.
    ///
    /// # Errors
    ///
    /// Returns the first failure after attempting every independent table and
    /// provider operation that remains possible.
    pub fn disable(&self) -> Result<(), IrqBindingError> {
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            binding_error(
                IrqBindingOperation::Disable,
                IrqBindingStage::Allocation,
                None,
                IrqBindingFailure::MissingAllocation,
            )
        })?;
        let vectors = allocation.vectors();
        let mut first_fault = mask_vector_table_entries(vectors, &mut |vector, masked| {
            set_table_masked(&self.table, vector, masked)
                .map_err(|error| table_vector_fault(vector, error))
        });

        let provider = match rdrive::get::<Msi>(self.provider) {
            Ok(provider) => provider,
            Err(error) => {
                let access_fault = provider_access_fault(IrqBindingStage::ProviderLookup, error);
                return Err(IrqBindingError::new(
                    IrqBindingOperation::Disable,
                    first_fault.unwrap_or(access_fault),
                ));
            }
        };
        let mut provider = match provider.lock() {
            Ok(provider) => provider,
            Err(error) => {
                let access_fault = provider_access_fault(IrqBindingStage::ProviderLock, error);
                return Err(IrqBindingError::new(
                    IrqBindingOperation::Disable,
                    first_fault.unwrap_or(access_fault),
                ));
            }
        };

        let provider_fault = disable_provider_vectors(vectors, &mut |vector, enabled| {
            provider
                .set_vector_enabled(vector, enabled)
                .map_err(|error| provider_vector_fault(vector, error))
        });
        if first_fault.is_none() {
            first_fault = provider_fault;
        }
        match first_fault {
            Some(fault) => Err(IrqBindingError::new(IrqBindingOperation::Disable, fault)),
            None => Ok(()),
        }
    }

    fn vectors(&self) -> &[rdif_msi::MsiVector] {
        self.allocation
            .as_ref()
            .map(MsiAllocation::vectors)
            .unwrap_or(&[])
    }
}

impl crate::IrqBindingLease for PciIrqLease {
    fn binding_info(&self) -> BindingInfo {
        PciIrqLease::binding_info(self)
    }

    fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.enable()
    }

    fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.disable()
    }
}

impl Drop for PciIrqLease {
    fn drop(&mut self) {
        let vector_disable_error = self.disable().err();
        let mut capability_disable_failed = false;
        if let Some(endpoint) = self.endpoint.as_mut() {
            if let Err(error) = endpoint.set_msix_function_mask(true) {
                capability_disable_failed = true;
                warn!("failed to assert MSI-X function mask before release: {error}");
            }
            if let Err(error) = endpoint.set_msix_enabled(false) {
                capability_disable_failed = true;
                warn!("failed to disable MSI-X capability before release: {error}");
            }
        }
        if vector_disable_error.is_some() || capability_disable_failed {
            retain_failed_lease_resources(
                &mut self.allocation,
                &mut self.table_mmio,
                &mut self.endpoint,
            );
            if let Some(error) = vector_disable_error {
                warn!(
                    "failed to disable MSI-X vectors before release; endpoint-wide containment \
                     was still attempted and the PCI endpoint, vector token, and table mapping \
                     are retained: {error}"
                );
            } else {
                warn!(
                    "failed to disable the MSI-X endpoint capability before release; retaining \
                     its PCI endpoint, vector token, and table mapping"
                );
            }
            return;
        }

        let Some(allocation) = self.allocation.take() else {
            return;
        };
        if let Ok(provider) = rdrive::get::<Msi>(self.provider)
            && let Ok(mut provider) = provider.lock()
            && let Err(err) = provider.free(allocation)
        {
            warn!("failed to free MSI-X allocation: {err:?}");
        }
    }
}
