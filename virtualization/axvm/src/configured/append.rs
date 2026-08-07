//! Merge machine defaults and configured virtual devices into one graph.

use super::*;

/// The machine-derived request and its non-user-configurable fixed identity.
#[derive(Clone, Debug)]
pub struct DefaultVirtualDeviceIntent {
    pub request: VirtualDeviceRequest,
    pub fixed_resources: FixedDeviceBindings,
    pub firmware_identity: Option<DeviceFirmwareBinding>,
}

pub(crate) fn append_configured_devices(
    config: &crate::config::AxVMConfig,
    nodes: &mut Vec<DeviceNodeSpec>,
    default_controller_node: &DeviceNodeId,
    default_controller: InterruptControllerId,
) -> AxVmResult {
    let base_context = DeviceInstantiationContext::new()
        .with_default_wired_controller(default_controller_node.clone(), default_controller);
    let default = default_serial_intent(config, default_controller)?;
    let request = config
        .virtual_device_requests()
        .iter()
        .find(|request| request.id == "console0")
        .unwrap_or(&default.request);
    if !crate::machine::is_serial_model(&request.model) {
        return Err(AxVmError::invalid_config(
            "console0 must use a registered virtual serial model",
        ));
    }
    let compatible = request.model == default.request.model;
    let (fixed_resources, firmware_binding) = if compatible {
        (
            default.fixed_resources.clone(),
            default.firmware_identity.clone().unwrap_or_default(),
        )
    } else {
        (FixedDeviceBindings::default(), DeviceFirmwareBinding::None)
    };
    let context = base_context.clone().with_serial_defaults(
        config.serial_profile(),
        config.serial_backend_factory(),
        fixed_resources,
        firmware_binding,
        true,
    );
    nodes.push(
        config
            .virtual_device_catalog()
            .instantiate_node(request, &context)
            .map_err(configured_error)?,
    );

    let mut host_console_owners = usize::from(serial_uses_host_console(request, true)?);
    for request in config
        .virtual_device_requests()
        .iter()
        .filter(|request| request.id != "console0")
    {
        let context = if crate::machine::is_serial_model(&request.model) {
            if serial_uses_host_console(request, false)? {
                host_console_owners += 1;
            }
            base_context.clone().with_serial_defaults(
                crate::machine::default_serial_profile(&request.model),
                config.serial_backend_factory(),
                FixedDeviceBindings::default(),
                DeviceFirmwareBinding::None,
                false,
            )
        } else {
            base_context.clone()
        };
        nodes.push(
            config
                .virtual_device_catalog()
                .instantiate_node(request, &context)
                .map_err(configured_error)?,
        );
    }
    if host_console_owners > 1 {
        return Err(AxVmError::invalid_config(
            "only one virtual serial device may own the host console backend",
        ));
    }
    Ok(())
}

fn default_serial_intent(
    config: &crate::config::AxVMConfig,
    default_controller: InterruptControllerId,
) -> AxVmResult<DefaultVirtualDeviceIntent> {
    let profile = config.serial_profile();
    let registers = ResourceSlot::new("registers")?;
    let irq = ResourceSlot::new("irq")?;
    let fixed_resources = match profile.transport {
        crate::machine::GuestSerialTransport::Port { base, length } => {
            FixedDeviceBindings::default().with_pio(registers, base, length)
        }
        crate::machine::GuestSerialTransport::Mmio { base, length, .. } => {
            FixedDeviceBindings::default().with_mmio(registers, base as u64, length as u64)
        }
    }
    .with_wired(
        irq,
        FixedWiredBinding {
            controller: default_controller,
            input: ControllerInputId::new(profile.irq),
            trigger: InterruptTrigger::LevelTriggered,
            sharing: InterruptSharing::Exclusive,
        },
    );
    Ok(DefaultVirtualDeviceIntent {
        request: VirtualDeviceRequest {
            id: "console0".into(),
            model: crate::machine::serial_model_name(profile).into(),
            options: Default::default(),
        },
        fixed_resources,
        firmware_identity: config
            .serial_firmware_identity()
            .map(GuestSerialFirmwareIdentity::binding),
    })
}

fn serial_uses_host_console(request: &VirtualDeviceRequest, default: bool) -> AxVmResult<bool> {
    if !crate::machine::is_serial_model(&request.model) {
        return Ok(false);
    }
    let Some(backend) = request.options.get("backend") else {
        return Ok(default);
    };
    let backend = backend.as_table().ok_or_else(|| {
        AxVmError::invalid_config(std::format!(
            "virtual serial '{}' backend must be a table",
            request.id
        ))
    })?;
    let backend_type = backend
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            AxVmError::invalid_config(std::format!(
                "virtual serial '{}' backend requires string field 'type'",
                request.id
            ))
        })?;
    Ok(backend_type == "host-console")
}

fn configured_error(error: ConfiguredDeviceError) -> AxVmError {
    AxVmError::invalid_config(std::format!("{error}"))
}

#[cfg(test)]
mod tests {
    use std::vec;

    use super::*;
    use crate::config::{AxVMConfig, AxVMConfigParams, PhysCpuList};

    #[test]
    fn console_override_and_extra_serial_share_deterministic_planning() {
        let config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            virtual_device_requests: vec![
                VirtualDeviceRequest {
                    id: "serial1".into(),
                    model: "uart16550-mmio".into(),
                    options: Default::default(),
                },
                VirtualDeviceRequest {
                    id: "console0".into(),
                    model: "pl011-mmio".into(),
                    options: Default::default(),
                },
            ],
            ..Default::default()
        });
        let controller = DeviceNodeId::new("controller").unwrap();
        let mut nodes = vec![DeviceNodeSpec::firmware_only(controller.clone())];
        append_configured_devices(
            &config,
            &mut nodes,
            &controller,
            InterruptControllerId::new(0),
        )
        .unwrap();

        let mut graph = DeviceGraphBuilder::new();
        for node in nodes {
            graph.add(node).unwrap();
        }
        let mut pools = ResourcePools::new();
        pools.add_auto_mmio(0x1000_0000..0x1001_0000).unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(16)..ControllerInputId::new(32),
            )
            .unwrap();
        let graph = graph.declare().unwrap().resolve(pools).unwrap();
        let registers = ResourceSlot::new("registers").unwrap();
        let irq = ResourceSlot::new("irq").unwrap();

        let console = graph
            .resources_for(&DeviceNodeId::new("console0").unwrap())
            .unwrap();
        assert_eq!(console.mmio(&registers).unwrap(), (0x1000_0000, 0x1000));
        assert_eq!(console.wired_irq(&irq).unwrap().input().value(), 16);

        let serial = graph
            .resources_for(&DeviceNodeId::new("serial1").unwrap())
            .unwrap();
        assert_eq!(serial.mmio(&registers).unwrap(), (0x1000_1000, 0x100));
        assert_eq!(serial.wired_irq(&irq).unwrap().input().value(), 17);
    }
}
