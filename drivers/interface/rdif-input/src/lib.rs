#![no_std]

extern crate alloc;

mod error;
mod event;
mod id;
mod interface;

pub use error::*;
pub use event::*;
pub use id::*;
pub use interface::*;
pub use rdif_base::{DriverGeneric, KError, io};

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

#[cfg(test)]
mod tests {
    use super::*;

    struct TestInput;

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
                fuzz: 0,
                flat: 0,
                res: 10,
            })
        }
    }

    #[test]
    fn input_interface_exposes_identity_and_events() {
        let mut input = TestInput;
        assert_eq!(input.device_id().vendor, 1);
        assert_eq!(input.physical_location(), "virtio/input0");

        let mut bits = [0; 2];
        assert!(input.get_event_bits(EventType::Key, &mut bits).unwrap());
        assert_eq!(bits[0], 1);
        let event = input.read_event().unwrap();
        assert_eq!(event.code, 30);
        assert_eq!(event.value, -1);
        assert_eq!(input.get_abs_info(0).unwrap().min, -100);
        assert_eq!(
            input.handle_irq(),
            Event {
                handled: false,
                input_ready: false
            }
        );
    }
}
