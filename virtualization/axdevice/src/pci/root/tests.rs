use alloc::sync::Arc;

use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};

use super::*;
use crate::{
    ConfigOffset, DeviceNodeId, PciCapabilityId, PciCapabilitySpec, PciClass, PciEndpointIdentity,
    PciFunctionSpec, PciIntxPin, PciIntxRequirement, PciIntxRouter, PciMemoryBar, PciSegment,
    PciTopologyBuilder, ResourceRequest, ResourceSlot,
};

const APERTURE_START: u64 = 0x2000_0000;
const APERTURE_END: u64 = 0x2040_0000;
const BAR_SIZE: u64 = 0x1_0000;

#[test]
fn exposes_a_256_byte_type_zero_config_image() {
    let (root, endpoint_bdf, _) = root_with_bar();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(0), AccessWidth::Dword)
            .unwrap(),
        0x5678_1234
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(8), AccessWidth::Dword)
            .unwrap(),
        0x0500_0001
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x0e), AccessWidth::Byte)
            .unwrap(),
        0
    );
    assert!(matches!(
        ConfigOffset::new(0x100),
        Err(PciError::InvalidAddress { .. })
    ));
}

#[test]
fn absent_functions_read_all_ones_and_ignore_writes() {
    let (root, ..) = root_with_bar();
    let absent = bdf(30, 0);

    assert_eq!(
        root.read_config(absent, offset(0), AccessWidth::Word)
            .unwrap(),
        0xffff
    );
    root.write_config(absent, offset(4), AccessWidth::Word, 0xffff)
        .unwrap();
    assert_eq!(
        root.read_config(absent, offset(4), AccessWidth::Word)
            .unwrap(),
        0xffff
    );
}

#[test]
fn binding_reservation_releases_command_gate_when_dropped() {
    let (root, endpoint_bdf, _) = root_with_bar();
    let function_id = node("endpoint");

    let reservation = root.reserve_endpoint_binding(&function_id).unwrap();
    let command = reservation.command();
    assert!(!command.bus_master_enable());
    assert!(matches!(
        root.write_config(endpoint_bdf, offset(4), AccessWidth::Word, 0x0406),
        Err(PciError::BindingInProgress { .. })
    ));

    drop(reservation);
    root.write_config(endpoint_bdf, offset(4), AccessWidth::Word, 0x0406)
        .unwrap();
    assert_eq!(
        root.read_config(endpoint_bdf, offset(4), AccessWidth::Word)
            .unwrap(),
        0x0406
    );
}

#[test]
fn config_space_publishes_the_resolved_intx_pin_and_line() {
    let function_id = node("intx-endpoint");
    let bdf = bdf(1, 0);
    let function = PciFunctionSpec::new(
        function_id.clone(),
        PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
    )
    .with_bdf(ResourceRequest::Fixed(bdf))
    .with_intx(PciIntxRequirement::new(
        PciIntxPin::C,
        ResourceSlot::new("intx").unwrap(),
    ))
    .unwrap();
    let large_line_router = PciIntxRouter::new(
        InterruptControllerId::new(0),
        [
            ControllerInputId::new(16),
            ControllerInputId::new(17),
            ControllerInputId::new(18),
            ControllerInputId::new(19),
        ],
        [300, 301, 302, 303],
        InterruptTrigger::LevelTriggered,
        InterruptSharing::Shared,
    );
    let large_line_route = large_line_router
        .resolve(&function_id, bdf, PciIntxPin::C)
        .unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder.add_function(function).unwrap();
    builder
        .set_intx_route(&function_id, large_line_route)
        .unwrap();
    let topology = Arc::new(builder.resolve(APERTURE_START..APERTURE_END).unwrap());
    let root = PciRootState::new(topology);

    assert_eq!(
        root.read_config(bdf, offset(0x3c), AccessWidth::Byte)
            .unwrap(),
        u64::from(u8::MAX)
    );
    assert_eq!(
        root.read_config(bdf, offset(0x3d), AccessWidth::Byte)
            .unwrap(),
        3
    );
}

#[test]
fn rejects_misaligned_and_qword_config_accesses() {
    let (root, endpoint_bdf, _) = root_with_bar();

    assert!(matches!(
        root.read_config(endpoint_bdf, offset(1), AccessWidth::Word),
        Err(PciError::InvalidConfigAccess { .. })
    ));
    assert!(matches!(
        root.read_config(endpoint_bdf, offset(0), AccessWidth::Qword),
        Err(PciError::InvalidConfigAccess { .. })
    ));
}

#[test]
fn serializes_capabilities_and_subsystem_ids_into_root_owned_config() {
    let first = PciCapabilitySpec::new(
        PciCapabilityId::new(1),
        alloc::vec![0xa1, 0xb2],
        alloc::vec![0, 0],
    )
    .unwrap();
    let second = PciCapabilitySpec::new(
        PciCapabilityId::new(2),
        alloc::vec![0, 0, 0x11, 0x22],
        alloc::vec![0, 0, 0xff, 0xff],
    )
    .unwrap();
    let endpoint = PciFunctionSpec::new(
        node("capability-endpoint"),
        PciEndpointIdentity::new(0x1234, 0x5678, PciClass::new(0x05, 0, 0))
            .with_subsystem_ids(0xabcd, 0x1234),
    )
    .with_capability(first)
    .with_capability(second);
    let mut builder = PciTopologyBuilder::new();
    builder.add_function(endpoint).unwrap();
    let topology = Arc::new(builder.resolve(APERTURE_START..APERTURE_END).unwrap());
    let resolved = topology.function(&node("capability-endpoint")).unwrap();
    let endpoint_bdf = resolved.bdf();
    let root = PciRootState::new(topology);

    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x2c), AccessWidth::Dword)
            .unwrap(),
        0x1234_abcd
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x06), AccessWidth::Byte)
            .unwrap(),
        0x10
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x34), AccessWidth::Byte)
            .unwrap(),
        0x40
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x40), AccessWidth::Dword)
            .unwrap(),
        0xb2a1_4401
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x44), AccessWidth::Dword)
            .unwrap(),
        0x0000_0002
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x48), AccessWidth::Word)
            .unwrap(),
        0x2211
    );

    root.write_config(endpoint_bdf, offset(0x2c), AccessWidth::Word, u64::MAX)
        .unwrap();
    root.write_config(endpoint_bdf, offset(0x40), AccessWidth::Byte, u64::MAX)
        .unwrap();
    root.write_config(endpoint_bdf, offset(0x48), AccessWidth::Word, 0x1234)
        .unwrap();
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x2c), AccessWidth::Dword)
            .unwrap(),
        0x1234_abcd
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x40), AccessWidth::Byte)
            .unwrap(),
        1
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x48), AccessWidth::Word)
            .unwrap(),
        0x1234
    );
}

#[test]
fn serializes_virtio_capability_length_before_payload_fields() {
    let body = alloc::vec![
        16, 1, 0, 0, 0, 0, 0x00, 0x01, 0x00, 0x00, 0x38, 0x00, 0x00, 0x00,
    ];
    let capability =
        PciCapabilitySpec::new(PciCapabilityId::new(0x09), body, alloc::vec![0; 14]).unwrap();
    let function_id = node("virtio-capability-endpoint");
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0x01, 0x00, 0x00)),
            )
            .with_capability(capability),
        )
        .unwrap();
    let topology = Arc::new(builder.resolve(APERTURE_START..APERTURE_END).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let root = PciRootState::new(topology);

    assert_eq!(
        root.read_config(bdf, offset(0x40), AccessWidth::Byte),
        Ok(0x09)
    );
    assert_eq!(
        root.read_config(bdf, offset(0x42), AccessWidth::Byte),
        Ok(16)
    );
    assert_eq!(
        root.read_config(bdf, offset(0x43), AccessWidth::Byte),
        Ok(1)
    );
    assert_eq!(
        root.read_config(bdf, offset(0x44), AccessWidth::Byte),
        Ok(0)
    );
    assert_eq!(
        root.read_config(bdf, offset(0x48), AccessWidth::Dword),
        Ok(0x0000_0100)
    );
    assert_eq!(
        root.read_config(bdf, offset(0x4c), AccessWidth::Dword),
        Ok(0x0000_0038)
    );
}

#[test]
fn platform_config_bytes_cannot_override_core_identity_or_bars() {
    let function = PciFunctionSpec::new(
        node("platform"),
        PciEndpointIdentity::new(0x1234, 0x5678, PciClass::new(0x06, 0, 0)),
    );
    assert!(matches!(
        function
            .clone()
            .with_platform_config_byte(ConfigOffset::new(0).unwrap(), 0, u8::MAX),
        Err(PciError::InvalidConfigPatch { offset: 0, .. })
    ));
    assert!(matches!(
        function.with_platform_config_byte(ConfigOffset::new(0x10).unwrap(), 0, u8::MAX),
        Err(PciError::InvalidConfigPatch { offset: 0x10, .. })
    ));
}

#[test]
fn command_register_accepts_memory_space_bus_master_and_interrupt_disable() {
    let (root, endpoint_bdf, bar_base) = root_with_bar();

    assert!(root.resolve_bar(bar_base, AccessWidth::Dword).is_none());
    root.write_config(endpoint_bdf, offset(4), AccessWidth::Word, 0xffff)
        .unwrap();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(4), AccessWidth::Word)
            .unwrap(),
        0x0406
    );
    let route = root.resolve_bar(bar_base + 8, AccessWidth::Dword).unwrap();
    assert_eq!(route.bdf(), endpoint_bdf);
    assert_eq!(route.bar(), PciBarIndex::new(2).unwrap());
    assert_eq!(route.offset(), 8);
    assert_eq!(route.width(), AccessWidth::Dword);
    assert!(
        root.resolve_bar(bar_base + BAR_SIZE - 2, AccessWidth::Dword)
            .is_none()
    );
}

#[test]
fn bar_probe_reports_size_without_changing_the_runtime_route() {
    let (root, endpoint_bdf, bar_base) = enabled_root_with_bar();

    root.write_config(
        endpoint_bdf,
        offset(0x18),
        AccessWidth::Dword,
        u64::from(u32::MAX),
    )
    .unwrap();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x18), AccessWidth::Dword)
            .unwrap(),
        0xffff_0000
    );
    assert!(root.resolve_bar(bar_base, AccessWidth::Byte).is_some());
}

#[test]
fn valid_bar_relocation_moves_the_route() {
    let (root, endpoint_bdf, old_base) = enabled_root_with_bar();
    let new_base = APERTURE_START + 0x10_0000;

    root.write_config(endpoint_bdf, offset(0x18), AccessWidth::Dword, new_base)
        .unwrap();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x18), AccessWidth::Dword)
            .unwrap(),
        new_base
    );
    assert!(root.resolve_bar(old_base, AccessWidth::Byte).is_none());
    assert!(root.resolve_bar(new_base, AccessWidth::Byte).is_some());
}

#[test]
fn partial_bar_write_uses_the_merged_dword_and_preserves_attributes() {
    let (root, endpoint_bdf, old_base) = enabled_root_with_bar();
    let new_base = APERTURE_START + 0x10_0000;

    root.write_config(
        endpoint_bdf,
        offset(0x1a),
        AccessWidth::Word,
        new_base >> 16,
    )
    .unwrap();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x18), AccessWidth::Dword)
            .unwrap(),
        new_base
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x18), AccessWidth::Byte)
            .unwrap(),
        0
    );
    assert!(root.resolve_bar(old_base, AccessWidth::Byte).is_none());
    assert!(root.resolve_bar(new_base, AccessWidth::Byte).is_some());
}

#[test]
fn invalid_bar_relocation_preserves_config_and_route() {
    let (root, endpoint_bdf, old_base) = enabled_root_with_bar();

    root.write_config(
        endpoint_bdf,
        offset(0x18),
        AccessWidth::Dword,
        APERTURE_START + 0x10,
    )
    .unwrap();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x18), AccessWidth::Dword)
            .unwrap(),
        old_base
    );
    assert!(root.resolve_bar(old_base, AccessWidth::Byte).is_some());
}

#[test]
fn overlapping_bar_relocation_preserves_the_previous_route() {
    let bar2 = PciBarIndex::new(2).unwrap();
    let alpha_base = APERTURE_START;
    let beta_base = APERTURE_START + BAR_SIZE;
    let mut builder = PciTopologyBuilder::new();
    for (id, base) in [("alpha", alpha_base), ("beta", beta_base)] {
        builder
            .add_function(
                function(id)
                    .with_bar(
                        PciMemoryBar::new(bar2, BAR_SIZE)
                            .unwrap()
                            .with_address(ResourceRequest::Fixed(base)),
                    )
                    .unwrap(),
            )
            .unwrap();
    }
    let topology = Arc::new(builder.resolve(APERTURE_START..APERTURE_END).unwrap());
    let alpha_bdf = topology.function(&node("alpha")).unwrap().bdf();
    let beta_bdf = topology.function(&node("beta")).unwrap().bdf();
    let root = PciRootState::new(topology);
    for endpoint_bdf in [alpha_bdf, beta_bdf] {
        root.write_config(endpoint_bdf, offset(4), AccessWidth::Word, 0x0002)
            .unwrap();
    }

    root.write_config(beta_bdf, offset(0x18), AccessWidth::Dword, alpha_base)
        .unwrap();

    assert_eq!(
        root.read_config(beta_bdf, offset(0x18), AccessWidth::Dword)
            .unwrap(),
        beta_base
    );
    assert_eq!(
        root.resolve_bar(alpha_base, AccessWidth::Byte)
            .unwrap()
            .bdf(),
        alpha_bdf
    );
    assert_eq!(
        root.resolve_bar(beta_base, AccessWidth::Byte)
            .unwrap()
            .bdf(),
        beta_bdf
    );
}

#[test]
fn reset_restores_root_owned_command_bar_and_route_state() {
    let (root, endpoint_bdf, power_on_base) = enabled_root_with_bar();
    let relocated = APERTURE_START + 0x10_0000;
    root.write_config(endpoint_bdf, offset(0x18), AccessWidth::Dword, relocated)
        .unwrap();
    assert!(root.resolve_bar(relocated, AccessWidth::Byte).is_some());

    root.reset().unwrap();

    assert_eq!(
        root.read_config(endpoint_bdf, offset(4), AccessWidth::Word)
            .unwrap(),
        0
    );
    assert_eq!(
        root.read_config(endpoint_bdf, offset(0x18), AccessWidth::Dword)
            .unwrap(),
        power_on_base
    );
    assert!(root.resolve_bar(power_on_base, AccessWidth::Byte).is_none());
}

fn function(id: &str) -> PciFunctionSpec {
    PciFunctionSpec::new(
        node(id),
        PciEndpointIdentity::new(0x1234, 0x5678, PciClass::new(0x05, 0x00, 0x00)).with_revision(1),
    )
}

fn node(id: &str) -> DeviceNodeId {
    DeviceNodeId::new(id).unwrap()
}

fn bdf(device: u8, function: u8) -> PciBdf {
    PciBdf::new(PciSegment::new(0), 0, device, function).unwrap()
}

fn offset(value: u16) -> ConfigOffset {
    ConfigOffset::new(value).unwrap()
}

fn root_with_bar() -> (PciRootState, PciBdf, u64) {
    let bar2 = PciBarIndex::new(2).unwrap();
    let endpoint = function("endpoint")
        .with_bar(PciMemoryBar::new(bar2, BAR_SIZE).unwrap())
        .unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder.add_function(endpoint).unwrap();
    let topology = Arc::new(builder.resolve(APERTURE_START..APERTURE_END).unwrap());
    let function = topology.function(&node("endpoint")).unwrap();
    let endpoint_bdf = function.bdf();
    let bar_base = function.bar(bar2).unwrap().address();
    (PciRootState::new(topology), endpoint_bdf, bar_base)
}

fn enabled_root_with_bar() -> (PciRootState, PciBdf, u64) {
    let (root, endpoint_bdf, bar_base) = root_with_bar();
    root.write_config(endpoint_bdf, offset(4), AccessWidth::Word, 0x0002)
        .unwrap();
    (root, endpoint_bdf, bar_base)
}
