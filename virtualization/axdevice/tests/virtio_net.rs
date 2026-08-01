use std::{
    ops::Range,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axdevice::{
    AccessWidth, AxVmDeviceConfig, AxVmDevices, BaseDeviceOps, DeviceManagerError,
    DeviceManagerResult, GuestPhysAddr, VirtioNet, VirtioNetHeaderMode, VirtioNetOptions,
};
use axdevice_base::{DeviceError, DeviceResult};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

const MMIO_BASE: usize = 0x1_0000;
const DESCRIPTOR_AREA: usize = 0x2_0000;
const DRIVER_AREA: usize = 0x2_2000;
const DEVICE_AREA: usize = 0x2_4000;
const BUFFER_AREA: usize = 0x2_6000;
const GUEST_MEMORY_BASE: usize = DESCRIPTOR_AREA;
const GUEST_MEMORY_SIZE: usize = 0x1_0000;

const RX_QUEUE: u32 = 0;
const TX_QUEUE: u32 = 1;
const QUEUE_NUM_MAX: u32 = 256;
const VIRTIO_NET_BASE_HEADER_LEN: usize = 10;
const VIRTIO_NET_HEADER_LEN: usize = 12;
const MAX_ETHERNET_FRAME_LEN: usize = 1514;

const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

const VIRTIO_NET_F_MRG_RXBUF: u32 = 1 << 15;
const VIRTIO_F_VERSION_1_WORD_1: u32 = 1;
const VIRTIO_MMIO_DEVICE_FEATURES: usize = 0x010;
const VIRTIO_MMIO_DRIVER_FEATURES: usize = 0x020;
const VIRTIO_MMIO_DRIVER_FEATURES_SEL: usize = 0x024;
const VIRTIO_MMIO_QUEUE_SEL: usize = 0x030;
const VIRTIO_MMIO_QUEUE_NUM_MAX: usize = 0x034;
const VIRTIO_MMIO_QUEUE_NUM: usize = 0x038;
const VIRTIO_MMIO_QUEUE_READY: usize = 0x044;
const VIRTIO_MMIO_INTERRUPT_STATUS: usize = 0x060;
const VIRTIO_MMIO_STATUS: usize = 0x070;
const VIRTIO_MMIO_QUEUE_DESC_LOW: usize = 0x080;
const VIRTIO_MMIO_QUEUE_DESC_HIGH: usize = 0x084;
const VIRTIO_MMIO_QUEUE_DRIVER_LOW: usize = 0x090;
const VIRTIO_MMIO_QUEUE_DRIVER_HIGH: usize = 0x094;
const VIRTIO_MMIO_QUEUE_DEVICE_LOW: usize = 0x0a0;
const VIRTIO_MMIO_QUEUE_DEVICE_HIGH: usize = 0x0a4;

struct GuestMemory {
    bytes: Mutex<Vec<u8>>,
    descriptor_range: Range<usize>,
    descriptor_reads: AtomicUsize,
    descriptor_read_limit: AtomicUsize,
}

impl GuestMemory {
    fn new() -> Self {
        Self {
            bytes: Mutex::new(vec![0; GUEST_MEMORY_SIZE]),
            descriptor_range: DESCRIPTOR_AREA..DESCRIPTOR_AREA + QUEUE_NUM_MAX as usize * 16,
            descriptor_reads: AtomicUsize::new(0),
            descriptor_read_limit: AtomicUsize::new(usize::MAX),
        }
    }

    fn read(&self, address: GuestPhysAddr, buffer: &mut [u8]) -> DeviceManagerResult {
        let address = address.as_usize();
        if self.descriptor_range.contains(&address) {
            let read_number = self.descriptor_reads.fetch_add(1, Ordering::Relaxed) + 1;
            if read_number > self.descriptor_read_limit.load(Ordering::Relaxed) {
                return Err(DeviceManagerError::UnexpectedResponse {
                    operation: "read test guest descriptor",
                    detail: "descriptor read limit exceeded".into(),
                });
            }
        }

        let range = self.memory_range(address, buffer.len())?;
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

    fn set_descriptor_read_limit(&self, limit: usize) {
        self.descriptor_read_limit.store(limit, Ordering::Relaxed);
    }

    fn descriptor_reads(&self) -> usize {
        self.descriptor_reads.load(Ordering::Relaxed)
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
fn rejects_zero_and_overflowing_queue_sizes() {
    let device = new_device();

    assert!(matches!(
        mmio_write(&device, VIRTIO_MMIO_QUEUE_NUM, 0),
        Err(DeviceError::InvalidInput { .. })
    ));
    assert!(matches!(
        mmio_write(&device, VIRTIO_MMIO_QUEUE_NUM, 65_536),
        Err(DeviceError::InvalidInput { .. })
    ));
    assert!(matches!(
        mmio_write(&device, VIRTIO_MMIO_QUEUE_NUM, 3),
        Err(DeviceError::InvalidInput { .. })
    ));
}

#[test]
fn invalid_queue_selector_does_not_alias_transmit_queue() {
    let device = new_device();
    mmio_write(&device, VIRTIO_MMIO_QUEUE_SEL, TX_QUEUE).unwrap();
    mmio_write(&device, VIRTIO_MMIO_QUEUE_NUM, 8).unwrap();
    configure_queue_addresses(&device);
    mmio_write(&device, VIRTIO_MMIO_QUEUE_READY, 1).unwrap();

    mmio_write(&device, VIRTIO_MMIO_QUEUE_SEL, 2).unwrap();
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_QUEUE_NUM_MAX), 0);
    mmio_write(&device, VIRTIO_MMIO_QUEUE_READY, 0).unwrap();

    mmio_write(&device, VIRTIO_MMIO_QUEUE_SEL, TX_QUEUE).unwrap();
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_QUEUE_READY), 1);
}

#[test]
fn undersized_receive_chain_is_rejected_without_consuming_it() {
    let device = new_device();
    negotiate_mrg_rxbuf(&device);
    let memory = GuestMemory::new();
    configure_queue(&device, RX_QUEUE, 1);
    let frame = [0xa5; 14];
    let required_length = VIRTIO_NET_HEADER_LEN + frame.len();
    write_descriptor(
        &memory,
        0,
        BUFFER_AREA,
        required_length - 1,
        VRING_DESC_F_WRITE,
        0,
    );
    post_available_descriptor(&memory, 0);

    let result = deliver_rx(&device, &memory, &frame);

    assert!(matches!(
        result,
        Err(DeviceManagerError::InvalidInput { .. })
    ));
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 0);
    assert_eq!(
        memory.load(BUFFER_AREA, required_length),
        vec![0; required_length]
    );

    write_descriptor(
        &memory,
        0,
        BUFFER_AREA,
        required_length,
        VRING_DESC_F_WRITE,
        0,
    );
    assert!(deliver_rx(&device, &memory, &frame).unwrap());
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 1);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 8), required_length as u32);
    assert_eq!(load_u16(&memory, BUFFER_AREA + 10), 1);
}

#[test]
fn transmit_head_outside_queue_is_rejected_before_memory_access() {
    let device = new_device();
    let memory = GuestMemory::new();
    configure_queue(&device, TX_QUEUE, 1);
    let packet = test_transmit_packet();
    memory.store(BUFFER_AREA, &packet);
    write_descriptor(&memory, 1, BUFFER_AREA, packet.len(), 0, 0);
    post_available_descriptor(&memory, 1);

    let result = process_tx(&device, &memory);

    assert!(matches!(
        result,
        Err(DeviceManagerError::InvalidInput { .. })
    ));
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 0);
}

#[test]
fn valid_transmit_chain_returns_frame_and_marks_it_used() {
    let device = new_device();
    negotiate_mrg_rxbuf(&device);
    let memory = GuestMemory::new();
    configure_queue(&device, TX_QUEUE, 1);
    let packet = test_transmit_packet();
    memory.store(BUFFER_AREA, &packet);
    write_descriptor(&memory, 0, BUFFER_AREA, packet.len(), 0, 0);
    post_available_descriptor(&memory, 0);

    let frames = process_tx(&device, &memory).unwrap();

    assert_eq!(frames, vec![packet[VIRTIO_NET_HEADER_LEN..].to_vec()]);
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 1);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 8), packet.len() as u32);
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_INTERRUPT_STATUS), 1);
}

#[test]
fn legacy_driver_without_version_1_or_mrg_rxbuf_uses_ten_byte_transmit_header() {
    let device = new_device();
    let memory = GuestMemory::new();
    configure_queue(&device, TX_QUEUE, 1);
    let packet = test_transmit_packet_with_header_len(VIRTIO_NET_BASE_HEADER_LEN);
    memory.store(BUFFER_AREA, &packet);
    write_descriptor(&memory, 0, BUFFER_AREA, packet.len(), 0, 0);
    post_available_descriptor(&memory, 0);

    let frames = process_tx(&device, &memory).unwrap();

    assert_eq!(frames, vec![packet[VIRTIO_NET_BASE_HEADER_LEN..].to_vec()]);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 8), packet.len() as u32);
}

#[test]
fn legacy_driver_without_version_1_or_mrg_rxbuf_uses_ten_byte_receive_header() {
    let device = new_device();
    let memory = GuestMemory::new();
    configure_queue(&device, RX_QUEUE, 1);
    let frame = [0xb7; 14];
    let packet_length = VIRTIO_NET_BASE_HEADER_LEN + frame.len();
    write_descriptor(
        &memory,
        0,
        BUFFER_AREA,
        packet_length,
        VRING_DESC_F_WRITE,
        0,
    );
    post_available_descriptor(&memory, 0);

    assert!(deliver_rx(&device, &memory, &frame).unwrap());
    let mut expected = vec![0; VIRTIO_NET_BASE_HEADER_LEN];
    expected.extend_from_slice(&frame);
    assert_eq!(memory.load(BUFFER_AREA, packet_length), expected);
    assert_eq!(load_u32(&memory, DEVICE_AREA + 8), packet_length as u32);
}

#[test]
fn advertises_mrg_rxbuf_for_negotiated_twelve_byte_headers() {
    let device = new_device();

    assert_ne!(
        mmio_read(&device, VIRTIO_MMIO_DEVICE_FEATURES) as u32 & VIRTIO_NET_F_MRG_RXBUF,
        0
    );
}

#[test]
fn version_1_without_mrg_rxbuf_uses_twelve_byte_header() {
    let device = new_device();
    negotiate_version_1(&device);
    assert_twelve_byte_tx_and_rx(&device);
}

#[test]
fn fixed_twelve_byte_mode_supports_zephyr_without_mrg_rxbuf() {
    let device = new_fixed_twelve_byte_device();
    assert_twelve_byte_tx_and_rx(&device);
}

#[test]
fn cyclic_transmit_chain_is_rejected_within_queue_length_budget() {
    let device = new_device();
    let memory = GuestMemory::new();
    configure_queue(&device, TX_QUEUE, 2);
    memory.store(BUFFER_AREA, &[0; VIRTIO_NET_HEADER_LEN]);
    memory.store(BUFFER_AREA + 0x100, &[0x5a; 14]);
    write_descriptor(
        &memory,
        0,
        BUFFER_AREA,
        VIRTIO_NET_HEADER_LEN,
        VRING_DESC_F_NEXT,
        1,
    );
    write_descriptor(&memory, 1, BUFFER_AREA + 0x100, 14, VRING_DESC_F_NEXT, 0);
    post_available_descriptor(&memory, 0);
    memory.set_descriptor_read_limit(12);

    let result = process_tx(&device, &memory);

    assert!(matches!(
        result,
        Err(DeviceManagerError::InvalidInput { .. })
    ));
    assert!(memory.descriptor_reads() <= 8);
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 0);
}

#[test]
fn transmit_packet_larger_than_non_gso_limit_is_rejected() {
    let device = new_device();
    let memory = GuestMemory::new();
    configure_queue(&device, TX_QUEUE, 1);
    let packet_length = VIRTIO_NET_HEADER_LEN + MAX_ETHERNET_FRAME_LEN + 1;
    memory.store(BUFFER_AREA, &vec![0; packet_length]);
    write_descriptor(&memory, 0, BUFFER_AREA, packet_length, 0, 0);
    post_available_descriptor(&memory, 0);

    assert!(matches!(
        process_tx(&device, &memory),
        Err(DeviceManagerError::InvalidInput { .. })
    ));
    assert_eq!(load_u16(&memory, DEVICE_AREA + 2), 0);
}

#[test]
fn queue_layout_address_overflow_is_rejected_before_ready() {
    let device = new_device();
    mmio_write(&device, VIRTIO_MMIO_QUEUE_SEL, TX_QUEUE).unwrap();
    mmio_write(&device, VIRTIO_MMIO_QUEUE_NUM, 1).unwrap();
    write_raw_address(
        &device,
        VIRTIO_MMIO_QUEUE_DESC_LOW,
        VIRTIO_MMIO_QUEUE_DESC_HIGH,
        u64::MAX - 15,
    );
    write_raw_address(
        &device,
        VIRTIO_MMIO_QUEUE_DRIVER_LOW,
        VIRTIO_MMIO_QUEUE_DRIVER_HIGH,
        DRIVER_AREA as u64,
    );
    write_raw_address(
        &device,
        VIRTIO_MMIO_QUEUE_DEVICE_LOW,
        VIRTIO_MMIO_QUEUE_DEVICE_HIGH,
        DEVICE_AREA as u64,
    );

    assert!(matches!(
        mmio_write(&device, VIRTIO_MMIO_QUEUE_READY, 1),
        Err(DeviceError::InvalidInput { .. })
    ));
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_QUEUE_READY), 0);
}

#[test]
fn status_zero_resets_interrupts_and_all_queue_state() {
    let device = new_device();
    let memory = GuestMemory::new();
    configure_queue(&device, RX_QUEUE, 1);
    let frame = [0xc3; 14];
    write_descriptor(
        &memory,
        0,
        BUFFER_AREA,
        VIRTIO_NET_HEADER_LEN + frame.len(),
        VRING_DESC_F_WRITE,
        0,
    );
    post_available_descriptor(&memory, 0);
    assert!(deliver_rx(&device, &memory, &frame).unwrap());
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_INTERRUPT_STATUS), 1);
    mmio_write(&device, VIRTIO_MMIO_DRIVER_FEATURES, u32::MAX).unwrap();
    mmio_write(&device, VIRTIO_MMIO_STATUS, 0xf).unwrap();

    mmio_write(&device, VIRTIO_MMIO_STATUS, 0).unwrap();

    assert_eq!(mmio_read(&device, VIRTIO_MMIO_STATUS), 0);
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_INTERRUPT_STATUS), 0);
    assert_eq!(mmio_read(&device, VIRTIO_MMIO_QUEUE_READY), 0);
    assert!(matches!(
        mmio_write(&device, VIRTIO_MMIO_QUEUE_READY, 1),
        Err(DeviceError::InvalidState { .. })
    ));
}

#[test]
fn virtio_net_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<VirtioNet>();
}

#[test]
fn exposes_mac_and_network_segment_without_changing_default_segment() {
    let mac = [0x52, 0x54, 0, 0, 0, 9];
    let default_segment = VirtioNet::new(GuestPhysAddr::from_usize(MMIO_BASE), mac, 32);
    let selected_segment =
        VirtioNet::new_with_segment(GuestPhysAddr::from_usize(MMIO_BASE), mac, 32, 7);

    assert_eq!(default_segment.mac(), mac);
    assert_eq!(default_segment.segment_id(), 0);
    assert_eq!(selected_segment.mac(), mac);
    assert_eq!(selected_segment.segment_id(), 7);
}

#[test]
fn device_config_assigns_mac_suffix_and_network_segment() {
    let devices =
        AxVmDevices::new(AxVmDeviceConfig::new(vec![virtio_net_config(vec![9, 7])])).unwrap();
    let device = &devices.virtio_nets()[0];

    assert_eq!(device.mac(), [0x52, 0x54, 0, 0, 0, 9]);
    assert_eq!(device.segment_id(), 7);
}

#[test]
fn device_config_rejects_network_segment_outside_u16() {
    let result = AxVmDevices::new(AxVmDeviceConfig::new(vec![virtio_net_config(vec![
        1,
        u16::MAX as usize + 1,
    ])]));

    assert!(matches!(
        result,
        Err(DeviceManagerError::InvalidConfig { .. })
    ));
}

#[test]
fn device_config_rejects_unknown_header_compatibility_mode() {
    let result = AxVmDevices::new(AxVmDeviceConfig::new(vec![virtio_net_config(vec![
        1, 0, 2,
    ])]));

    assert!(matches!(
        result,
        Err(DeviceManagerError::InvalidConfig { .. })
    ));
}

fn new_device() -> VirtioNet {
    VirtioNet::new(
        GuestPhysAddr::from_usize(MMIO_BASE),
        [0x52, 0x54, 0, 0, 0, 1],
        32,
    )
}

fn new_fixed_twelve_byte_device() -> VirtioNet {
    VirtioNet::new_with_options(
        GuestPhysAddr::from_usize(MMIO_BASE),
        [0x52, 0x54, 0, 0, 0, 2],
        32,
        VirtioNetOptions {
            segment_id: 0,
            header_mode: VirtioNetHeaderMode::FixedTwelveByte,
        },
    )
}

fn negotiate_mrg_rxbuf(device: &VirtioNet) {
    mmio_write(device, VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0).unwrap();
    mmio_write(device, VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_NET_F_MRG_RXBUF).unwrap();
}

fn negotiate_version_1(device: &VirtioNet) {
    mmio_write(device, VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1).unwrap();
    mmio_write(
        device,
        VIRTIO_MMIO_DRIVER_FEATURES,
        VIRTIO_F_VERSION_1_WORD_1,
    )
    .unwrap();
}

fn assert_twelve_byte_tx_and_rx(device: &VirtioNet) {
    let tx_memory = GuestMemory::new();
    configure_queue(device, TX_QUEUE, 1);
    let packet = test_transmit_packet();
    tx_memory.store(BUFFER_AREA, &packet);
    write_descriptor(&tx_memory, 0, BUFFER_AREA, packet.len(), 0, 0);
    post_available_descriptor(&tx_memory, 0);

    assert_eq!(
        process_tx(device, &tx_memory).unwrap(),
        vec![packet[VIRTIO_NET_HEADER_LEN..].to_vec()]
    );

    let rx_memory = GuestMemory::new();
    configure_queue(device, RX_QUEUE, 1);
    let frame = [0xd2; 14];
    let packet_length = VIRTIO_NET_HEADER_LEN + frame.len();
    write_descriptor(
        &rx_memory,
        0,
        BUFFER_AREA,
        packet_length,
        VRING_DESC_F_WRITE,
        0,
    );
    post_available_descriptor(&rx_memory, 0);

    assert!(deliver_rx(device, &rx_memory, &frame).unwrap());
    assert_eq!(load_u16(&rx_memory, BUFFER_AREA + 10), 1);
    assert_eq!(
        rx_memory.load(BUFFER_AREA + VIRTIO_NET_HEADER_LEN, frame.len()),
        frame
    );
}

fn virtio_net_config(cfg_list: Vec<usize>) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: "virtio-net".into(),
        base_gpa: MMIO_BASE,
        length: 0x1000,
        irq_id: 32,
        emu_type: EmulatedDeviceType::VirtioNet,
        cfg_list,
    }
}

fn configure_queue(device: &VirtioNet, queue: u32, size: u32) {
    mmio_write(device, VIRTIO_MMIO_QUEUE_SEL, queue).unwrap();
    mmio_write(device, VIRTIO_MMIO_QUEUE_NUM, size).unwrap();
    configure_queue_addresses(device);
    mmio_write(device, VIRTIO_MMIO_QUEUE_READY, 1).unwrap();
}

fn configure_queue_addresses(device: &VirtioNet) {
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
}

fn write_address(device: &VirtioNet, low: usize, high: usize, address: usize) {
    write_raw_address(device, low, high, address as u64);
}

fn write_raw_address(device: &VirtioNet, low: usize, high: usize, address: u64) {
    mmio_write(device, low, address as u32).unwrap();
    mmio_write(device, high, (address >> 32) as u32).unwrap();
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

fn post_available_descriptor(memory: &GuestMemory, head: u16) {
    memory.store(DRIVER_AREA + 2, &1u16.to_le_bytes());
    memory.store(DRIVER_AREA + 4, &head.to_le_bytes());
}

fn test_transmit_packet() -> Vec<u8> {
    test_transmit_packet_with_header_len(VIRTIO_NET_HEADER_LEN)
}

fn test_transmit_packet_with_header_len(header_len: usize) -> Vec<u8> {
    let mut packet = vec![0; header_len];
    packet.extend_from_slice(&[0x3c; 14]);
    packet
}

fn process_tx(device: &VirtioNet, memory: &GuestMemory) -> DeviceManagerResult<Vec<Vec<u8>>> {
    device.process_tx(
        &|address, buffer| memory.read(address, buffer),
        &|address, buffer| memory.write(address, buffer),
    )
}

fn deliver_rx(device: &VirtioNet, memory: &GuestMemory, frame: &[u8]) -> DeviceManagerResult<bool> {
    device.deliver_rx(
        &|address, buffer| memory.read(address, buffer),
        &|address, buffer| memory.write(address, buffer),
        frame,
    )
}

fn mmio_read(device: &VirtioNet, offset: usize) -> usize {
    device
        .handle_read(
            GuestPhysAddr::from_usize(MMIO_BASE + offset),
            AccessWidth::Dword,
        )
        .unwrap()
}

fn mmio_write(device: &VirtioNet, offset: usize, value: u32) -> DeviceResult {
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
