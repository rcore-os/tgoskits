extern crate alloc;

use alloc::{boxed::Box, format, string::String};

use rdrive::{
    Descriptor, DeviceId, DriverGeneric, DriverId, Platform, PlatformSource, driver::Empty,
    error::DriverError,
};

#[test]
fn rdrive_descriptor_allocates_monotonic_device_ids() {
    let first = Descriptor::new();
    let second = Descriptor::new();

    assert!(second.device_id() > first.device_id());
    assert_eq!(first.name, "");
    assert_eq!(first.irq_parent, None);
    assert_ne!(DeviceId::new(), DeviceId::new());
}

#[test]
fn rdrive_custom_ids_round_trip_and_debug_as_raw_values() {
    let driver_from_usize = DriverId::from(7_usize);
    let driver_from_u32 = DriverId::from(8_u32);

    assert_eq!(u64::from(driver_from_usize), 7);
    assert_eq!(u64::from(driver_from_u32), 8);
    assert_eq!(format!("{driver_from_usize:?}"), "7");
}

#[test]
fn rdrive_driver_errors_preserve_source_categories() {
    let unsupported = DriverError::Unsupported("acpi");
    assert_eq!(format!("{unsupported}"), "unsupported driver source: acpi");

    let boxed: Box<dyn core::error::Error> = Box::new(DriverError::Unknown(String::from("inner")));
    let converted = DriverError::from(boxed);
    assert!(matches!(converted, DriverError::Unknown(_)));

    let fdt_error = DriverError::Fdt(String::from("bad header"));
    assert!(format!("{fdt_error}").contains("bad header"));
}

#[test]
fn rdrive_empty_driver_and_static_platform_are_lightweight_values() {
    let empty = Empty;
    assert_eq!(empty.name(), "Empty Driver");
    assert!(matches!(Platform::Static, Platform::Static));
    assert!(matches!(PlatformSource::Static, PlatformSource::Static));
}
