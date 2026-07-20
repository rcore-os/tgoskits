//! Ownership-taking MSI-X activation transaction.

extern crate alloc;

use alloc::format;

use log::warn;
use pcie::{Endpoint, MsixTableRegion};
use rdif_msi::{Msi, MsiRequest};
use rdrive::probe::OnProbeError;

use super::{
    lease::{PciIrqLease, PciMsixActivationFailure, PciMsixPreflight},
    quarantine::{PciMsiQuarantineReason, PciMsiQuarantineReservation},
    routing::{msi_provider_lookup_error, msix_probe_error},
    transaction::{
        MsixSetupRollbackStep, binding_info_from_msi_vectors_with_host_resources,
        rollback_msix_setup_steps,
    },
};

#[cfg(feature = "nvme")]
impl PciMsixPreflight {
    /// Activates MSI-X after taking exclusive ownership of the PCI endpoint.
    ///
    /// Every failure explicitly reports whether the endpoint was restored to
    /// a reusable state or retained with uncertain hardware resources.
    pub(crate) fn activate(
        self,
        mut endpoint: Endpoint,
    ) -> Result<PciIrqLease, PciMsixActivationFailure> {
        let provider = match rdrive::get::<Msi>(self.target.provider) {
            Ok(provider) => provider,
            Err(error) => {
                return Err(PciMsixActivationFailure::Returned {
                    endpoint,
                    error: msi_provider_lookup_error(
                        self.info.address,
                        self.target.provider,
                        error,
                    ),
                });
            }
        };
        let mut provider = match provider.lock() {
            Ok(provider) => provider,
            Err(_) => {
                return Err(PciMsixActivationFailure::Returned {
                    endpoint,
                    error: OnProbeError::other("failed to lock MSI provider"),
                });
            }
        };
        let quarantine = match PciMsiQuarantineReservation::reserve(self.info.address) {
            Ok(quarantine) => quarantine,
            Err(error) => {
                return Err(PciMsixActivationFailure::Returned {
                    endpoint,
                    error: OnProbeError::other(format!(
                        "cannot allocate MSI-X vectors for {}: {error}",
                        self.info.address
                    )),
                });
            }
        };
        let mut allocation =
            match provider.allocate(MsiRequest::new(self.target.device, self.vector_count)) {
                Ok(allocation) => Some(allocation),
                Err(error) => {
                    quarantine.release();
                    return Err(PciMsixActivationFailure::Returned {
                        endpoint,
                        error: OnProbeError::other(format!(
                            "failed to allocate {} MSI-X vectors for {}: {error:?}",
                            self.vector_count, self.info.address
                        )),
                    });
                }
            };
        let binding = binding_info_from_msi_vectors_with_host_resources(
            allocation
                .as_ref()
                .expect("MSI-X allocation was just installed")
                .vectors(),
            &self.host_resources,
        );

        let mut table_mmio =
            match axklib::mmio::ioremap(self.table_range.start.into(), self.table_range.len()) {
                Ok(mapping) => Some(mapping),
                Err(map_error) => {
                    let allocation = allocation
                        .take()
                        .expect("MSI-X allocation remains owned before table mapping");
                    let activation_error =
                        OnProbeError::other(format!("failed to map MSI-X table: {map_error}"));
                    match provider.free(allocation) {
                        Ok(()) => {
                            quarantine.release();
                            return Err(PciMsixActivationFailure::Returned {
                                endpoint,
                                error: activation_error,
                            });
                        }
                        Err(failure) => {
                            let (allocation, free_error) = failure.into_parts();
                            quarantine.retain(
                                allocation,
                                None,
                                endpoint,
                                PciMsiQuarantineReason::ProviderRelease,
                            );
                            warn!(
                                "failed to free MSI-X allocation for {} after table-map failure; \
                                 retaining the allocation and PCI endpoint: {free_error:?}",
                                self.info.address
                            );
                            return Err(PciMsixActivationFailure::Claimed {
                                error: OnProbeError::other(format!(
                                    "MSI-X table mapping failed for {} and vector release could \
                                     not be proven: {map_error}",
                                    self.info.address
                                )),
                            });
                        }
                    }
                }
            };
        // SAFETY: `table_mmio` owns the complete validated MSI-X table range
        // for at least as long as `table`; preflight verified entry count and
        // range bounds against the endpoint BAR.
        let table = unsafe {
            MsixTableRegion::new(
                table_mmio
                    .as_ref()
                    .expect("MSI-X table mapping was just installed")
                    .as_nonnull_ptr(),
                self.table_info.entries,
            )
        };

        let setup = (|| -> Result<(), OnProbeError> {
            endpoint
                .set_msix_function_mask(true)
                .map_err(msix_probe_error)?;
            for vector in allocation
                .as_ref()
                .expect("MSI-X allocation remains owned during setup")
                .vectors()
            {
                let message = provider.compose_message(vector).map_err(|error| {
                    OnProbeError::other(format!(
                        "failed to compose MSI-X message for {} vector {:?}: {error:?}",
                        self.info.address, vector.index
                    ))
                })?;
                table
                    .program_masked(vector.index.0, message)
                    .map_err(msix_probe_error)?;
                provider
                    .set_vector_enabled(vector, false)
                    .map_err(|error| {
                        OnProbeError::other(format!("failed to disable MSI vector: {error:?}"))
                    })?;
            }
            endpoint.set_msix_enabled(true).map_err(msix_probe_error)?;
            endpoint
                .set_msix_function_mask(false)
                .map_err(msix_probe_error)?;
            Ok(())
        })();

        if let Err(setup_error) = setup {
            let steps_complete = {
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
                            self.info.address
                        );
                    }
                    result
                })
            };
            let rollback_complete = steps_complete
                && endpoint
                    .set_msix_function_mask(false)
                    .map_err(|error| {
                        warn!(
                            "MSI-X setup rollback for {} failed to restore the function mask: \
                             {error}",
                            self.info.address
                        );
                    })
                    .is_ok();

            let allocation = allocation
                .take()
                .expect("MSI-X setup failure retains the provider allocation");
            if rollback_complete {
                match provider.free(allocation) {
                    Ok(()) => {
                        quarantine.release();
                        return Err(PciMsixActivationFailure::Returned {
                            endpoint,
                            error: setup_error,
                        });
                    }
                    Err(failure) => {
                        let (allocation, free_error) = failure.into_parts();
                        quarantine.retain(
                            allocation,
                            table_mmio.take(),
                            endpoint,
                            PciMsiQuarantineReason::ProviderRelease,
                        );
                        warn!(
                            "failed to free MSI-X allocation for {} after setup rollback; \
                             retaining the allocation, table mapping, and endpoint: {free_error:?}",
                            self.info.address
                        );
                        return Err(PciMsixActivationFailure::Claimed {
                            error: OnProbeError::other(format!(
                                "MSI-X setup for {} failed and vector release could not be \
                                 proven: {setup_error}",
                                self.info.address
                            )),
                        });
                    }
                }
            }

            quarantine.retain(
                allocation,
                table_mmio.take(),
                endpoint,
                PciMsiQuarantineReason::SetupContainment,
            );
            warn!(
                "MSI-X setup rollback for {} was incomplete; retaining the vector allocation, \
                 table mapping, and endpoint",
                self.info.address
            );
            return Err(PciMsixActivationFailure::Claimed {
                error: OnProbeError::other(format!(
                    "MSI-X setup for {} failed and hardware rollback was incomplete: {setup_error}",
                    self.info.address
                )),
            });
        }

        Ok(PciIrqLease {
            provider: self.target.provider,
            allocation,
            binding,
            table,
            table_mmio,
            endpoint: Some(endpoint),
            quarantine: Some(quarantine),
        })
    }
}
