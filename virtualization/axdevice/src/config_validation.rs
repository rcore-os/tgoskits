//! Validation helpers for immutable device descriptors.

use axvm_types::EmulatedDeviceConfig;

use crate::{DeviceManagerError, DeviceManagerResult};

/// Rejects a device descriptor that differs from its immutable machine plan.
///
/// Comparing the complete configuration keeps validation exhaustive when the
/// descriptor gains a new field.
pub fn validate_device_config(
    expected: &EmulatedDeviceConfig,
    actual: &EmulatedDeviceConfig,
    operation: &'static str,
) -> DeviceManagerResult {
    if expected == actual {
        return Ok(());
    }

    Err(DeviceManagerError::InvalidConfig {
        operation,
        detail: alloc::format!(
            "device '{}' does not match the immutable machine plan",
            actual.name
        ),
    })
}

#[cfg(test)]
mod tests {
    use axvm_types::EmulatedDeviceType;

    use super::*;

    fn config() -> EmulatedDeviceConfig {
        EmulatedDeviceConfig {
            name: "controller".into(),
            base_gpa: 0x1000,
            length: 0x1000,
            irq_id: 7,
            emu_type: EmulatedDeviceType::InterruptController,
            cfg_list: alloc::vec![1, 2],
        }
    }

    #[test]
    fn validates_the_complete_machine_descriptor() {
        let expected = config();
        assert_eq!(
            validate_device_config(&expected, &expected, "build device"),
            Ok(())
        );

        let mut changed = expected.clone();
        changed.cfg_list.push(3);
        assert!(matches!(
            validate_device_config(&expected, &changed, "build device"),
            Err(DeviceManagerError::InvalidConfig {
                operation: "build device",
                ..
            })
        ));
    }
}
