use alloc::{boxed::Box, format, string::String};

use axtest::prelude::*;

use crate::{
    Descriptor, DeviceId, DriverGeneric, DriverId, Platform, PlatformSource, driver::Empty,
    error::DriverError,
};

#[axtest]
fn rdrive_descriptor_allocates_monotonic_device_ids() {
    let first = Descriptor::new();
    let second = Descriptor::new();

    ax_assert!(second.device_id() > first.device_id());
    ax_assert_eq!(first.name, "");
    ax_assert_eq!(first.irq_parent, None);
    ax_assert_ne!(DeviceId::new(), DeviceId::new());
}

#[axtest]
fn rdrive_custom_ids_round_trip_and_debug_as_raw_values() {
    let driver_from_usize = DriverId::from(7_usize);
    let driver_from_u32 = DriverId::from(8_u32);

    ax_assert_eq!(u64::from(driver_from_usize), 7);
    ax_assert_eq!(u64::from(driver_from_u32), 8);
    ax_assert_eq!(format!("{driver_from_usize:?}"), "7");
}

#[axtest]
fn rdrive_driver_errors_preserve_source_categories() {
    let unsupported = DriverError::Unsupported("acpi");
    ax_assert_eq!(format!("{unsupported}"), "unsupported driver source: acpi");

    let boxed: Box<dyn core::error::Error> = Box::new(DriverError::Unknown(String::from("inner")));
    let converted = DriverError::from(boxed);
    ax_assert!(matches!(converted, DriverError::Unknown(_)));

    let fdt_error = DriverError::Fdt(String::from("bad header"));
    ax_assert!(format!("{fdt_error}").contains("bad header"));
}

#[axtest]
fn rdrive_empty_driver_and_static_platform_are_lightweight_values() {
    let empty = Empty;
    ax_assert_eq!(empty.name(), "Empty Driver");
    ax_assert!(matches!(Platform::Static, Platform::Static));
    ax_assert!(matches!(PlatformSource::Static, PlatformSource::Static));
}
