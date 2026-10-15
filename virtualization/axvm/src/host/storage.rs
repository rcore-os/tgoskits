//! Conservative ownership checks before host storage handoff.

use std::string::String;

use axvm_types::{HostAddressAssignment, HostDeviceAssignment};

#[cfg(feature = "host-fs")]
use crate::VmConfigSnapshot;

/// Determines whether a prepared VM assigns any registered host block controller.
///
/// `None` means ownership cannot be proven from the available firmware identity;
/// callers must retain their existing conservative handoff policy. No device,
/// filesystem, interrupt, or DMA state is changed by this observation.
#[cfg(feature = "host-fs")]
pub fn guest_uses_host_block_controller(config: &VmConfigSnapshot) -> Option<bool> {
    let controllers = ax_std::os::arceos::driver::block::registered_block_controller_paths();
    assigned_controller(
        &config.passthrough_devices,
        &config.passthrough_addresses,
        &controllers,
    )
}

fn assigned_controller(
    devices: &[HostDeviceAssignment],
    addresses: &[HostAddressAssignment],
    controllers: &[Option<String>],
) -> Option<bool> {
    if !addresses.is_empty()
        || controllers.is_empty()
        || devices.iter().any(|device| !device.name.starts_with('/'))
        || controllers.iter().any(Option::is_none)
    {
        return None;
    }
    Some(controllers.iter().flatten().any(|controller| {
        devices.iter().any(|device| {
            // A real firmware node may itself end in a generated-looking name.
            // Preserve literal ownership before considering parser suffixes.
            [device.name.as_str(), assignment_node_path(&device.name)]
                .into_iter()
                .any(|assignment| {
                    controller == assignment
                        || controller
                            .strip_prefix(assignment)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                        || assignment
                            .strip_prefix(controller.as_str())
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
        })
    }))
}

// Address parsing appends a register index and, for PCI windows, a space kind.
// Strip only the generated suffix grammar, never an arbitrary dash component.
fn assignment_node_path(name: &str) -> &str {
    let name = match name.rsplit_once("-region") {
        Some((path, index))
            if !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            path
        }
        _ => name,
    };
    for suffix in ["-mem32", "-mem64", "-io", "-ecam"] {
        if let Some(path) = name.strip_suffix(suffix) {
            return path;
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use std::{string::ToString, vec};

    use super::*;

    #[test]
    fn storage_handoff_tracks_controller_ownership_and_unknown_resources() {
        let controllers = vec![Some("/storage/controller".to_string())];
        let mut devices = vec![HostDeviceAssignment {
            name: "/sensors/camera".to_string(),
            ..Default::default()
        }];
        assert_eq!(
            assigned_controller(&devices, &[], &controllers),
            Some(false)
        );
        for path in [
            "/storage/controller",
            "/storage",
            "/storage/controller/child",
            "/storage/controller-region1",
            "/storage/controller-mem32",
            "/storage/controller-mem64-region2",
            "/storage/controller-io",
            "/storage/controller-ecam-region1",
        ] {
            devices[0].name = path.to_string();
            assert_eq!(assigned_controller(&devices, &[], &controllers), Some(true));
        }
        devices[0].name = "/storage/controller-io".to_string();
        assert_eq!(
            assigned_controller(
                &devices,
                &[],
                &[Some("/storage/controller-io/child".to_string())]
            ),
            Some(true)
        );
        devices[0].name = "/storage/controller-other".to_string();
        assert_eq!(
            assigned_controller(&devices, &[], &controllers),
            Some(false)
        );
        assert_eq!(assigned_controller(&devices, &[], &[None]), None);
        assert_eq!(assigned_controller(&devices, &[], &[]), None);
        assert_eq!(
            assigned_controller(&devices, &[HostAddressAssignment::default()], &controllers),
            None
        );
        devices[0].name = "opaque-controller".to_string();
        assert_eq!(assigned_controller(&devices, &[], &controllers), None);
    }
}
