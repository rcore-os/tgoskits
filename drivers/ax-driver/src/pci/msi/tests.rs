use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use irq_framework::{HwIrq, IrqDomainId, IrqId};
use rdif_msi::{MsiEventId, MsiVector, MsiVectorIndex};
use rdrive::{
    DeviceId,
    probe::{OnProbeError, pci::PciAddress},
};

use super::{
    routing::{msi_provider_lookup_error, pci_requester_id, resolve_msi_map},
    transaction::*,
};
use crate::{
    BindingInfo, BindingIrq, BindingIrqBinding, IrqBindingFailure, IrqBindingFault,
    IrqBindingOperation, IrqBindingStage,
};

#[test]
fn requester_id_uses_bus_device_function() {
    let address = PciAddress::new(0, 3, 4, 2);
    assert_eq!(pci_requester_id(address), 0x322);
}

#[test]
fn msi_map_matches_masked_requester_id() {
    let mut host = fdt_edit::Node::new("pcie@0");
    host.set_property(prop_u32s("msi-map-mask", &[0xff]));
    host.set_property(prop_u32s("msi-map", &[0x40, 1, 0x1000, 0x40]));

    // Provider phandle lookup is intentionally outside this pure parser
    // test; the tuple walk should reject non-matching masked RIDs first.
    assert!(resolve_msi_map(&host, 0x20).unwrap().is_none());
}

#[test]
fn missing_msi_provider_is_unsupported_for_legacy_fallback() {
    let err = msi_provider_lookup_error(
        PciAddress::new(0, 0, 1, 0),
        DeviceId::from(7),
        rdrive::GetDeviceError::NotFound,
    );

    assert!(matches!(
        err,
        OnProbeError::Unsupported("PCI MSI provider is not registered")
    ));
}

#[test]
fn non_msi_controller_interface_is_unsupported_for_legacy_fallback() {
    let err = msi_provider_lookup_error(
        PciAddress::new(0, 0, 1, 0),
        DeviceId::from(7),
        rdrive::GetDeviceError::TypeNotMatch,
    );

    assert!(matches!(
        err,
        OnProbeError::Unsupported("PCI MSI provider interface is unavailable")
    ));
}

#[test]
fn binding_info_keeps_host_resources_and_uses_leaf_irq_not_parent_lpi() {
    let parent_irq = IrqId::new(IrqDomainId(7), HwIrq(8192));
    let leaf_irq = IrqId::new(IrqDomainId(8), HwIrq(0));
    let range = crate::HostMmioRange::try_new(0x8000_0000, 0x4000).unwrap();
    let resources = BindingInfo::empty().with_host_resources(
        crate::BindingLocator::Pci {
            segment: 1,
            bus: 2,
            device: 3,
            function: 4,
        },
        alloc::vec![range],
    );
    let info = binding_info_from_msi_vectors_with_host_resources(
        &[MsiVector::with_parent(
            MsiVectorIndex(0),
            MsiEventId(32),
            leaf_irq,
            parent_irq,
        )],
        &resources,
    );

    assert_eq!(
        info.irq_sources(),
        &[BindingIrqBinding {
            source_id: 0,
            irq: BindingIrq::id(leaf_irq),
        }]
    );
    assert_eq!(info.locator(), resources.locator());
    assert_eq!(info.host_mmio_ranges(), &[range]);
}

#[test]
fn enable_rolls_back_every_prior_vector_after_table_failure() {
    let vectors = test_vectors();
    let transitions = MockVectorTransitions::new();
    transitions.fail_table.replace(Some((1, false)));

    let result = enable_vector_bindings(
        &vectors,
        &mut |vector, enabled| transitions.set_provider(vector, enabled),
        &mut |vector, masked| transitions.set_table(vector, masked),
    );

    let error = result.unwrap_err();
    assert_eq!(error.operation(), IrqBindingOperation::Enable);
    assert_eq!(error.fault().stage(), IrqBindingStage::TableEntry);
    assert_eq!(error.fault().source_id(), Some(1));
    assert_eq!(error.rollback_fault(), None);
    assert_eq!(*transitions.provider_enabled.borrow(), [false; 3]);
    assert_eq!(*transitions.table_masked.borrow(), [true; 3]);
    assert_eq!(
        *transitions.log.borrow(),
        alloc::vec![
            Transition::Provider(0, true),
            Transition::Table(0, false),
            Transition::Provider(1, true),
            Transition::Table(1, false),
            Transition::Table(1, true),
            Transition::Table(0, true),
            Transition::Provider(1, false),
            Transition::Provider(0, false),
        ]
    );
}

#[test]
fn enable_reports_rollback_failure_after_attempting_remaining_cleanup() {
    let vectors = test_vectors();
    let transitions = MockVectorTransitions::new();
    transitions.fail_table.replace(Some((1, false)));
    transitions.fail_provider.replace(Some((0, false)));

    let error = enable_vector_bindings(
        &vectors,
        &mut |vector, enabled| transitions.set_provider(vector, enabled),
        &mut |vector, masked| transitions.set_table(vector, masked),
    )
    .unwrap_err();

    let rollback = error.rollback_fault().unwrap();
    assert_eq!(rollback.stage(), IrqBindingStage::ProviderVector);
    assert_eq!(rollback.source_id(), Some(0));
    assert!(transitions.provider_enabled.borrow()[0]);
    assert!(!transitions.provider_enabled.borrow()[1]);
    assert!(
        transitions
            .log
            .borrow()
            .contains(&Transition::Provider(1, false))
    );
}

#[test]
fn provider_enable_failure_conservatively_rolls_back_the_current_vector() {
    let vectors = test_vectors();
    let transitions = MockVectorTransitions::new();
    transitions.fail_provider.replace(Some((1, true)));

    let error = enable_vector_bindings(
        &vectors,
        &mut |vector, enabled| transitions.set_provider(vector, enabled),
        &mut |vector, masked| transitions.set_table(vector, masked),
    )
    .unwrap_err();

    assert_eq!(error.fault().stage(), IrqBindingStage::ProviderVector);
    assert_eq!(error.fault().source_id(), Some(1));
    assert_eq!(*transitions.provider_enabled.borrow(), [false; 3]);
    assert_eq!(*transitions.table_masked.borrow(), [true; 3]);
    assert_eq!(
        *transitions.log.borrow(),
        alloc::vec![
            Transition::Provider(0, true),
            Transition::Table(0, false),
            Transition::Provider(1, true),
            Transition::Table(1, true),
            Transition::Table(0, true),
            Transition::Provider(1, false),
            Transition::Provider(0, false),
        ]
    );
}

#[test]
fn disable_attempts_every_table_and_provider_operation_and_keeps_first_error() {
    let vectors = test_vectors();
    let transitions = MockVectorTransitions::enabled();
    transitions.fail_table.replace(Some((0, true)));
    transitions.fail_provider.replace(Some((0, false)));

    let table_fault = mask_vector_table_entries(&vectors, &mut |vector, masked| {
        transitions.set_table(vector, masked)
    });
    let provider_fault = disable_provider_vectors(&vectors, &mut |vector, enabled| {
        transitions.set_provider(vector, enabled)
    });
    let first_fault = table_fault.or(provider_fault).unwrap();

    assert_eq!(first_fault.stage(), IrqBindingStage::TableEntry);
    assert_eq!(first_fault.source_id(), Some(0));
    assert_eq!(*transitions.provider_enabled.borrow(), [true, false, false]);
    assert_eq!(*transitions.table_masked.borrow(), [false, true, true]);
    assert_eq!(transitions.log.borrow().len(), 6);
}

#[test]
fn setup_rollback_attempts_every_containment_step_after_one_failure() {
    let vectors = test_vectors();
    let mut transitions = Vec::new();

    let complete = rollback_msix_setup_steps(&vectors, |step| {
        let should_fail = matches!(
            step,
            MsixSetupRollbackStep::TableEntry(vector) if vector.index.0 == 1
        );
        transitions.push(match step {
            MsixSetupRollbackStep::FunctionMask => SetupRollbackTransition::FunctionMask,
            MsixSetupRollbackStep::TableEntry(vector) => {
                SetupRollbackTransition::Table(vector.index.0)
            }
            MsixSetupRollbackStep::ProviderVector(vector) => {
                SetupRollbackTransition::Provider(vector.index.0)
            }
            MsixSetupRollbackStep::DisableCapability => SetupRollbackTransition::DisableCapability,
        });
        if should_fail { Err(()) } else { Ok(()) }
    });

    assert!(!complete);
    assert_eq!(
        transitions,
        alloc::vec![
            SetupRollbackTransition::FunctionMask,
            SetupRollbackTransition::Table(0),
            SetupRollbackTransition::Table(1),
            SetupRollbackTransition::Table(2),
            SetupRollbackTransition::Provider(0),
            SetupRollbackTransition::Provider(1),
            SetupRollbackTransition::Provider(2),
            SetupRollbackTransition::DisableCapability,
        ]
    );
}

#[test]
fn failed_drop_retains_endpoint_allocation_and_table_mapping() {
    let allocation_dropped = Cell::new(false);
    let mapping_dropped = Cell::new(false);
    let endpoint_dropped = Cell::new(false);
    let mut allocation = Some(DropProbe(&allocation_dropped));
    let mut mapping = Some(DropProbe(&mapping_dropped));
    let mut endpoint = Some(DropProbe(&endpoint_dropped));

    retain_failed_lease_resources(&mut allocation, &mut mapping, &mut endpoint);

    assert!(allocation.is_none());
    assert!(mapping.is_none());
    assert!(endpoint.is_none());
    assert!(!allocation_dropped.get());
    assert!(!mapping_dropped.get());
    assert!(!endpoint_dropped.get());
}

fn prop_u32s(name: &str, values: &[u32]) -> fdt_edit::Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(&value.to_be_bytes());
    }
    fdt_edit::Property::new(name, data)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition {
    Provider(u16, bool),
    Table(u16, bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupRollbackTransition {
    FunctionMask,
    Table(u16),
    Provider(u16),
    DisableCapability,
}

struct DropProbe<'a>(&'a Cell<bool>);

impl Drop for DropProbe<'_> {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

struct MockVectorTransitions {
    provider_enabled: RefCell<[bool; 3]>,
    table_masked: RefCell<[bool; 3]>,
    fail_provider: RefCell<Option<(u16, bool)>>,
    fail_table: RefCell<Option<(u16, bool)>>,
    log: RefCell<Vec<Transition>>,
}

impl MockVectorTransitions {
    fn new() -> Self {
        Self {
            provider_enabled: RefCell::new([false; 3]),
            table_masked: RefCell::new([true; 3]),
            fail_provider: RefCell::new(None),
            fail_table: RefCell::new(None),
            log: RefCell::new(Vec::new()),
        }
    }

    fn enabled() -> Self {
        Self {
            provider_enabled: RefCell::new([true; 3]),
            table_masked: RefCell::new([false; 3]),
            fail_provider: RefCell::new(None),
            fail_table: RefCell::new(None),
            log: RefCell::new(Vec::new()),
        }
    }

    fn set_provider(&self, vector: &MsiVector, enabled: bool) -> Result<(), IrqBindingFault> {
        let index = vector.index.0;
        self.log
            .borrow_mut()
            .push(Transition::Provider(index, enabled));
        if *self.fail_provider.borrow() == Some((index, enabled)) {
            return Err(provider_vector_fault(
                vector,
                irq_framework::IrqError::Controller,
            ));
        }
        self.provider_enabled.borrow_mut()[usize::from(index)] = enabled;
        Ok(())
    }

    fn set_table(&self, vector: &MsiVector, masked: bool) -> Result<(), IrqBindingFault> {
        let index = vector.index.0;
        self.log.borrow_mut().push(Transition::Table(index, masked));
        if *self.fail_table.borrow() == Some((index, masked)) {
            return Err(IrqBindingFault::new(
                IrqBindingStage::TableEntry,
                Some(usize::from(index)),
                IrqBindingFailure::InvalidVector,
            ));
        }
        self.table_masked.borrow_mut()[usize::from(index)] = masked;
        Ok(())
    }
}

fn test_vectors() -> [MsiVector; 3] {
    core::array::from_fn(|index| {
        MsiVector::new(
            MsiVectorIndex(index as u16),
            MsiEventId(index as u32),
            IrqId::new(IrqDomainId(8), HwIrq(index as u32)),
        )
    })
}
