//! Fallible MSI-X vector transitions and fail-closed rollback.

use pcie::{MsixError, MsixTableRegion};

use crate::{
    BindingInfo, BindingIrq, IrqBindingError, IrqBindingFailure, IrqBindingFault,
    IrqBindingOperation, IrqBindingStage,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MsixSetupRollbackStep<'vector> {
    FunctionMask,
    TableEntry(&'vector rdif_msi::MsiVector),
    ProviderVector(&'vector rdif_msi::MsiVector),
    DisableCapability,
}

/// Attempts every independent containment step for a partially configured
/// MSI-X endpoint.
///
/// The function-wide mask is asserted before touching individual entries.
/// Vector ownership may be released only when this returns `true`; otherwise
/// the caller must retain the provider allocation and table mapping.
pub(super) fn rollback_msix_setup_steps<E>(
    vectors: &[rdif_msi::MsiVector],
    mut apply: impl FnMut(MsixSetupRollbackStep<'_>) -> Result<(), E>,
) -> bool {
    let mut complete = apply(MsixSetupRollbackStep::FunctionMask).is_ok();
    for vector in vectors {
        complete &= apply(MsixSetupRollbackStep::TableEntry(vector)).is_ok();
    }
    for vector in vectors {
        complete &= apply(MsixSetupRollbackStep::ProviderVector(vector)).is_ok();
    }
    complete &= apply(MsixSetupRollbackStep::DisableCapability).is_ok();
    complete
}

pub(super) fn retain_failed_lease_resources<A, M, E>(
    allocation: &mut Option<A>,
    mapping: &mut Option<M>,
    endpoint: &mut Option<E>,
) {
    // Drop cannot report the failed hardware transition. Leaking these bounded
    // resources keeps the PCI function, provider allocation, and MSI-X table
    // mapping under one owner instead of releasing them underneath a possibly
    // live source.
    if let Some(allocation) = allocation.take() {
        core::mem::forget(allocation);
    }
    if let Some(mapping) = mapping.take() {
        core::mem::forget(mapping);
    }
    if let Some(endpoint) = endpoint.take() {
        core::mem::forget(endpoint);
    }
}

pub(super) fn retain_failed_setup_resources<A, M>(
    allocation: &mut Option<A>,
    mapping: &mut Option<M>,
) {
    if let Some(allocation) = allocation.take() {
        core::mem::forget(allocation);
    }
    if let Some(mapping) = mapping.take() {
        core::mem::forget(mapping);
    }
}

pub(super) fn enable_vector_bindings<P, T>(
    vectors: &[rdif_msi::MsiVector],
    provider: &mut P,
    table: &mut T,
) -> Result<(), IrqBindingError>
where
    P: FnMut(&rdif_msi::MsiVector, bool) -> Result<(), IrqBindingFault>,
    T: FnMut(&rdif_msi::MsiVector, bool) -> Result<(), IrqBindingFault>,
{
    for (index, vector) in vectors.iter().enumerate() {
        if let Err(fault) = provider(vector, true) {
            // A provider may report failure after touching hardware. Include
            // the current vector in rollback rather than assuming no effect.
            let rollback_fault = rollback_enabled_vectors(&vectors[..=index], provider, table);
            return Err(IrqBindingError::new(IrqBindingOperation::Enable, fault)
                .with_rollback_fault(rollback_fault));
        }

        if let Err(fault) = table(vector, false) {
            let rollback_fault = rollback_enabled_vectors(&vectors[..=index], provider, table);
            return Err(IrqBindingError::new(IrqBindingOperation::Enable, fault)
                .with_rollback_fault(rollback_fault));
        }
    }
    Ok(())
}

pub(super) fn rollback_enabled_vectors<P, T>(
    vectors: &[rdif_msi::MsiVector],
    provider: &mut P,
    table: &mut T,
) -> Option<IrqBindingFault>
where
    P: FnMut(&rdif_msi::MsiVector, bool) -> Result<(), IrqBindingFault>,
    T: FnMut(&rdif_msi::MsiVector, bool) -> Result<(), IrqBindingFault>,
{
    let mut first_fault = None;
    for vector in vectors.iter().rev() {
        record_first_fault(&mut first_fault, table(vector, true));
    }
    for vector in vectors.iter().rev() {
        record_first_fault(&mut first_fault, provider(vector, false));
    }
    first_fault
}

pub(super) fn mask_vector_table_entries<T>(
    vectors: &[rdif_msi::MsiVector],
    table: &mut T,
) -> Option<IrqBindingFault>
where
    T: FnMut(&rdif_msi::MsiVector, bool) -> Result<(), IrqBindingFault>,
{
    let mut first_fault = None;
    for vector in vectors {
        record_first_fault(&mut first_fault, table(vector, true));
    }
    first_fault
}

pub(super) fn disable_provider_vectors<P>(
    vectors: &[rdif_msi::MsiVector],
    provider: &mut P,
) -> Option<IrqBindingFault>
where
    P: FnMut(&rdif_msi::MsiVector, bool) -> Result<(), IrqBindingFault>,
{
    let mut first_fault = None;
    for vector in vectors {
        record_first_fault(&mut first_fault, provider(vector, false));
    }
    first_fault
}

pub(super) fn record_first_fault(
    first_fault: &mut Option<IrqBindingFault>,
    result: Result<(), IrqBindingFault>,
) {
    if first_fault.is_none()
        && let Err(fault) = result
    {
        *first_fault = Some(fault);
    }
}

pub(super) fn set_table_masked(
    table: &MsixTableRegion,
    vector: &rdif_msi::MsiVector,
    masked: bool,
) -> Result<(), MsixError> {
    if masked {
        table.mask(vector.index.0)
    } else {
        table.unmask(vector.index.0)
    }
}

pub(super) fn binding_error(
    operation: IrqBindingOperation,
    stage: IrqBindingStage,
    source_id: Option<usize>,
    failure: IrqBindingFailure,
) -> IrqBindingError {
    IrqBindingError::new(operation, IrqBindingFault::new(stage, source_id, failure))
}

pub(super) fn provider_access_error(
    operation: IrqBindingOperation,
    stage: IrqBindingStage,
    error: rdrive::GetDeviceError,
) -> IrqBindingError {
    IrqBindingError::new(operation, provider_access_fault(stage, error))
}

pub(super) fn provider_access_fault(
    stage: IrqBindingStage,
    error: rdrive::GetDeviceError,
) -> IrqBindingFault {
    let failure = match error {
        rdrive::GetDeviceError::NotFound => IrqBindingFailure::ProviderNotFound,
        rdrive::GetDeviceError::TypeNotMatch | rdrive::GetDeviceError::DeviceReleased => {
            IrqBindingFailure::ProviderUnavailable
        }
        rdrive::GetDeviceError::UsedByOthers(_) | rdrive::GetDeviceError::UsedByUnknown => {
            IrqBindingFailure::ProviderBusy
        }
    };
    IrqBindingFault::new(stage, None, failure)
}

pub(super) fn provider_vector_fault(
    vector: &rdif_msi::MsiVector,
    error: irq_framework::IrqError,
) -> IrqBindingFault {
    IrqBindingFault::new(
        IrqBindingStage::ProviderVector,
        Some(usize::from(vector.index.0)),
        IrqBindingFailure::Irq(error),
    )
}

pub(super) fn table_vector_fault(
    vector: &rdif_msi::MsiVector,
    _error: MsixError,
) -> IrqBindingFault {
    IrqBindingFault::new(
        IrqBindingStage::TableEntry,
        Some(usize::from(vector.index.0)),
        IrqBindingFailure::InvalidVector,
    )
}

pub(super) fn binding_info_from_msi_vectors(vectors: &[rdif_msi::MsiVector]) -> BindingInfo {
    let irqs = vectors
        .iter()
        .map(|vector| (usize::from(vector.index.0), BindingIrq::id(vector.irq)));
    BindingInfo::with_irq_sources(irqs)
}

pub(super) fn binding_info_from_msi_vectors_with_host_resources(
    vectors: &[rdif_msi::MsiVector],
    host_resources: &BindingInfo,
) -> BindingInfo {
    binding_info_from_msi_vectors(vectors).with_host_resources(
        host_resources.locator().clone(),
        host_resources.host_mmio_ranges().to_vec(),
    )
}
