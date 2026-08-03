//! In-memory virtio block device backed by a split virtqueue and virtio-mmio.

use alloc::{boxed::Box, format, vec::Vec};
use core::fmt;

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceError, DeviceResult, EmuDeviceType};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

use crate::{
    DeviceManagerError, DeviceManagerResult,
    virtio::{
        memory::{GuestRead, GuestWrite},
        queue::{QueueAddressKind, QueueState},
    },
};

mod descriptor;

use descriptor::{BlockRequest, RequestType};

const SECTOR_SIZE: usize = 512;
const NUM_QUEUES: usize = 1;
const REQUEST_QUEUE: usize = 0;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

const VIRTIO_MMIO_MAGIC_VALUE: usize = 0x000;
const VIRTIO_MMIO_VERSION: usize = 0x004;
const VIRTIO_MMIO_DEVICE_ID: usize = 0x008;
const VIRTIO_MMIO_VENDOR_ID: usize = 0x00c;
const VIRTIO_MMIO_DEVICE_FEATURES: usize = 0x010;
const VIRTIO_MMIO_DEVICE_FEATURES_SEL: usize = 0x014;
const VIRTIO_MMIO_DRIVER_FEATURES: usize = 0x020;
const VIRTIO_MMIO_DRIVER_FEATURES_SEL: usize = 0x024;
const VIRTIO_MMIO_QUEUE_SEL: usize = 0x030;
const VIRTIO_MMIO_QUEUE_NUM_MAX: usize = 0x034;
const VIRTIO_MMIO_QUEUE_NUM: usize = 0x038;
const VIRTIO_MMIO_QUEUE_READY: usize = 0x044;
const VIRTIO_MMIO_QUEUE_NOTIFY: usize = 0x050;
const VIRTIO_MMIO_INTERRUPT_STATUS: usize = 0x060;
const VIRTIO_MMIO_INTERRUPT_ACK: usize = 0x064;
const VIRTIO_MMIO_STATUS: usize = 0x070;
const VIRTIO_MMIO_QUEUE_DESC_LOW: usize = 0x080;
const VIRTIO_MMIO_QUEUE_DESC_HIGH: usize = 0x084;
const VIRTIO_MMIO_QUEUE_DRIVER_LOW: usize = 0x090;
const VIRTIO_MMIO_QUEUE_DRIVER_HIGH: usize = 0x094;
const VIRTIO_MMIO_QUEUE_DEVICE_LOW: usize = 0x0a0;
const VIRTIO_MMIO_QUEUE_DEVICE_HIGH: usize = 0x0a4;
const VIRTIO_MMIO_CONFIG_GENERATION: usize = 0x0fc;
const VIRTIO_MMIO_CONFIG: usize = 0x100;

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_VERSION_2: u32 = 2;
const VIRTIO_ID_BLOCK: u32 = 2;
const VIRTIO_VENDOR_ID: u32 = 0x1af4;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const DEVICE_FEATURES: u64 = VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_FLUSH;

const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// MMIO window size for one emulated virtio block device.
pub const VIRTIO_BLOCK_MMIO_SIZE: usize = 0x1000;

/// A volatile, memory-backed block capability shared with one virtio device.
///
/// Writes are not persisted implicitly. Call [`Self::snapshot`] after the VM
/// has stopped when a caller explicitly needs the final image bytes.
pub struct MemoryBlockBackend {
    bytes: Mutex<Box<[u8]>>,
}

impl MemoryBlockBackend {
    /// Creates a block backing from complete image bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the image is empty or not sector aligned.
    pub fn new(bytes: Vec<u8>) -> DeviceManagerResult<Self> {
        if bytes.is_empty() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "create memory block backend",
                detail: "block image must not be empty".into(),
            });
        }
        if !bytes.len().is_multiple_of(SECTOR_SIZE) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "create memory block backend",
                detail: format!(
                    "block image size {} is not aligned to {SECTOR_SIZE} bytes",
                    bytes.len()
                ),
            });
        }
        Ok(Self {
            bytes: Mutex::new(bytes.into_boxed_slice()),
        })
    }

    /// Returns the capacity in bytes.
    pub fn len(&self) -> usize {
        self.bytes.lock().len()
    }

    /// Returns whether this backing contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copies bytes from the backing at `offset`.
    pub fn read(&self, offset: usize, destination: &mut [u8]) -> DeviceManagerResult {
        let bytes = self.bytes.lock();
        let source = checked_block_range(&bytes, offset, destination.len(), "read block image")?;
        destination.copy_from_slice(source);
        Ok(())
    }

    /// Copies bytes into the volatile backing at `offset`.
    pub fn write(&self, offset: usize, source: &[u8]) -> DeviceManagerResult {
        let mut bytes = self.bytes.lock();
        let destination =
            checked_block_range_mut(&mut bytes, offset, source.len(), "write block image")?;
        destination.copy_from_slice(source);
        Ok(())
    }

    /// Copies the complete current backing into an owned snapshot.
    ///
    /// The returned bytes are independent of later guest writes. Callers must
    /// quiesce the owning VM before using this method as a persistence boundary.
    pub fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().to_vec()
    }

    fn capacity_sectors(&self) -> u64 {
        (self.len() / SECTOR_SIZE) as u64
    }
}

impl fmt::Debug for MemoryBlockBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryBlockBackend")
            .field("capacity_bytes", &self.len())
            .finish()
    }
}

fn checked_block_range<'a>(
    bytes: &'a [u8],
    offset: usize,
    length: usize,
    operation: &'static str,
) -> DeviceManagerResult<&'a [u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation,
            detail: format!("block range {offset:#x}+{length:#x} overflows"),
        })?;
    bytes
        .get(offset..end)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation,
            detail: format!(
                "block range {offset:#x}..{end:#x} exceeds capacity {:#x}",
                bytes.len()
            ),
        })
}

fn checked_block_range_mut<'a>(
    bytes: &'a mut [u8],
    offset: usize,
    length: usize,
    operation: &'static str,
) -> DeviceManagerResult<&'a mut [u8]> {
    let capacity = bytes.len();
    let end = offset
        .checked_add(length)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation,
            detail: format!("block range {offset:#x}+{length:#x} overflows"),
        })?;
    bytes
        .get_mut(offset..end)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation,
            detail: format!("block range {offset:#x}..{end:#x} exceeds capacity {capacity:#x}"),
        })
}

#[derive(Default)]
struct VirtioBlockState {
    device_features_sel: u32,
    driver_features: [u32; 2],
    driver_features_sel: u32,
    queue_sel: u32,
    queues: [QueueState; NUM_QUEUES],
    status: u32,
    interrupt_status: u32,
}

impl VirtioBlockState {
    fn selected_queue(&self) -> Option<&QueueState> {
        self.queues.get(self.queue_sel as usize)
    }

    fn selected_queue_mut(&mut self) -> Option<&mut QueueState> {
        self.queues.get_mut(self.queue_sel as usize)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// An emulated virtio block device exposed to one guest over virtio-mmio.
pub struct VirtioBlock {
    base: GuestPhysAddr,
    size: usize,
    irq: usize,
    backend: alloc::sync::Arc<MemoryBlockBackend>,
    state: Mutex<VirtioBlockState>,
}

impl VirtioBlock {
    /// Creates a virtio block device backed by `backend`.
    pub fn new(
        base: GuestPhysAddr,
        irq: usize,
        backend: alloc::sync::Arc<MemoryBlockBackend>,
    ) -> Self {
        Self {
            base,
            size: VIRTIO_BLOCK_MMIO_SIZE,
            irq,
            backend,
            state: Mutex::new(VirtioBlockState::default()),
        }
    }

    /// Returns the base guest-physical address of the MMIO window.
    pub fn base(&self) -> GuestPhysAddr {
        self.base
    }

    /// Returns the guest IRQ line asserted for request completion.
    pub fn irq(&self) -> usize {
        self.irq
    }

    /// Returns whether an MMIO write notifies this device's request queue.
    pub fn is_queue_notify(&self, offset: usize, value: u32) -> bool {
        offset == VIRTIO_MMIO_QUEUE_NOTIFY && value as usize == REQUEST_QUEUE
    }

    /// Processes all currently available block requests.
    ///
    /// The state lock is always acquired before the backing lock. No backing
    /// method calls into guest memory or re-enters the device.
    pub fn process_queue(
        &self,
        read: GuestRead<'_>,
        write: GuestWrite<'_>,
    ) -> DeviceManagerResult<usize> {
        let mut state = self.state.lock();
        let Some(queue) = state.queues[REQUEST_QUEUE].active("process virtio block requests")?
        else {
            return Ok(0);
        };
        let pending = queue.pending_count(read)?;
        let mut available_index = queue.last_avail();
        let mut completed = 0usize;

        for _ in 0..pending {
            let head = queue.available_head(read, available_index)?;
            let request = BlockRequest::read(
                read,
                queue.descriptor_table(),
                queue.size(),
                head,
                MAX_REQUEST_BYTES,
            )?;
            let completion = self.execute_request(read, write, &request)?;
            request.write_status(write, completion.status)?;
            queue.write_used(read, write, head, completion.used_length)?;
            available_index = available_index.wrapping_add(1);
            state.queues[REQUEST_QUEUE].complete_available();
            state.interrupt_status |= 1;
            completed += 1;
        }
        Ok(completed)
    }

    fn execute_request(
        &self,
        read: GuestRead<'_>,
        write: GuestWrite<'_>,
        request: &BlockRequest,
    ) -> DeviceManagerResult<BlockCompletion> {
        match request.request_type() {
            RequestType::Read => {
                let mut data = request.allocate_data_buffer(true)?;
                let status = match self.request_offset(request, data.len()) {
                    Ok(offset) => match self.backend.read(offset, &mut data) {
                        Ok(()) => {
                            request.write_data(write, &data)?;
                            VIRTIO_BLK_S_OK
                        }
                        Err(_) => VIRTIO_BLK_S_IOERR,
                    },
                    Err(()) => VIRTIO_BLK_S_IOERR,
                };
                let used_length = if status == VIRTIO_BLK_S_OK {
                    data.len() + 1
                } else {
                    1
                };
                Ok(BlockCompletion {
                    status,
                    used_length,
                })
            }
            RequestType::Write => {
                let data = request.read_data(read, false)?;
                let status = match self.request_offset(request, data.len()) {
                    Ok(offset) => match self.backend.write(offset, &data) {
                        Ok(()) => VIRTIO_BLK_S_OK,
                        Err(_) => VIRTIO_BLK_S_IOERR,
                    },
                    Err(()) => VIRTIO_BLK_S_IOERR,
                };
                Ok(BlockCompletion {
                    status,
                    used_length: 1,
                })
            }
            RequestType::Flush => {
                request.require_empty_data()?;
                Ok(BlockCompletion {
                    status: VIRTIO_BLK_S_OK,
                    used_length: 1,
                })
            }
            RequestType::Unsupported => Ok(BlockCompletion {
                status: VIRTIO_BLK_S_UNSUPP,
                used_length: 1,
            }),
        }
    }

    fn request_offset(&self, request: &BlockRequest, length: usize) -> Result<usize, ()> {
        if !length.is_multiple_of(SECTOR_SIZE) {
            return Err(());
        }
        let sector = usize::try_from(request.sector()).map_err(|_| ())?;
        let offset = sector.checked_mul(SECTOR_SIZE).ok_or(())?;
        let end = offset.checked_add(length).ok_or(())?;
        (end <= self.backend.len()).then_some(offset).ok_or(())
    }

    fn guest_address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn read_register(&self, offset: usize) -> u32 {
        let state = self.state.lock();
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_BLOCK,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_VENDOR_ID,
            VIRTIO_MMIO_DEVICE_FEATURES => match state.device_features_sel {
                0 => DEVICE_FEATURES as u32,
                1 => (DEVICE_FEATURES >> 32) as u32,
                _ => 0,
            },
            VIRTIO_MMIO_QUEUE_NUM_MAX => state
                .selected_queue()
                .map(|_| u32::from(crate::virtio::queue::QUEUE_NUM_MAX))
                .unwrap_or(0),
            VIRTIO_MMIO_QUEUE_READY => state
                .selected_queue()
                .map(QueueState::ready_value)
                .unwrap_or(0),
            VIRTIO_MMIO_INTERRUPT_STATUS => state.interrupt_status,
            VIRTIO_MMIO_STATUS => state.status,
            VIRTIO_MMIO_CONFIG_GENERATION => 0,
            VIRTIO_MMIO_CONFIG => self.backend.capacity_sectors() as u32,
            offset if offset == VIRTIO_MMIO_CONFIG + 4 => {
                (self.backend.capacity_sectors() >> 32) as u32
            }
            _ => 0,
        }
    }

    fn write_register(&self, offset: usize, value: u32) -> DeviceResult {
        let mut state = self.state.lock();
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => state.device_features_sel = value,
            VIRTIO_MMIO_DRIVER_FEATURES => {
                let feature_word = state.driver_features_sel as usize;
                if let Some(features) = state.driver_features.get_mut(feature_word) {
                    *features = value;
                }
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => state.driver_features_sel = value,
            VIRTIO_MMIO_QUEUE_SEL => state.queue_sel = value,
            VIRTIO_MMIO_QUEUE_NUM => {
                if let Some(queue) = state.selected_queue_mut() {
                    queue.set_size(value)?;
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if let Some(queue) = state.selected_queue_mut() {
                    queue.set_ready(value)?;
                }
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {}
            VIRTIO_MMIO_INTERRUPT_ACK => state.interrupt_status &= !value,
            VIRTIO_MMIO_STATUS if value == 0 => state.reset(),
            VIRTIO_MMIO_STATUS => state.status = value,
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                Self::set_queue_address(&mut state, QueueAddressKind::Descriptor, false, value)?;
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                Self::set_queue_address(&mut state, QueueAddressKind::Descriptor, true, value)?;
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                Self::set_queue_address(&mut state, QueueAddressKind::Driver, false, value)?;
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                Self::set_queue_address(&mut state, QueueAddressKind::Driver, true, value)?;
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                Self::set_queue_address(&mut state, QueueAddressKind::Device, false, value)?;
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                Self::set_queue_address(&mut state, QueueAddressKind::Device, true, value)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn set_queue_address(
        state: &mut VirtioBlockState,
        kind: QueueAddressKind,
        high: bool,
        value: u32,
    ) -> DeviceResult {
        if let Some(queue) = state.selected_queue_mut() {
            queue.set_address(kind, high, value)?;
        }
        Ok(())
    }

    fn mmio_offset(&self, address: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        let raw_address = address.as_usize();
        let offset =
            raw_address
                .checked_sub(self.base.as_usize())
                .ok_or(DeviceError::OutOfRange {
                    addr: raw_address as u64,
                })?;
        let access_size = match width {
            AccessWidth::Byte => 1,
            AccessWidth::Word => 2,
            AccessWidth::Dword => 4,
            AccessWidth::Qword => 8,
        };
        offset
            .checked_add(access_size)
            .filter(|end| *end <= self.size)
            .ok_or(DeviceError::OutOfRange {
                addr: raw_address as u64,
            })?;
        Ok(offset)
    }
}

struct BlockCompletion {
    status: u8,
    used_length: usize,
}

impl BaseDeviceOps<GuestPhysAddrRange> for VirtioBlock {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::VirtioBlk
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        self.guest_address_range()
    }

    fn handle_read(&self, address: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        let offset = self.mmio_offset(address, width)?;
        if offset == VIRTIO_MMIO_CONFIG && matches!(width, AccessWidth::Qword) {
            return Ok(self.backend.capacity_sectors() as usize);
        }
        let value = match width {
            AccessWidth::Byte => self.read_register(offset) & 0xff,
            AccessWidth::Word => self.read_register(offset) & 0xffff,
            AccessWidth::Dword | AccessWidth::Qword => self.read_register(offset),
        };
        Ok(value as usize)
    }

    fn handle_write(
        &self,
        address: GuestPhysAddr,
        width: AccessWidth,
        value: usize,
    ) -> DeviceResult {
        let offset = self.mmio_offset(address, width)?;
        let value = match width {
            AccessWidth::Byte => (value & 0xff) as u32,
            AccessWidth::Word => (value & 0xffff) as u32,
            AccessWidth::Dword | AccessWidth::Qword => value as u32,
        };
        self.write_register(offset, value)
    }
}
