extern crate alloc;

#[cfg(feature = "nvme")]
use alloc::format;
use alloc::vec::Vec;
#[cfg(feature = "nvme")]
use core::ops::Range;

use log::warn;
#[cfg(feature = "nvme")]
use pcie::MsixTableInfo;
use pcie::{Endpoint, MsixTableRegion};
use rdif_msi::{Msi, MsiAllocation};
use rdrive::DeviceId;
#[cfg(feature = "nvme")]
use rdrive::probe::{OnProbeError, pci::PciInfo};

#[cfg(feature = "nvme")]
use super::routing::{
    PciMsiTarget, msi_provider_lookup_error, msi_target_for_endpoint, msix_probe_error,
};
use super::{
    quarantine::{PciMsiQuarantineReason, PciMsiQuarantineReservation},
    transaction::{
        binding_error, disable_provider_vectors, enable_vector_bindings, mask_vector_table_entries,
        provider_access_error, provider_access_fault, provider_vector_fault, set_table_masked,
        table_vector_fault,
    },
};
#[cfg(feature = "nvme")]
use crate::binding_resolver::binding_info_from_pci_endpoint_resources;
use crate::{
    BindingInfo, BindingIrqBinding, IrqBindingError, IrqBindingFailure, IrqBindingOperation,
    IrqBindingStage,
};

pub struct PciIrqLease {
    pub(super) provider: DeviceId,
    pub(super) allocation: Option<MsiAllocation>,
    pub(super) binding: BindingInfo,
    pub(super) table: MsixTableRegion,
    pub(super) table_mmio: Option<mmio_api::Mmio>,
    pub(super) endpoint: Option<Endpoint>,
    pub(super) quarantine: Option<PciMsiQuarantineReservation>,
}

pub type PciMsixAllocation = PciIrqLease;

/// Read-only MSI-X facts collected while the PCI probe still owns the endpoint.
#[cfg(feature = "nvme")]
pub(crate) struct PciMsixPreflight {
    pub(super) info: PciInfo,
    pub(super) target: PciMsiTarget,
    pub(super) vector_count: u16,
    pub(super) table_info: MsixTableInfo,
    pub(super) table_range: Range<usize>,
    pub(super) host_resources: BindingInfo,
}

/// Ownership result of a failed MSI-X activation transaction.
#[cfg(feature = "nvme")]
pub(crate) enum PciMsixActivationFailure {
    /// Hardware-visible changes were fully rolled back, so probing may restore
    /// the endpoint and report the ordinary activation error.
    Returned {
        endpoint: Endpoint,
        error: OnProbeError,
    },
    /// Some hardware owner remains retained in the named quarantine registry.
    /// The probe framework must treat the endpoint as permanently claimed.
    Claimed { error: OnProbeError },
}

impl PciIrqLease {
    /// Validates every read-only MSI-X prerequisite before endpoint ownership
    /// leaves the PCI probe slot.
    #[cfg(feature = "nvme")]
    pub(crate) fn preflight(
        endpoint: &Endpoint,
        info: PciInfo,
        vector_count: u16,
    ) -> Result<PciMsixPreflight, OnProbeError> {
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
        drop(
            provider
                .lock()
                .map_err(|_| OnProbeError::other("failed to lock MSI provider"))?,
        );

        Ok(PciMsixPreflight {
            info,
            target,
            vector_count,
            table_info,
            table_range,
            host_resources,
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

    fn retain_quarantined_resources(
        &mut self,
        allocation: MsiAllocation,
        reason: PciMsiQuarantineReason,
    ) {
        self.quarantine
            .take()
            .expect("live MSI-X lease retains one quarantine reservation")
            .retain(
                allocation,
                self.table_mmio.take(),
                self.endpoint
                    .take()
                    .expect("live MSI-X lease retains its PCI endpoint"),
                reason,
            );
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
            let allocation = self
                .allocation
                .take()
                .expect("live MSI-X lease retains its vector allocation");
            self.retain_quarantined_resources(allocation, PciMsiQuarantineReason::LeaseContainment);
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
            self.quarantine
                .take()
                .expect("live MSI-X lease retains one quarantine reservation")
                .release();
            return;
        };
        let provider = match rdrive::get::<Msi>(self.provider) {
            Ok(provider) => provider,
            Err(error) => {
                warn!("failed to find MSI provider during lease release: {error:?}");
                self.retain_quarantined_resources(
                    allocation,
                    PciMsiQuarantineReason::ProviderRelease,
                );
                return;
            }
        };
        let mut provider = match provider.lock() {
            Ok(provider) => provider,
            Err(error) => {
                warn!("failed to lock MSI provider during lease release: {error:?}");
                self.retain_quarantined_resources(
                    allocation,
                    PciMsiQuarantineReason::ProviderRelease,
                );
                return;
            }
        };
        match provider.free(allocation) {
            Ok(()) => self
                .quarantine
                .take()
                .expect("live MSI-X lease retains one quarantine reservation")
                .release(),
            Err(failure) => {
                let (allocation, error) = failure.into_parts();
                warn!("failed to free MSI-X allocation: {error:?}");
                self.retain_quarantined_resources(
                    allocation,
                    PciMsiQuarantineReason::ProviderRelease,
                );
            }
        }
    }
}
