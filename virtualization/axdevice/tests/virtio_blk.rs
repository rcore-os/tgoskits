use std::{ops::Range, sync::Arc};

use axdevice::{
    AccessWidth, AxVmDeviceConfig, AxVmDevices, BaseDeviceOps, DeviceManagerError,
    DeviceManagerResult, GuestPhysAddr, MemoryBlockBackend, VirtioBlock,
};
use axdevice_base::DeviceResult;
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

const MMIO_BASE: usize = 0x1_0000;
const DESCRIPTOR_AREA: usize = 0x2_0000;
const DRIVER_AREA: usize = 0x2_2000;
const DEVICE_AREA: usize = 0x2_4000;
const HEADER_AREA: usize = 0x2_6000;
const DATA_AREA: usize = 0x2_7000;
const STATUS_AREA: usize = 0x2_9000;
const GUEST_MEMORY_BASE: usize = DESCRIPTOR_AREA;
const GUEST_MEMORY_SIZE: usize = 0x1_0000;
const SECTOR_SIZE: usize = 512;

const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;

const VIRTIO_MMIO_QUEUE_NUM: usize = 0x038;
const VIRTIO_MMIO_QUEUE_READY: usize = 0x044;
const VIRTIO_MMIO_QUEUE_NOTIFY: usize = 0x050;
const VIRTIO_MMIO_INTERRUPT_STATUS: usize = 0x060;
const VIRTIO_MMIO_STATUS: usize = 0x070;
const VIRTIO_MMIO_QUEUE_DESC_LOW: usize = 0x080;
const VIRTIO_MMIO_QUEUE_DESC_HIGH: usize = 0x084;
const VIRTIO_MMIO_QUEUE_DRIVER_LOW: usize = 0x090;
const VIRTIO_MMIO_QUEUE_DRIVER_HIGH: usize = 0x094;
const VIRTIO_MMIO_QUEUE_DEVICE_LOW: usize = 0x0a0;
const VIRTIO_MMIO_QUEUE_DEVICE_HIGH: usize = 0x0a4;
const VIRTIO_MMIO_CONFIG: usize = 0x100;

struct GuestMemory {
    bytes: std::sync::Mutex<Vec<u8>>,
}

impl GuestMemory {
    fn new() -> Self {
        Self {
            bytes: std::sync::Mutex::new(vec![0; GUEST_MEMORY_SIZE]),
        }
    }

    fn read(&self, address: GuestPhysAddr, buffer: &mut [u8]) -> DeviceManagerResult {
        let range = self.memory_range(address.as_usize(), buffer.len())?;
        buffer.copy_from_slice(&self.bytes.lock().unwrap()[range]);
        Ok(())
    }

    fn write(&self, address: GuestPhysAddr, buffer: &[u8]) -> DeviceManagerResult {
        let range = self.memory_range(address.as_usize(), buffer.len())?;
        self.bytes.lock().unwrap()[range].copy_from_slice(buffer);
        Ok(())
    }

    fn store(&self, address: usize, bytes: &[u8]) {
        let range = self.memory_range(address, bytes.len()).unwrap();
        self.bytes.lock().unwrap()[range].copy_from_slice(bytes);
    }

    fn load(&self, address: usize, length: usize) -> Vec<u8> {
        let range = self.memory_range(address, length).unwrap();
        self.bytes.lock().unwrap()[range].to_vec()
    }

    fn memory_range(&self, address: usize, length: usize) -> DeviceManagerResult<Range<usize>> {
        let offset = address.checked_sub(GUEST_MEMORY_BASE).ok_or_else(|| {
            DeviceManagerError::InvalidInput {
                operation: "access test guest memory",
                detail: format!("address {address:#x} is below guest memory"),
            }
        })?;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= GUEST_MEMORY_SIZE)
            .ok_or_else(|| DeviceManagerError::InvalidInput {
                operation: "access test guest memory",
                detail: format!("range {address:#x}+{length:#x} is outside guest memory"),
            })?;
        Ok(offset..end)
    }
}

#[test]
fn reads_one_sector_and_completes_the_used_ring() {
    let mut image = vec![0; 2 * SECTOR_SIZE];
    image[SECTOR_SIZE..].fill(0xa5);
    let (device, _) = new_device(image);
    let memory = GuestMemory::new();
    configure_queue(&device);
    post_request(&memory, VIRTIO_BLK_T_IN, 1, true, SECTOR_SIZE);

    assert!(device.is_queue_notify(VIRTIO_MMIO_QUEUE_NOTIFY, 0));
    assert_eq!(process_queue(&device, &memory).unwrap(), 1);
    assert_eq!(memory.load(DATA_AREA, SECTOR_SIZE), vec![0xa5; SECTOR_SIZE]);
    assert_eq!(memory.load(STATUS_AREA, 1), vec![VIRTIO_BLK_S_OK]);
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 1);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 4), 0);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 8), (SECTOR_SIZE + 1) as u32);
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_INTERRUPT_STATUS), 1);
}

#[test]
fn writes_one_sector_to_the_volatile_backing() {
    let (device, backend) = new_device(vec![0; 2 * SECTOR_SIZE]);
    let memory = GuestMemory::new();
    configure_queue(&device);
    memory.store(DATA_AREA, &vec![0x3c; SECTOR_SIZE]);
    post_request(&memory, VIRTIO_BLK_T_OUT, 1, false, SECTOR_SIZE);

    assert_eq!(process_queue(&device, &memory).unwrap(), 1);

    let mut stored = vec![0; SECTOR_SIZE];
    backend.read(SECTOR_SIZE, &mut stored).unwrap();
    assert_eq!(stored, vec![0x3c; SECTOR_SIZE]);
    assert_eq!(memory.load(STATUS_AREA, 1), vec![VIRTIO_BLK_S_OK]);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 8), 1);
}

#[test]
fn snapshot_captures_final_backing_state_as_owned_bytes() {
    let backend = MemoryBlockBackend::new(vec![0; 2 * SECTOR_SIZE]).unwrap();
    backend
        .write(SECTOR_SIZE, &vec![0x3c; SECTOR_SIZE])
        .unwrap();

    let snapshot = backend.snapshot();
    backend
        .write(SECTOR_SIZE, &vec![0xa5; SECTOR_SIZE])
        .unwrap();

    assert_eq!(&snapshot[SECTOR_SIZE..], &vec![0x3c; SECTOR_SIZE]);
    let mut current = vec![0; SECTOR_SIZE];
    backend.read(SECTOR_SIZE, &mut current).unwrap();
    assert_eq!(current, vec![0xa5; SECTOR_SIZE]);
}

#[test]
fn out_of_range_request_returns_io_error_without_touching_data() {
    let (device, _) = new_device(vec![0; SECTOR_SIZE]);
    let memory = GuestMemory::new();
    configure_queue(&device);
    memory.store(DATA_AREA, &vec![0xcc; SECTOR_SIZE]);
    post_request(&memory, VIRTIO_BLK_T_IN, 1, true, SECTOR_SIZE);

    assert_eq!(process_queue(&device, &memory).unwrap(), 1);
    assert_eq!(memory.load(DATA_AREA, SECTOR_SIZE), vec![0xcc; SECTOR_SIZE]);
    assert_eq!(memory.load(STATUS_AREA, 1), vec![VIRTIO_BLK_S_IOERR]);
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 1);
}

#[test]
fn wrong_data_direction_is_rejected_without_consuming_request() {
    let (device, _) = new_device(vec![0; SECTOR_SIZE]);
    let memory = GuestMemory::new();
    configure_queue(&device);
    post_request(&memory, VIRTIO_BLK_T_IN, 0, false, SECTOR_SIZE);

    assert!(matches!(
        process_queue(&device, &memory),
        Err(DeviceManagerError::InvalidInput { .. })
    ));
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 0);
}

#[test]
fn reset_clears_interrupt_and_queue_state() {
    let (device, _) = new_device(vec![0; SECTOR_SIZE]);
    let memory = GuestMemory::new();
    configure_queue(&device);
    post_request(&memory, VIRTIO_BLK_T_IN, 0, true, SECTOR_SIZE);
    process_queue(&device, &memory).unwrap();
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_INTERRUPT_STATUS), 1);

    mmio_write(&device, VIRTIO_MMIO_STATUS, 0).unwrap();

    assert_eq!(mmio_read(&device, VIRTIO_MMIO_INTERRUPT_STATUS), 0);
    assert_eq!(process_queue(&device, &memory).unwrap(), 0);
}

#[test]
fn reports_capacity_in_512_byte_sectors() {
    let (device, _) = new_device(vec![0; 7 * SECTOR_SIZE]);

    assert_eq!(mmio_read(&device, VIRTIO_MMIO_CONFIG), 7);
    assert_eq!(
        device
            .handle_read(
                GuestPhysAddr::from_usize(MMIO_BASE + VIRTIO_MMIO_CONFIG),
                AccessWidth::Qword,
            )
            .unwrap(),
        7
    );
}

#[test]
fn rejects_unaligned_backing_images() {
    assert!(matches!(
        MemoryBlockBackend::new(vec![0; SECTOR_SIZE - 1]),
        Err(DeviceManagerError::InvalidInput { .. })
    ));
}

#[test]
fn device_config_requires_the_selected_backing() {
    let config = AxVmDeviceConfig::new(vec![virtio_block_config(vec![1])]);
    let backing = Arc::new(MemoryBlockBackend::new(vec![0; SECTOR_SIZE]).unwrap());

    assert!(matches!(
        AxVmDevices::new_with_block_backings(config, &[backing]),
        Err(DeviceManagerError::InvalidConfig { .. })
    ));
}

#[test]
fn device_config_registers_a_memory_backed_block_device() {
    let config = AxVmDeviceConfig::new(vec![virtio_block_config(vec![0])]);
    let backing = Arc::new(MemoryBlockBackend::new(vec![0; SECTOR_SIZE]).unwrap());

    let devices = AxVmDevices::new_with_block_backings(config, &[backing]).unwrap();

    assert_eq!(devices.virtio_blocks().len(), 1);
    assert!(
        devices
            .virtio_block_for_addr(GuestPhysAddr::from_usize(MMIO_BASE))
            .is_some()
    );
}

#[test]
fn virtio_block_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<VirtioBlock>();
}

fn new_device(image: Vec<u8>) -> (VirtioBlock, Arc<MemoryBlockBackend>) {
    let backend = Arc::new(MemoryBlockBackend::new(image).unwrap());
    let device = VirtioBlock::new(GuestPhysAddr::from_usize(MMIO_BASE), 48, backend.clone());
    (device, backend)
}

fn configure_queue(device: &VirtioBlock) {
    mmio_write(device, VIRTIO_MMIO_QUEUE_NUM, 8).unwrap();
    write_address(
        device,
        VIRTIO_MMIO_QUEUE_DESC_LOW,
        VIRTIO_MMIO_QUEUE_DESC_HIGH,
        DESCRIPTOR_AREA,
    );
    write_address(
        device,
        VIRTIO_MMIO_QUEUE_DRIVER_LOW,
        VIRTIO_MMIO_QUEUE_DRIVER_HIGH,
        DRIVER_AREA,
    );
    write_address(
        device,
        VIRTIO_MMIO_QUEUE_DEVICE_LOW,
        VIRTIO_MMIO_QUEUE_DEVICE_HIGH,
        DEVICE_AREA,
    );
    mmio_write(device, VIRTIO_MMIO_QUEUE_READY, 1).unwrap();
}

fn post_request(
    memory: &GuestMemory,
    request_type: u32,
    sector: u64,
    data_writable: bool,
    data_length: usize,
) {
    let mut header = [0_u8; 16];
    header[0..4].copy_from_slice(&request_type.to_le_bytes());
    header[8..16].copy_from_slice(&sector.to_le_bytes());
    memory.store(HEADER_AREA, &header);
    memory.store(STATUS_AREA, &[0xff]);

    write_descriptor(memory, 0, HEADER_AREA, header.len(), VRING_DESC_F_NEXT, 1);
    let data_flags = VRING_DESC_F_NEXT | if data_writable { VRING_DESC_F_WRITE } else { 0 };
    write_descriptor(memory, 1, DATA_AREA, data_length, data_flags, 2);
    write_descriptor(memory, 2, STATUS_AREA, 1, VRING_DESC_F_WRITE, 0);
    memory.store(DRIVER_AREA + 2, &1u16.to_le_bytes());
    memory.store(DRIVER_AREA + 4, &0u16.to_le_bytes());
}

fn write_descriptor(
    memory: &GuestMemory,
    index: usize,
    address: usize,
    length: usize,
    flags: u16,
    next: u16,
) {
    let descriptor = DESCRIPTOR_AREA + index * 16;
    memory.store(descriptor, &(address as u64).to_le_bytes());
    memory.store(descriptor + 8, &(length as u32).to_le_bytes());
    memory.store(descriptor + 12, &flags.to_le_bytes());
    memory.store(descriptor + 14, &next.to_le_bytes());
}

fn process_queue(device: &VirtioBlock, memory: &GuestMemory) -> DeviceManagerResult<usize> {
    device.process_queue(
        &|address, buffer| memory.read(address, buffer),
        &|address, buffer| memory.write(address, buffer),
    )
}

fn write_address(device: &VirtioBlock, low: usize, high: usize, address: usize) {
    mmio_write(device, low, address as u32).unwrap();
    mmio_write(device, high, (address as u64 >> 32) as u32).unwrap();
}

fn mmio_read(device: &VirtioBlock, offset: usize) -> usize {
    device
        .handle_read(
            GuestPhysAddr::from_usize(MMIO_BASE + offset),
            AccessWidth::Dword,
        )
        .unwrap()
}

fn mmio_write(device: &VirtioBlock, offset: usize, value: u32) -> DeviceResult {
    device.handle_write(
        GuestPhysAddr::from_usize(MMIO_BASE + offset),
        AccessWidth::Dword,
        value as usize,
    )
}

fn load_u16(memory: &GuestMemory, address: usize) -> u16 {
    u16::from_le_bytes(memory.load(address, 2).try_into().unwrap())
}

fn load_u32(memory: &GuestMemory, address: usize) -> u32 {
    u32::from_le_bytes(memory.load(address, 4).try_into().unwrap())
}

fn virtio_block_config(cfg_list: Vec<usize>) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: "virtio-blk".into(),
        base_gpa: MMIO_BASE,
        length: 0x1000,
        irq_id: 48,
        emu_type: EmulatedDeviceType::VirtioBlk,
        cfg_list,
    }
}
