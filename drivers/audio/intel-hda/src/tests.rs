use super::*;

#[test]
fn pci_admission_is_limited_to_intel_hda_class_functions() {
    let device = identify(0x8086, 0x54c8, 0x04, 0x03, 0).unwrap();
    assert_eq!(device.vendor_id(), 0x8086);
    assert_eq!(device.device_id(), 0x54c8);
    assert!(identify(0x1234, 0x54c8, 0x04, 0x03, 0).is_none());
    assert!(identify(0x8086, 0x54c8, 0x04, 0x01, 0).is_none());
    assert!(identify(0x8086, 0x54c8, 0x04, 0x03, 1).is_none());
}

#[test]
fn playback_stream_follows_the_advertised_input_streams() {
    assert_eq!(bringup::first_playback_stream_offset(1 << 12), Ok(0x80));
    assert_eq!(
        bringup::first_playback_stream_offset((4 << 8) | (3 << 12)),
        Ok(0x100)
    );
    assert!(matches!(
        bringup::first_playback_stream_offset(4 << 8),
        Err(Error::Unsupported)
    ));
}

#[test]
fn mapped_register_access_rejects_short_unaligned_and_overflowing_ranges() {
    assert_eq!(checked_register_range(8, 4, RegisterWidth::Dword), Ok(()));
    assert_eq!(
        checked_register_range(7, 4, RegisterWidth::Dword),
        Err(Error::RegisterRange)
    );
    assert_eq!(
        checked_register_range(8, 2, RegisterWidth::Dword),
        Err(Error::RegisterRange)
    );
    assert_eq!(
        checked_register_range(usize::MAX, usize::MAX, RegisterWidth::Word),
        Err(Error::RegisterRange)
    );
}

#[test]
fn codec_route_search_finds_supported_analog_output_path() {
    let widgets = [
        Widget {
            node: 1,
            caps: (4 << 20) | (1 << 8),
            pin_caps: 1 << 4,
            config: 0,
            connections: alloc::vec![2],
        },
        Widget {
            node: 2,
            caps: (3 << 20) | (1 << 8),
            pin_caps: 0,
            config: 0,
            connections: alloc::vec![3],
        },
        Widget {
            node: 3,
            caps: 1,
            pin_caps: 0,
            config: 0,
            connections: alloc::vec![],
        },
    ];

    let route = codec::find_route(&widgets).unwrap();
    assert_eq!(
        route
            .iter()
            .map(|widget| widget.node)
            .collect::<alloc::vec::Vec<_>>(),
        alloc::vec![1, 2, 3]
    );
}

#[test]
fn malformed_codec_child_ranges_are_rejected_before_node_access() {
    struct InvalidChildren;
    impl codec::Verbs for InvalidChildren {
        fn verb(&mut self, _codec: u8, _node: u8, operation: u16, _payload: u16) -> Result<u32> {
            Ok(if operation == 0xf00 { 0x7f0002 } else { 0 })
        }
    }

    assert!(matches!(
        codec::enumerate(&mut InvalidChildren, 1),
        Err(Error::InvalidParam)
    ));
}
