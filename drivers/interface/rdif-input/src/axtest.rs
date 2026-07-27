use axtest::prelude::*;

use crate::{
    AbsInfo, DriverGeneric, Event, EventType, InputDeviceId, InputError, InputEvent, Interface, io,
};

struct TestInput {
    irq_enabled: bool,
}

impl DriverGeneric for TestInput {
    fn name(&self) -> &str {
        "test-input"
    }
}

impl Interface for TestInput {
    fn device_id(&self) -> InputDeviceId {
        InputDeviceId {
            bus_type: 3,
            vendor: 1,
            product: 2,
            version: 1,
        }
    }

    fn physical_location(&self) -> &str {
        "virtio/input0"
    }

    fn unique_id(&self) -> &str {
        "input0"
    }

    fn irq_num(&self) -> Option<usize> {
        Some(5)
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError> {
        if ty == EventType::Key && !out.is_empty() {
            out[0] = 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn read_event(&mut self) -> Result<InputEvent, InputError> {
        Ok(InputEvent {
            event_type: EventType::Key as u16,
            code: 30,
            value: -1,
        })
    }

    fn get_abs_info(&mut self, _axis: u8) -> Result<AbsInfo, InputError> {
        Ok(AbsInfo {
            min: -100,
            max: 100,
            fuzz: 1,
            flat: 2,
            res: 10,
        })
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        Event {
            handled: true,
            input_ready: true,
        }
    }
}

#[axtest]
fn rdif_input_interface_exposes_identity_events_and_irq_state() {
    let mut input = TestInput { irq_enabled: false };

    ax_assert_eq!(input.name(), "test-input");
    ax_assert_eq!(input.device_id().vendor, 1);
    ax_assert_eq!(input.physical_location(), "virtio/input0");
    ax_assert_eq!(input.unique_id(), "input0");
    ax_assert_eq!(input.irq_num(), Some(5));

    let mut bits = [0; 2];
    ax_assert!(input.get_event_bits(EventType::Key, &mut bits).unwrap());
    ax_assert_eq!(bits[0], 1);
    ax_assert!(!input.get_event_bits(EventType::Led, &mut bits).unwrap());

    let event = input.read_event().unwrap();
    ax_assert_eq!(event.event_type, EventType::Key as u16);
    ax_assert_eq!(event.code, 30);
    ax_assert_eq!(event.value, -1);
    ax_assert_eq!(input.get_abs_info(0).unwrap().res, 10);

    input.enable_irq();
    ax_assert!(input.is_irq_enabled());
    ax_assert_eq!(
        input.handle_irq(),
        Event {
            handled: true,
            input_ready: true
        }
    );
    input.disable_irq();
    ax_assert!(!input.is_irq_enabled());
}

#[axtest]
fn rdif_input_event_types_and_defaults_are_stable() {
    ax_assert_eq!(EventType::COUNT, 0x20);
    ax_assert_eq!(EventType::Synchronization.bits_count(), 0x10);
    ax_assert_eq!(EventType::Key.bits_count(), 0x300);
    ax_assert_eq!(EventType::Relative.bits_count(), 0x10);
    ax_assert_eq!(EventType::Absolute.bits_count(), 0x40);
    ax_assert_eq!(EventType::Misc.bits_count(), 0x08);
    ax_assert_eq!(EventType::Switch.bits_count(), 0x12);
    ax_assert_eq!(EventType::Led.bits_count(), 0x10);
    ax_assert_eq!(EventType::Sound.bits_count(), 0x08);
    ax_assert_eq!(EventType::ForceFeedback.bits_count(), 0x80);
    ax_assert_eq!(Event::none().handled, false);
    ax_assert_eq!(Event::none().input_ready, false);
}

#[axtest]
fn rdif_input_default_methods_and_error_mapping_hold() {
    struct MinimalInput;

    impl DriverGeneric for MinimalInput {
        fn name(&self) -> &str {
            "minimal-input"
        }
    }

    impl Interface for MinimalInput {
        fn device_id(&self) -> InputDeviceId {
            InputDeviceId {
                bus_type: 0,
                vendor: 0,
                product: 0,
                version: 0,
            }
        }

        fn physical_location(&self) -> &str {
            ""
        }

        fn unique_id(&self) -> &str {
            ""
        }

        fn get_event_bits(&mut self, _ty: EventType, _out: &mut [u8]) -> Result<bool, InputError> {
            Err(InputError::Again)
        }

        fn read_event(&mut self) -> Result<InputEvent, InputError> {
            Err(InputError::NotAvailable)
        }
    }

    let mut input = MinimalInput;
    let mut props = [0xff; 2];
    ax_assert!(matches!(input.get_prop_bits(&mut props), Ok(0)));
    ax_assert!(matches!(
        input.get_abs_info(0),
        Err(InputError::NotSupported)
    ));
    input.enable_irq();
    input.disable_irq();
    ax_assert!(!input.is_irq_enabled());
    ax_assert_eq!(input.handle_irq(), Event::none());

    ax_assert!(matches!(
        io::ErrorKind::from(InputError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(InputError::Again),
        io::ErrorKind::Interrupted
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(InputError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(InputError::InvalidEvent),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(InputError::Other("input backend".into())),
        io::ErrorKind::Other(_)
    ));
}

#[axtest]
fn rdif_input_event_type_and_event_hold() {
    use crate::{Event, EventType, InputEvent};

    // Test EventType variants
    let sync = EventType::Synchronization;
    let key = EventType::Key;
    let abs = EventType::Absolute;
    ax_assert!(sync != key);
    ax_assert!(key != abs);

    // Test Event
    let _event = Event::none();

    // Test InputEvent
    let input_event = InputEvent {
        event_type: EventType::Key as u16,
        code: 30, // 'a' key
        value: 1,
    };
    ax_assert_eq!(input_event.event_type, EventType::Key as u16);
}

#[axtest]
fn rdif_input_abs_info_and_device_id_hold() {
    use crate::{AbsInfo, InputDeviceId};

    // Test AbsInfo
    let abs_info = AbsInfo {
        min: 0,
        max: 255,
        fuzz: 0,
        flat: 0,
        res: 1,
    };
    ax_assert_eq!(abs_info.min, 0);
    ax_assert_eq!(abs_info.max, 255);

    // Test InputDeviceId
    let device_id = InputDeviceId {
        bus_type: 3,
        vendor: 0x1234,
        product: 0x5678,
        version: 1,
    };
    ax_assert_eq!(device_id.bus_type, 3);
    ax_assert_eq!(device_id.vendor, 0x1234);
}

#[axtest]
fn rdif_input_event_type_all_variants_hold() {
    use crate::EventType;

    // Test all EventType variants
    let _sync = EventType::Synchronization;
    let _key = EventType::Key;
    let _rel = EventType::Relative;
    let _abs = EventType::Absolute;
}
