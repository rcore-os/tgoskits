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
        .with_vm_id(config.id())
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
    use std::{sync::Arc, vec};

    use axdevice_base::{IrqResult, VirtualInterruptController, WiredIrqInput, WiredIrqSink};

    use super::*;
    use crate::config::{AxVMConfig, AxVMConfigParams, PhysCpuList};

    struct TestInterruptController;
    struct TestInterruptSink;

    impl WiredIrqSink for TestInterruptSink {
        fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
            Ok(())
        }

        fn pulse(&self, _input: ControllerInputId) -> IrqResult {
            Ok(())
        }
    }

    impl VirtualInterruptController for TestInterruptController {
        fn id(&self) -> InterruptControllerId {
            InterruptControllerId::new(0)
        }

        fn wired_input(
            &self,
            input: ControllerInputId,
            trigger: InterruptTrigger,
        ) -> IrqResult<WiredIrqInput> {
            Ok(WiredIrqInput::new(
                self.id(),
                input,
                trigger,
                Arc::new(TestInterruptSink),
            ))
        }
    }

    struct TestInterruptControllerModel;

    impl DeviceModel for TestInterruptControllerModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            Ok(DeviceRequirements::new())
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            let controller: Arc<dyn VirtualInterruptController> = Arc::new(TestInterruptController);
            Ok(DeviceBundle::from_registration(
                DeviceRegistration::InterruptController(ControllerRegistration::new(
                    InterruptControllerId::new(0),
                    controller,
                )),
            ))
        }
    }

    fn test_interrupt_controller_node(id: DeviceNodeId) -> DeviceNodeSpec {
        DeviceNodeSpec::virtual_device(id, Arc::new(TestInterruptControllerModel))
    }

    fn registered_catalog() -> Arc<ConfiguredDeviceCatalog> {
        let mut catalog = ConfiguredDeviceCatalog::new();
        crate::machine::register_devices(&mut catalog).unwrap();
        Arc::new(catalog)
    }

    #[test]
    fn console_override_and_extra_serial_share_deterministic_planning() {
        let config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            virtual_device_catalog: registered_catalog(),
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

    #[test]
    fn ivc_channel_uses_resolved_notify_irq_and_planned_mmio_aperture() {
        let config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            virtual_device_catalog: registered_catalog(),
            virtual_device_requests: vec![VirtualDeviceRequest {
                id: "ivc0".into(),
                model: "ivc-channel".into(),
                options: Default::default(),
            }],
            ..Default::default()
        });
        let controller = DeviceNodeId::new("controller").unwrap();
        let mut nodes = vec![test_interrupt_controller_node(controller.clone())];
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
        pools
            .add_auto_mmio(0x1000_0000..0x1000_0000 + super::devices::IVC_CHANNEL_SHARED_RANGE_SIZE)
            .unwrap();
        pools.allow_fixed_pio(0x3f8..0x400).unwrap();
        pools
            .allow_fixed_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(4)..ControllerInputId::new(5),
            )
            .unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(32)..ControllerInputId::new(36),
            )
            .unwrap();
        let graph = graph.declare().unwrap().resolve(pools).unwrap();

        let registers = ResourceSlot::new("registers").unwrap();
        let notify = ResourceSlot::new("notify").unwrap();
        let ivc = graph
            .resources_for(&DeviceNodeId::new("ivc0").unwrap())
            .unwrap();
        assert_eq!(
            ivc.mmio(&registers).unwrap(),
            (0x1000_0000, super::devices::IVC_CHANNEL_SHARED_RANGE_SIZE,)
        );
        assert_eq!(ivc.wired_irq(&notify).unwrap().input().value(), 32);

        let mut runtime = DeviceRuntimeBuilder::new(Default::default());
        for node in graph.nodes() {
            runtime
                .build_graph_node(node, graph.resource_plan())
                .unwrap();
        }
        let runtime = runtime.finish(graph.resource_plan()).unwrap();
        assert_eq!(
            crate::runtime::ivc::alloc_guest_binding(&runtime, 0x1000).unwrap(),
            GuestPhysAddr::from_usize(0x1000_0000)
        );
    }
}
