use core::cell::Cell;

use super::*;

#[test]
fn dma_read_uses_bounded_chunks_for_large_guest_length() {
    let writes = Cell::new(0usize);
    dma_read_entry(
        b"abc",
        0,
        FW_CFG_DMA_SCRATCH_SIZE * 2 + 17,
        GuestPhysAddr::from_usize(0x8000),
        &mut |_addr, buffer| {
            assert!(buffer.len() <= FW_CFG_DMA_SCRATCH_SIZE);
            writes.set(writes.get() + 1);
            Ok(())
        },
    )
    .unwrap();

    assert!(writes.get() > 1);
}

#[test]
fn dma_write_discard_uses_bounded_chunks_for_large_guest_length() {
    let reads = Cell::new(0usize);
    dma_discard_guest_write(
        FW_CFG_DMA_SCRATCH_SIZE * 2 + 17,
        GuestPhysAddr::from_usize(0x8000),
        &mut |_addr, buffer| {
            assert!(buffer.len() <= FW_CFG_DMA_SCRATCH_SIZE);
            buffer.fill(0xaa);
            reads.set(reads.get() + 1);
            Ok(())
        },
    )
    .unwrap();

    assert!(reads.get() > 1);
}

#[test]
fn dma_rejects_buffer_address_overflow() {
    assert!(validate_dma_buffer(GuestPhysAddr::from_usize(usize::MAX), 2).is_err());
}

#[cfg(feature = "host-test")]
struct TestGuestMemory {
    bytes: Vec<u8>,
}

#[cfg(feature = "host-test")]
impl DeviceAccess for TestGuestMemory {
    fn device_id(&self) -> axdevice_base::DeviceId {
        axdevice_base::DeviceId::new(0)
    }

    fn read_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        addr: GuestPhysAddr,
        data: &mut [u8],
    ) -> DeviceResult {
        let start = addr.as_usize();
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or(axdevice_base::DeviceError::OutOfRange { addr: start as u64 })?;
        data.copy_from_slice(&self.bytes[start..end]);
        Ok(())
    }

    fn write_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        addr: GuestPhysAddr,
        data: &[u8],
    ) -> DeviceResult {
        let start = addr.as_usize();
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or(axdevice_base::DeviceError::OutOfRange { addr: start as u64 })?;
        self.bytes[start..end].copy_from_slice(data);
        Ok(())
    }
}

#[cfg(feature = "host-test")]
#[test]
fn dma_descriptor_uses_the_runtime_granted_memory_port() {
    const BASE: usize = 0x1000;
    const DESCRIPTOR: usize = 0x80;
    const BUFFER: usize = 0x100;
    let bundle = FwCfgDeviceFactory::new()
        .build(FwCfgBuildConfig {
            base: GuestPhysAddr::from_usize(BASE),
            size: 0x20,
            kernel: FwCfgKernelPayload::unsplit(Arc::from(&b"kernel"[..])),
            initrd: None,
            cmdline: None,
            cpu_num: 1,
            platform: FwCfgPlatformConfig::default(),
        })
        .unwrap();
    let mut runtime = crate::DeviceRuntime::empty();
    runtime.register_bundle(bundle).unwrap();
    let mut memory = TestGuestMemory {
        bytes: alloc::vec![0; 0x200],
    };
    let control = FW_CFG_DMA_CTL_SELECT | FW_CFG_DMA_CTL_READ;
    memory.bytes[DESCRIPTOR..DESCRIPTOR + 4].copy_from_slice(&control.to_be_bytes());
    memory.bytes[DESCRIPTOR + 4..DESCRIPTOR + 8].copy_from_slice(&4u32.to_be_bytes());
    memory.bytes[DESCRIPTOR + 8..DESCRIPTOR + 16].copy_from_slice(&(BUFFER as u64).to_be_bytes());

    runtime
        .handle_mmio_write_with_memory(
            GuestPhysAddr::from_usize(BASE + FW_CFG_DMA_OFFSET),
            AccessWidth::Qword,
            (DESCRIPTOR as u64).swap_bytes() as usize,
            &mut memory,
        )
        .unwrap();

    assert_eq!(&memory.bytes[BUFFER..BUFFER + 4], b"QEMU");
    assert_eq!(&memory.bytes[DESCRIPTOR..DESCRIPTOR + 4], &[0, 0, 0, 0]);
}

#[cfg(feature = "host-test")]
#[test]
fn pio_selector_data_and_dma_share_one_fw_cfg_state() {
    const DESCRIPTOR: usize = 0x80;
    const BUFFER: usize = 0x100;
    let bundle = FwCfgDeviceFactory::new()
        .build_pio(
            0x510,
            2,
            0x514,
            8,
            FwCfgBuildConfig {
                base: GuestPhysAddr::from_usize(0x510),
                size: 0x0c,
                kernel: FwCfgKernelPayload::split(
                    Arc::from(&b"setup"[..]),
                    Arc::from(&b"kernel"[..]),
                ),
                initrd: None,
                cmdline: None,
                cpu_num: 1,
                platform: FwCfgPlatformConfig::default(),
            },
        )
        .unwrap();
    let mut runtime = crate::DeviceRuntime::empty();
    runtime.register_bundle(bundle).unwrap();

    runtime
        .handle_port_write(
            axdevice_base::Port::new(0x510),
            AccessWidth::Word,
            FW_CFG_SIGNATURE.swap_bytes() as usize,
        )
        .unwrap();
    let signature = (0..4)
        .map(|_| {
            runtime
                .handle_port_read(axdevice_base::Port::new(0x511), AccessWidth::Byte)
                .unwrap() as u8
        })
        .collect::<Vec<_>>();
    assert_eq!(signature, b"QEMU");

    runtime
        .handle_port_write(
            axdevice_base::Port::new(0x510),
            AccessWidth::Word,
            FW_CFG_ID as usize,
        )
        .unwrap();
    let version = (0..4).fold(0usize, |version, shift| {
        version
            | runtime
                .handle_port_read(axdevice_base::Port::new(0x511), AccessWidth::Byte)
                .unwrap()
                << (shift * 8)
    });
    assert_eq!(version, (FW_CFG_VERSION | FW_CFG_VERSION_DMA) as usize);

    runtime
        .handle_port_write(
            axdevice_base::Port::new(0x510),
            AccessWidth::Word,
            FW_CFG_KERNEL_SETUP_SIZE as usize,
        )
        .unwrap();
    let setup_size = (0..4).fold(0usize, |size, shift| {
        size | runtime
            .handle_port_read(axdevice_base::Port::new(0x511), AccessWidth::Byte)
            .unwrap()
            << (shift * 8)
    });
    assert_eq!(setup_size, b"setup".len());

    runtime
        .handle_port_write(
            axdevice_base::Port::new(0x510),
            AccessWidth::Word,
            FW_CFG_KERNEL_SETUP_DATA as usize,
        )
        .unwrap();
    let setup = (0..setup_size)
        .map(|_| {
            runtime
                .handle_port_read(axdevice_base::Port::new(0x511), AccessWidth::Byte)
                .unwrap() as u8
        })
        .collect::<Vec<_>>();
    assert_eq!(setup, b"setup");

    let mut memory = TestGuestMemory {
        bytes: alloc::vec![0; 0x200],
    };
    let control = FW_CFG_DMA_CTL_SELECT | FW_CFG_DMA_CTL_READ;
    memory.bytes[DESCRIPTOR..DESCRIPTOR + 4].copy_from_slice(&control.to_be_bytes());
    memory.bytes[DESCRIPTOR + 4..DESCRIPTOR + 8].copy_from_slice(&4u32.to_be_bytes());
    memory.bytes[DESCRIPTOR + 8..DESCRIPTOR + 16].copy_from_slice(&(BUFFER as u64).to_be_bytes());
    runtime
        .handle_port_write_with_memory(
            axdevice_base::Port::new(0x514),
            AccessWidth::Qword,
            (DESCRIPTOR as u64).swap_bytes() as usize,
            &mut memory,
        )
        .unwrap();
    assert_eq!(&memory.bytes[BUFFER..BUFFER + 4], b"QEMU");
}
