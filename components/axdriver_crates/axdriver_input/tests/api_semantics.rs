use ax_driver_input::{EventType, InputDeviceId};

#[test]
fn test_event_type_from_repr_roundtrip() {
    let ty = EventType::from_repr(EventType::Key as u8);
    assert_eq!(ty, Some(EventType::Key));

    let ty = EventType::from_repr(EventType::Relative as u8);
    assert_eq!(ty, Some(EventType::Relative));

    let ty = EventType::from_repr(0xff);
    assert_eq!(ty, None);
}

#[test]
fn test_event_type_bits_count_is_non_zero_for_supported_types() {
    assert!(EventType::Synchronization.bits_count() > 0);
    assert!(EventType::Key.bits_count() > 0);
    assert!(EventType::Relative.bits_count() > 0);
    assert!(EventType::Absolute.bits_count() > 0);
}

#[test]
fn test_input_device_id_ordering_is_lexicographic() {
    let a = InputDeviceId {
        bus_type: 1,
        vendor: 2,
        product: 3,
        version: 4,
    };
    let b = InputDeviceId {
        bus_type: 1,
        vendor: 2,
        product: 3,
        version: 5,
    };

    assert!(a < b);
}

