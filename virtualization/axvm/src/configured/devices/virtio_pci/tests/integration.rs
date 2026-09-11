use axvirtio_common::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK, constants::VIRTIO_F_VERSION_1,
};

use super::*;

struct BlockEndpointContext {
    device_id: DeviceId,
    endpoint_device_id: DeviceId,
    expected_dma_grant: DmaGrant,
    memory: Arc<Mutex<Vec<u8>>>,
    guest_memory_calls: Arc<AtomicUsize>,
    active_dma_enabled: bool,
    guest_memory_pause: Option<Arc<TestPause>>,
    active_dma_grant: Option<DmaGrant>,
}

#[test]
fn serialized_virtio_block_config_passes_ovmf_modern_probe() {
    let (root, binding, bdf, _runtime) = build_bound_endpoint();
    let mut context = TestEndpointContext::new();
    let mut config = [0_u8; 0x100];
    let pci_cfg_offset = usize::from(
        root.topology()
            .function(&node("virtio-pci"))
            .expect("VirtIO PCI function is present")
            .capabilities()
            .nth(4)
            .expect("VirtIO PCI_CFG capability is present")
            .offset()
            .value(),
    );
    for (offset, byte) in config.iter_mut().enumerate() {
        if (pci_cfg_offset + 16..pci_cfg_offset + 20).contains(&offset) {
            // `pci_cfg_data` is an effect register, not a static image byte;
            // without a guest-selected target it has no readable value.  Its
            // capability header and selectors are validated below, while the
            // data window remains zero in this power-on probe image.
            continue;
        }
        *byte = binding
            .read_config_with_context(
                bdf,
                ConfigOffset::new(offset as u16).expect("conventional config offset is valid"),
                AccessWidth::Byte,
                &mut context,
            )
            .expect("serialized PCI config byte should be readable") as u8;
    }

    // These are the identity predicates used by OVMF's modern VirtIO PCI
    // binding.  The device type is derived from Device ID, not Subsystem ID.
    assert_eq!(u16::from_le_bytes([config[0], config[1]]), 0x1af4);
    let device_id = u16::from_le_bytes([config[2], config[3]]);
    assert!(
        (0x1040..=0x107f).contains(&device_id),
        "unexpected VirtIO modern device ID: {device_id:#06x}"
    );
    assert_eq!(device_id - 0x1040, 2, "device must be VirtIO Block");
    assert!(config[0x08] >= 1, "modern VirtIO requires revision >= 1");
    assert_eq!(u16::from_le_bytes([config[0x2c], config[0x2d]]), 0x1af4);
    assert_eq!(u16::from_le_bytes([config[0x2e], config[0x2f]]), 0x1042);
    assert_ne!(config[0x06] & 0x10, 0, "capability list must be advertised");

    let mut pointer = config[0x34] as usize;
    let mut visited = [false; 0x100];
    let mut capabilities = Vec::new();
    while pointer != 0 {
        assert!(pointer >= 0x40 && pointer.is_multiple_of(4));
        assert!(pointer + 2 <= config.len());
        assert!(!visited[pointer], "capability list contains a cycle");
        visited[pointer] = true;
        assert_eq!(
            config[pointer], 0x09,
            "VirtIO capabilities are vendor-specific"
        );

        let cap_len = config[pointer + 2] as usize;
        assert!(matches!(cap_len, 16 | 20));
        assert!(pointer + cap_len <= config.len());
        let cfg_type = config[pointer + 3];
        let bar = config[pointer + 4];
        let offset = u32::from_le_bytes(
            config[pointer + 8..pointer + 12]
                .try_into()
                .expect("VirtIO capability offset has four bytes"),
        );
        let length = u32::from_le_bytes(
            config[pointer + 12..pointer + 16]
                .try_into()
                .expect("VirtIO capability length has four bytes"),
        );
        let multiplier = if cap_len == 20 {
            u32::from_le_bytes(
                config[pointer + 16..pointer + 20]
                    .try_into()
                    .expect("VirtIO notify multiplier has four bytes"),
            )
        } else {
            0
        };
        capabilities.push((cfg_type, cap_len, bar, offset, length, multiplier));
        pointer = config[pointer + 1] as usize;
    }

    assert_eq!(
        capabilities,
        vec![
            (1, 16, 0, 0x000, 0x38, 0),
            (2, 20, 0, 0x100, 0x04, 4),
            (3, 16, 0, 0x200, 0x01, 0),
            (4, 16, 0, 0x300, 16, 0),
            (5, 20, 0, 0x000, 0, 0),
        ]
    );
}

const READ_DATA: usize = 0x4400;

impl BlockEndpointContext {
    fn new(size: usize, endpoint_device_id: DeviceId, expected_dma_grant: DmaGrant) -> Self {
        Self {
            device_id: DeviceId::new(0),
            endpoint_device_id,
            expected_dma_grant,
            memory: Arc::new(Mutex::new(vec![0; size])),
            guest_memory_calls: Arc::new(AtomicUsize::new(0)),
            active_dma_enabled: true,
            guest_memory_pause: None,
            active_dma_grant: None,
        }
    }

    fn nested(&self, device_id: DeviceId) -> Self {
        Self {
            device_id,
            endpoint_device_id: self.endpoint_device_id,
            expected_dma_grant: self.expected_dma_grant.clone(),
            memory: Arc::clone(&self.memory),
            guest_memory_calls: Arc::clone(&self.guest_memory_calls),
            active_dma_enabled: false,
            guest_memory_pause: self.guest_memory_pause.clone(),
            active_dma_grant: None,
        }
    }

    fn pause_next_guest_memory(&mut self, pause: Arc<TestPause>) {
        self.guest_memory_pause = Some(pause);
    }

    fn reset_context(&self) -> Self {
        Self {
            device_id: DeviceId::new(0),
            endpoint_device_id: self.endpoint_device_id,
            expected_dma_grant: self.expected_dma_grant.clone(),
            memory: Arc::clone(&self.memory),
            guest_memory_calls: Arc::clone(&self.guest_memory_calls),
            active_dma_enabled: true,
            guest_memory_pause: None,
            active_dma_grant: None,
        }
    }

    fn write_bytes(&self, address: usize, bytes: &[u8]) {
        self.memory.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }

    fn read_bytes(&self, address: usize, length: usize) -> Vec<u8> {
        self.memory.lock().unwrap()[address..address + length].to_vec()
    }

    fn set_descriptor(&self, index: usize, address: u64, length: u32, flags: u16, next: u16) {
        let offset = 0x1000 + index * 16;
        self.write_bytes(offset, &address.to_le_bytes());
        self.write_bytes(offset + 8, &length.to_le_bytes());
        self.write_bytes(offset + 12, &flags.to_le_bytes());
        self.write_bytes(offset + 14, &next.to_le_bytes());
    }

    fn set_available_head(&self, index: u16, head: u16) {
        self.write_bytes(0x2000 + 2, &index.to_le_bytes());
        self.write_bytes(
            0x2000 + 4 + (usize::from(index) - 1) * 2,
            &head.to_le_bytes(),
        );
    }

    fn set_header(&self, request_type: u32, sector: u64) {
        self.write_bytes(0x4000, &request_type.to_le_bytes());
        self.write_bytes(0x4008, &sector.to_le_bytes());
    }

    fn check_dma_grant(&mut self, grant: &DmaGrant) -> DeviceResult {
        if !self.active_dma_enabled {
            return Err(DeviceError::Unsupported {
                operation: "access guest memory from VirtIO PCI endpoint",
                detail: "guest-memory access was attempted while PCI bus mastering was disabled"
                    .into(),
            });
        }
        if self.device_id != self.endpoint_device_id {
            return Err(DeviceError::Unsupported {
                operation: "access guest memory from VirtIO PCI endpoint",
                detail: "guest-memory access escaped the routed endpoint context".into(),
            });
        }
        if !self.expected_dma_grant.same_token(grant) {
            return Err(DeviceError::Unsupported {
                operation: "access guest memory from VirtIO PCI endpoint",
                detail: "guest-memory access used an unregistered DMA grant".into(),
            });
        }
        if let Some(active) = &self.active_dma_grant {
            if !active.same_token(grant) {
                return Err(DeviceError::Unsupported {
                    operation: "access guest memory from VirtIO PCI endpoint",
                    detail: "guest-memory access used a different DMA grant".into(),
                });
            }
        } else {
            self.active_dma_grant = Some(grant.clone());
        }
        Ok(())
    }
}

impl DeviceContext for BlockEndpointContext {
    fn device_id(&self) -> DeviceId {
        self.device_id
    }

    fn with_routed_device(
        &mut self,
        grant: &axdevice_base::RoutedDeviceGrant,
        callback: &mut dyn FnMut(&mut dyn DeviceContext) -> DeviceResult,
    ) -> DeviceResult {
        if grant.device_id() != self.endpoint_device_id {
            return Err(DeviceError::Unsupported {
                operation: "enter VirtIO PCI endpoint context",
                detail: "routed grant names a different endpoint device".into(),
            });
        }
        if !grant.admission_is_open() {
            return Err(DeviceError::Unsupported {
                operation: "enter VirtIO PCI endpoint context",
                detail: "routed grant does not carry an open DMA capability".into(),
            });
        }
        let mut nested = self.nested(grant.device_id());
        nested.active_dma_enabled = grant.dma_enabled();
        let result = callback(&mut nested);
        self.active_dma_grant = nested.active_dma_grant;
        result
    }

    fn read_guest_memory(
        &mut self,
        grant: &DmaGrant,
        address: GuestPhysAddr,
        data: &mut [u8],
    ) -> DeviceResult {
        if let Some(pause) = self.guest_memory_pause.take() {
            pause.wait();
        }
        self.guest_memory_calls.fetch_add(1, Ordering::Relaxed);
        self.check_dma_grant(grant)?;
        let memory = self.memory.lock().unwrap();
        let start = address.as_usize();
        data.copy_from_slice(memory.get(start..start + data.len()).ok_or(
            DeviceError::OutOfRange {
                addr: address.as_usize() as u64,
            },
        )?);
        Ok(())
    }

    fn write_guest_memory(
        &mut self,
        grant: &DmaGrant,
        address: GuestPhysAddr,
        data: &[u8],
    ) -> DeviceResult {
        if let Some(pause) = self.guest_memory_pause.take() {
            pause.wait();
        }
        self.guest_memory_calls.fetch_add(1, Ordering::Relaxed);
        self.check_dma_grant(grant)?;
        let mut memory = self.memory.lock().unwrap();
        let start = address.as_usize();
        memory
            .get_mut(start..start + data.len())
            .ok_or(DeviceError::OutOfRange {
                addr: address.as_usize() as u64,
            })?
            .copy_from_slice(data);
        Ok(())
    }
}

#[test]
fn adapter_retries_completion_assert_after_irq_failure() {
    let (root, binding, bdf, _runtime, sink) = build_bound_endpoint_with_options(false);
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let mut context = TestEndpointContext::new();
    configure_running_endpoint(&root, &binding, bdf, bar.address(), &mut context);

    sink.fail_assert.store(true, Ordering::Relaxed);
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .expect_err("completion IRQ failure must be reported to the dispatcher");
    assert_eq!(sink.assert_calls.load(Ordering::Relaxed), 1);

    sink.fail_assert.store(false, Ordering::Relaxed);
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x406,
            &mut context,
        )
        .expect("disabling INTx should reconcile the failed Assert state");
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            6,
            &mut context,
        )
        .expect("reenabling INTx should retry the pending Assert");
    assert_eq!(sink.assert_calls.load(Ordering::Relaxed), 2);
}

#[test]
fn adapter_retries_fault_assert_before_status_reset() {
    let (root, binding, bdf, _runtime, sink) = build_bound_endpoint_with_options(true);
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let mut context = TestEndpointContext::new();
    configure_running_endpoint(&root, &binding, bdf, bar.address(), &mut context);

    sink.fail_assert.store(true, Ordering::Relaxed);
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .expect_err("queue fault should be reported after failed Assert publication");
    assert_eq!(sink.assert_calls.load(Ordering::Relaxed), 1);
    assert_ne!(
        binding
            .read_bar_with_context(bar.address() + 0x14, AccessWidth::Byte, &mut context)
            .expect("status read should succeed")
            & axvirtio_common::VIRTIO_STATUS_DEVICE_NEEDS_RESET as u64,
        0
    );

    sink.fail_assert.store(false, Ordering::Relaxed);
    binding
        .write_bar_with_context(bar.address() + 0x14, AccessWidth::Byte, 0, &mut context)
        .expect("status reset should deassert the resynchronized line");
    assert_eq!(
        binding
            .read_bar_with_context(bar.address() + 0x14, AccessWidth::Byte, &mut context)
            .expect("reset status read should succeed"),
        0
    );
    // The failed Assert never raised the wired source, so IrqLine's
    // idempotent Deassert needs no backend call. The successful status
    // reset still consumed the coordinator's forced Deassert transition.
    assert_eq!(sink.assert_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn adapter_records_command_deassert_failure_for_later_isr_retry() {
    let (root, binding, bdf, _runtime, sink) = build_bound_endpoint_with_options(false);
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let mut context = TestEndpointContext::new();
    configure_running_endpoint(&root, &binding, bdf, bar.address(), &mut context);
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .expect("completion should assert the line");

    sink.fail_deassert.store(true, Ordering::Relaxed);
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x406,
            &mut context,
        )
        .expect_err("command transition failure should reach the adapter");
    assert_eq!(sink.deassert_calls.load(Ordering::Relaxed), 1);

    sink.fail_deassert.store(false, Ordering::Relaxed);
    assert_eq!(
        binding
            .read_bar_with_context(bar.address() + 0x200, AccessWidth::Byte, &mut context)
            .expect("ISR read should retain its value"),
        1
    );
    assert_eq!(sink.deassert_calls.load(Ordering::Relaxed), 2);
}

#[test]
fn adapter_records_isr_deassert_failure_for_command_retry() {
    let (root, binding, bdf, _runtime, sink) = build_bound_endpoint_with_options(false);
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let mut context = TestEndpointContext::new();
    configure_running_endpoint(&root, &binding, bdf, bar.address(), &mut context);
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .expect("completion should assert the line");

    sink.fail_deassert.store(true, Ordering::Relaxed);
    assert_eq!(
        binding
            .read_bar_with_context(bar.address() + 0x200, AccessWidth::Byte, &mut context)
            .expect("ISR read value should not depend on line publication"),
        1
    );
    assert_eq!(sink.deassert_calls.load(Ordering::Relaxed), 1);

    sink.fail_deassert.store(false, Ordering::Relaxed);
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x406,
            &mut context,
        )
        .expect("command transition should retry ISR deassertion");
    assert_eq!(sink.deassert_calls.load(Ordering::Relaxed), 2);
}

#[test]
fn real_endpoint_serializes_command_revision_with_interrupt_state() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let armed = Arc::new(AtomicBool::new(false));
    let command_revision_hook = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let armed = Arc::clone(&armed);
        Arc::new(move || {
            if armed
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                entered.wait();
                release.wait();
            }
        })
    };
    let (root, binding, bdf, _runtime, sink) =
        build_bound_endpoint_with_command_hook(false, Some(command_revision_hook));
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let mut context = TestEndpointContext::new();
    configure_running_endpoint(&root, &binding, bdf, bar.address(), &mut context);
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .expect("completion should assert the line before the command race");
    let assert_calls_before = sink.assert_calls.load(Ordering::Relaxed);

    armed.store(true, Ordering::Release);
    let first_binding = Arc::clone(&binding);
    let first = thread::spawn(move || {
        let mut context = TestEndpointContext::new();
        first_binding.write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x406,
            &mut context,
        )
    });
    entered.wait();

    let mut second_context = TestEndpointContext::new();
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            6,
            &mut second_context,
        )
        .expect("newer command callback should complete while the old one is paused");
    release.wait();
    first
        .join()
        .expect("older command callback should finish")
        .expect("older command callback should not fail");

    assert_eq!(
        root.read_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word),
        Ok(6)
    );
    assert_eq!(
        sink.assert_calls.load(Ordering::Relaxed),
        assert_calls_before + 1,
        "the latest INTx-enable command must win after the older transition completes"
    );
}

#[test]
fn stale_command_transition_cannot_assert_after_status_reset() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let armed = Arc::new(AtomicBool::new(false));
    let command_revision_hook = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let armed = Arc::clone(&armed);
        Arc::new(move || {
            if armed
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                entered.wait();
                release.wait();
            }
        })
    };
    let (root, binding, bdf, _runtime, sink) =
        build_bound_endpoint_with_command_hook(false, Some(command_revision_hook));
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let mut context = TestEndpointContext::new();
    configure_running_endpoint(&root, &binding, bdf, bar.address(), &mut context);

    // Keep the ISR pending while INTx is disabled.  Re-enabling INTx below
    // creates the old Assert intent that will be paused before admission.
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x406,
            &mut context,
        )
        .expect("disabling INTx should succeed");
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .expect("completion should remain pending while INTx is disabled");
    let assert_calls_before = sink.assert_calls.load(Ordering::Relaxed);

    armed.store(true, Ordering::Release);
    let command_binding = Arc::clone(&binding);
    let command = thread::spawn(move || {
        let mut command_context = TestEndpointContext::new();
        command_binding.write_config_with_context(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            6,
            &mut command_context,
        )
    });
    entered.wait();

    // Complete a new VirtIO generation while the old Command callback is
    // paused after its logical update but before activity admission.
    let mut reset_context = TestEndpointContext::new();
    binding
        .write_bar_with_context(
            bar.address() + 0x14,
            AccessWidth::Byte,
            0,
            &mut reset_context,
        )
        .expect("status reset should complete");
    assert_eq!(
        binding
            .read_bar_with_context(bar.address() + 0x14, AccessWidth::Byte, &mut reset_context)
            .expect("reset status should be readable"),
        0
    );

    release.wait();
    command
        .join()
        .expect("paused command callback should finish")
        .expect("stale command transition should be suppressed without a line error");
    assert_eq!(
        sink.assert_calls.load(Ordering::Relaxed),
        assert_calls_before,
        "a stale pre-reset Command transition must not reassert INTx"
    );
}

#[test]
fn bound_virtio_endpoint_serializes_dispatches_and_relocates_bar() {
    let (root, binding, bdf, _runtime) = build_bound_endpoint();
    let function = root.topology().function(&node("virtio-pci")).unwrap();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let pci_cfg = function.capabilities().nth(4).unwrap();
    let capability_offset = u64::from(pci_cfg.offset().value());

    assert_eq!(
        root.read_config(bdf, ConfigOffset::new(0x40).unwrap(), AccessWidth::Byte),
        Ok(9)
    );
    assert_eq!(
        root.read_config(
            bdf,
            ConfigOffset::new(capability_offset as u16 + 2).unwrap(),
            AccessWidth::Byte,
        ),
        Ok(20)
    );
    assert_eq!(
        root.read_config(
            bdf,
            ConfigOffset::new(capability_offset as u16 + 3).unwrap(),
            AccessWidth::Byte,
        ),
        Ok(5)
    );

    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 2)
        .unwrap();
    assert_eq!(
        root.read_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word),
        Ok(2)
    );
    assert!(
        root.resolve_bar(bar.address() + 0x300, AccessWidth::Dword)
            .is_some()
    );
    let direct = binding
        .read_bar(bar.address() + 0x300, AccessWidth::Dword)
        .unwrap();

    root.write_config(
        bdf,
        ConfigOffset::new(capability_offset as u16 + 4).unwrap(),
        AccessWidth::Byte,
        0,
    )
    .unwrap();
    root.write_config(
        bdf,
        ConfigOffset::new(capability_offset as u16 + 8).unwrap(),
        AccessWidth::Dword,
        0x300,
    )
    .unwrap();
    root.write_config(
        bdf,
        ConfigOffset::new(capability_offset as u16 + 12).unwrap(),
        AccessWidth::Dword,
        4,
    )
    .unwrap();
    let through_pci_cfg = binding
        .read_config(
            bdf,
            ConfigOffset::new(capability_offset as u16 + 16).unwrap(),
            AccessWidth::Dword,
        )
        .unwrap();
    assert_eq!(through_pci_cfg, direct);

    let mut context = TestEndpointContext::new();
    let split_queue_address = 0x0000_0001_0000_1000_u64;
    for (target, value) in [
        (0x20, split_queue_address & u32::MAX as u64),
        (0x24, split_queue_address >> 32),
    ] {
        root.write_config(
            bdf,
            ConfigOffset::new(capability_offset as u16 + 8).unwrap(),
            AccessWidth::Dword,
            target,
        )
        .unwrap();
        binding
            .write_config_with_context(
                bdf,
                ConfigOffset::new(capability_offset as u16 + 16).unwrap(),
                AccessWidth::Dword,
                value,
                &mut context,
            )
            .unwrap();
    }
    assert_eq!(
        binding.read_bar(bar.address() + 0x20, AccessWidth::Qword),
        Ok(split_queue_address)
    );

    for status in [1, 3, 0x0b, 0x0f] {
        binding
            .write_bar_with_context(
                bar.address() + 0x14,
                AccessWidth::Byte,
                status,
                &mut context,
            )
            .unwrap();
    }
    for (offset, width, value) in [
        (0x20, AccessWidth::Qword, 0x1000),
        (0x28, AccessWidth::Qword, 0x2000),
        (0x30, AccessWidth::Qword, 0x3000),
    ] {
        binding
            .write_bar_with_context(bar.address() + offset, width, value, &mut context)
            .unwrap();
    }
    binding
        .write_bar_with_context(bar.address() + 0x1c, AccessWidth::Word, 1, &mut context)
        .unwrap();
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .unwrap();
    assert_eq!(context.reads.load(Ordering::Relaxed), 0);
    assert_eq!(context.writes.load(Ordering::Relaxed), 0);

    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 6)
        .unwrap();
    assert_eq!(
        root.read_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word),
        Ok(6)
    );

    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let race_context =
        TestEndpointContext::new().paused(Arc::clone(&entered), Arc::clone(&release));
    let race_binding = Arc::clone(&binding);
    let notify_address = bar.address() + 0x100;
    let race_thread = thread::spawn(move || {
        let mut race_context = race_context;
        race_binding.write_bar_with_context(notify_address, AccessWidth::Word, 0, &mut race_context)
    });
    entered.wait();
    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 2)
        .unwrap();
    release.wait();
    race_thread
        .join()
        .expect("BME race operation should finish")
        .unwrap();

    let reads_before_stopped_notify = context.reads.load(Ordering::Relaxed);
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .unwrap();
    assert_eq!(
        context.reads.load(Ordering::Relaxed),
        reads_before_stopped_notify
    );
    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 6)
        .unwrap();
    binding
        .write_bar_with_context(bar.address() + 0x100, AccessWidth::Word, 0, &mut context)
        .unwrap();
    assert!(context.reads.load(Ordering::Relaxed) >= 7);
    assert!(context.writes.load(Ordering::Relaxed) >= 1);
    assert_eq!(
        binding.read_bar(bar.address() + 0x200, AccessWidth::Byte),
        Ok(1)
    );

    root.write_config(
        bdf,
        ConfigOffset::new(capability_offset as u16 + 8).unwrap(),
        AccessWidth::Dword,
        0x100,
    )
    .unwrap();
    root.write_config(
        bdf,
        ConfigOffset::new(capability_offset as u16 + 12).unwrap(),
        AccessWidth::Dword,
        2,
    )
    .unwrap();
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new(capability_offset as u16 + 16).unwrap(),
            AccessWidth::Word,
            0,
            &mut context,
        )
        .unwrap();
    assert_eq!(
        binding.read_bar(bar.address() + 0x200, AccessWidth::Byte),
        Ok(1)
    );

    let relocated = APERTURE_BASE + 0x80000;
    root.write_config(
        bdf,
        ConfigOffset::new(0x10).unwrap(),
        AccessWidth::Dword,
        relocated,
    )
    .unwrap();
    assert_eq!(
        binding.read_bar(relocated + 0x300, AccessWidth::Dword),
        Ok(direct)
    );
    assert_eq!(
        binding.read_bar(bar.address() + 0x300, AccessWidth::Dword),
        Err(DeviceError::NotFound)
    );
}

#[test]
fn configured_virtio_blk_pci_runs_ramdisk_write_read_and_flush() {
    let config = axvmconfig::GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "virtio-blk"
model = "virtio-blk"
transport = "pci"
backend = "ramdisk"
capacity = "1MiB"
read_only = false
"#,
    )
    .unwrap();
    let context = crate::DeviceInstantiationContext::new()
        .with_default_wired_controller(node("controller"), INTX_CONTROLLER)
        .with_default_pci_host_key(host_key());
    let mut catalog = crate::ConfiguredDeviceCatalog::new();
    crate::machine::register_devices(&mut catalog).unwrap();
    let endpoint = catalog
        .instantiate_node(&config.devices.virtual_devices[0], &context)
        .unwrap();

    let root_slot = Arc::new(Mutex::new(None));
    let sink = Arc::new(TestIrqSink::new());
    let provider = PciHostProvider::new(
        host_key(),
        DeviceNodeSpec::virtual_device(
            node("pci-host"),
            Arc::new(HostModel {
                root: Arc::clone(&root_slot),
                sink: Arc::clone(&sink),
            }),
        ),
        slot("pci-memory"),
    )
    .with_intx_router(PciIntxRouter::new(
        INTX_CONTROLLER,
        [
            ControllerInputId::new(16),
            ControllerInputId::new(17),
            ControllerInputId::new(18),
            ControllerInputId::new(19),
        ],
        [16, 17, 18, 19],
        InterruptTrigger::LevelTriggered,
        InterruptSharing::Shared,
    ));
    let mut builder = DeviceGraphBuilder::new();
    builder
        .add(DeviceNodeSpec::firmware_only(node("controller")))
        .unwrap();
    builder.register_pci_host(provider).unwrap();
    builder.add(endpoint).unwrap();
    let mut pools = ResourcePools::new();
    pools
        .add_auto_mmio(APERTURE_BASE..APERTURE_BASE + APERTURE_SIZE)
        .unwrap();
    pools
        .allow_fixed_controller_inputs(
            INTX_CONTROLLER,
            ControllerInputId::new(16)..ControllerInputId::new(20),
        )
        .unwrap();
    let graph = builder.declare().unwrap().resolve(pools).unwrap();
    let mut runtime_builder = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
    for graph_node in graph.nodes() {
        runtime_builder
            .build_graph_node(graph_node, graph.resource_plan())
            .unwrap();
    }
    let runtime = Arc::new(runtime_builder.finish(graph.resource_plan()).unwrap());
    let root = root_slot.lock().unwrap().clone().unwrap();
    let binding = runtime
        .services()
        .all::<PciRootBindingKey>()
        .into_iter()
        .next()
        .unwrap();
    let function = graph
        .pci_topology(&host_key())
        .unwrap()
        .function(&node("virtio-blk"))
        .unwrap();
    assert_eq!(function.identity().vendor_id(), 0x1af4);
    assert_eq!(function.identity().device_id(), 0x1042);
    assert_eq!(function.identity().revision(), 1);
    assert_eq!(function.identity().subsystem_device_id(), 0x1042);
    let bdf = function.bdf();
    let bar = function.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let bar_address = bar.address();
    let pci_cfg = function.capabilities().nth(4).unwrap();
    let pci_cfg_offset = u64::from(pci_cfg.offset().value());
    let endpoint_device_id = DeviceId::new((runtime.device_count() - 1) as u32);

    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 6)
        .unwrap();
    let expected_dma_grant = runtime
        .dma_grant_for_test(endpoint_device_id)
        .expect("configured PCI endpoint must register its DMA grant");
    let mut endpoint_context =
        BlockEndpointContext::new(0x10_000, endpoint_device_id, expected_dma_grant);
    for (value, expected) in [
        (VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_ACKNOWLEDGE),
        (
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
        ),
    ] {
        binding
            .write_bar_with_context(
                bar_address + 0x14,
                AccessWidth::Byte,
                u64::from(value),
                &mut endpoint_context,
            )
            .unwrap();
        assert_eq!(
            binding
                .read_bar_with_context(
                    bar_address + 0x14,
                    AccessWidth::Byte,
                    &mut endpoint_context,
                )
                .unwrap(),
            u64::from(expected)
        );
    }
    for selector in [0_u64, 1] {
        binding
            .write_bar_with_context(
                bar_address,
                AccessWidth::Dword,
                selector,
                &mut endpoint_context,
            )
            .unwrap();
        let offered = binding
            .read_bar_with_context(
                bar_address + 0x04,
                AccessWidth::Dword,
                &mut endpoint_context,
            )
            .unwrap();
        if selector == 0 {
            assert_ne!(offered & (1 << 9), 0, "ramdisk must advertise FLUSH");
        } else {
            assert_ne!(offered & (VIRTIO_F_VERSION_1 >> 32), 0);
        }
        binding
            .write_bar_with_context(
                bar_address + 0x08,
                AccessWidth::Dword,
                selector,
                &mut endpoint_context,
            )
            .unwrap();
        binding
            .write_bar_with_context(
                bar_address + 0x0c,
                AccessWidth::Dword,
                offered,
                &mut endpoint_context,
            )
            .unwrap();
    }
    let features_ok = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
    binding
        .write_bar_with_context(
            bar_address + 0x14,
            AccessWidth::Byte,
            u64::from(features_ok),
            &mut endpoint_context,
        )
        .unwrap();
    assert_eq!(
        binding
            .read_bar_with_context(bar_address + 0x14, AccessWidth::Byte, &mut endpoint_context)
            .unwrap()
            & u64::from(VIRTIO_STATUS_FEATURES_OK),
        u64::from(VIRTIO_STATUS_FEATURES_OK)
    );
    for (offset, width, value) in [
        (
            0x14,
            AccessWidth::Byte,
            u64::from(features_ok | VIRTIO_STATUS_DRIVER_OK),
        ),
        (0x20, AccessWidth::Qword, 0x1000),
        (0x28, AccessWidth::Qword, 0x2000),
        (0x30, AccessWidth::Qword, 0x3000),
    ] {
        binding
            .write_bar_with_context(bar_address + offset, width, value, &mut endpoint_context)
            .unwrap();
    }
    binding
        .write_bar_with_context(
            bar_address + 0x1c,
            AccessWidth::Word,
            1,
            &mut endpoint_context,
        )
        .unwrap();
    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 2)
        .unwrap();
    let guest_memory_calls_before_bme_off =
        endpoint_context.guest_memory_calls.load(Ordering::Relaxed);
    binding
        .write_bar_with_context(
            bar_address + 0x100,
            AccessWidth::Word,
            0,
            &mut endpoint_context,
        )
        .expect("PCI transport must stop a notify before guest-memory access when BME is off");
    assert_eq!(
        endpoint_context.guest_memory_calls.load(Ordering::Relaxed),
        guest_memory_calls_before_bme_off,
        "BME-off notify must not enter the guest-memory context",
    );
    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 6)
        .unwrap();

    endpoint_context.set_descriptor(0, 0x4000, 16, 1, 1);
    endpoint_context.set_descriptor(1, 0x4100, 512, 1, 2);
    endpoint_context.set_descriptor(2, 0x4300, 1, 2, 0);
    endpoint_context.set_header(1, 2);
    endpoint_context.write_bytes(0x4100, &[0xa5; 512]);
    endpoint_context.set_available_head(1, 0);
    for (relative_offset, width, value) in [
        (4, AccessWidth::Byte, 0),
        (8, AccessWidth::Dword, 0x100),
        (12, AccessWidth::Dword, 2),
    ] {
        root.write_config(
            bdf,
            ConfigOffset::new((pci_cfg_offset + relative_offset) as u16).unwrap(),
            width,
            value,
        )
        .unwrap();
    }
    binding
        .write_config_with_context(
            bdf,
            ConfigOffset::new((pci_cfg_offset + 16) as u16).unwrap(),
            AccessWidth::Word,
            0,
            &mut endpoint_context,
        )
        .unwrap();
    assert_eq!(endpoint_context.read_bytes(0x4300, 1), vec![0]);
    assert_eq!(
        binding
            .read_bar_with_context(
                bar_address + 0x200,
                AccessWidth::Byte,
                &mut endpoint_context
            )
            .unwrap(),
        1
    );
    assert!(sink.assert_calls.load(Ordering::Relaxed) >= 1);

    endpoint_context.set_descriptor(1, READ_DATA as u64, 512, 3, 2);
    endpoint_context.write_bytes(READ_DATA, &[0; 512]);
    endpoint_context.set_header(0, 2);
    endpoint_context.set_available_head(2, 0);
    binding
        .write_bar_with_context(
            bar_address + 0x100,
            AccessWidth::Word,
            0,
            &mut endpoint_context,
        )
        .unwrap();
    assert_eq!(endpoint_context.read_bytes(0x4300, 1), vec![0]);
    assert_eq!(endpoint_context.read_bytes(READ_DATA, 512), vec![0xa5; 512]);
    assert_eq!(
        binding
            .read_bar_with_context(
                bar_address + 0x200,
                AccessWidth::Byte,
                &mut endpoint_context
            )
            .unwrap(),
        1
    );

    endpoint_context.set_descriptor(1, 0x4300, 1, 2, 0);
    endpoint_context.set_header(4, 0);
    endpoint_context.set_available_head(3, 0);
    binding
        .write_bar_with_context(
            bar_address + 0x100,
            AccessWidth::Word,
            0,
            &mut endpoint_context,
        )
        .unwrap();
    assert_eq!(endpoint_context.read_bytes(0x4300, 1), vec![0]);
    assert_eq!(
        binding
            .read_bar_with_context(
                bar_address + 0x200,
                AccessWidth::Byte,
                &mut endpoint_context
            )
            .unwrap(),
        1
    );

    // Exercise the real configured endpoint reset barrier. The first pause is
    // before the request can reach the ramdisk backend (A); the second is in
    // the wired INTx sink after used/status have been written (B). The
    // endpoint's VirtIO status reset must remain incomplete at both points.
    let guest_memory_pause = TestPause::new();
    let irq_pause = TestPause::new();
    sink.pause_next_assert(Arc::clone(&irq_pause));
    endpoint_context.pause_next_guest_memory(Arc::clone(&guest_memory_pause));
    endpoint_context.set_descriptor(0, 0x4000, 16, 1, 1);
    endpoint_context.set_descriptor(1, 0x4100, 512, 1, 2);
    endpoint_context.set_descriptor(2, 0x4300, 1, 2, 0);
    endpoint_context.set_header(1, 0);
    endpoint_context.write_bytes(0x4100, &[0x3c; 512]);
    endpoint_context.set_available_head(4, 0);
    let endpoint_memory = Arc::clone(&endpoint_context.memory);
    let mut reset_context = endpoint_context.reset_context();
    let notify_binding = Arc::clone(&binding);
    let notify = thread::spawn(move || {
        let mut endpoint_context = endpoint_context;
        notify_binding.write_bar_with_context(
            bar_address + 0x100,
            AccessWidth::Word,
            0,
            &mut endpoint_context,
        )
    });
    guest_memory_pause.entered.wait();

    let reset_started = Arc::new(Barrier::new(2));
    let reset_finished = Arc::new(AtomicBool::new(false));
    let reset_binding = Arc::clone(&binding);
    let reset_started_thread = Arc::clone(&reset_started);
    let reset_finished_thread = Arc::clone(&reset_finished);
    let reset = thread::spawn(move || {
        reset_started_thread.wait();
        let result = reset_binding.write_bar_with_context(
            bar_address + 0x14,
            AccessWidth::Byte,
            0,
            &mut reset_context,
        );
        reset_finished_thread.store(true, Ordering::Release);
        result
    });
    reset_started.wait();
    assert!(!reset_finished.load(Ordering::Acquire));

    guest_memory_pause.release.wait();
    irq_pause.entered.wait();
    assert!(!reset_finished.load(Ordering::Acquire));
    let endpoint_memory = endpoint_memory.lock().unwrap();
    assert_eq!(endpoint_memory[0x4300], 0);
    assert_eq!(
        u16::from_le_bytes(endpoint_memory[0x2002..0x2004].try_into().unwrap()),
        4
    );
    drop(endpoint_memory);

    irq_pause.release.wait();
    notify
        .join()
        .expect("configured endpoint notify should finish")
        .unwrap();
    reset
        .join()
        .expect("configured endpoint reset should finish")
        .unwrap();
    assert!(reset_finished.load(Ordering::Acquire));
    assert_eq!(
        binding
            .read_bar_with_context(
                bar_address + 0x14,
                AccessWidth::Byte,
                &mut BlockEndpointContext::new(
                    0x10_000,
                    endpoint_device_id,
                    runtime
                        .dma_grant_for_test(endpoint_device_id)
                        .expect("reset must retain the registered endpoint grant"),
                ),
            )
            .unwrap(),
        0
    );
}
