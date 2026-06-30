//! Minimal emulated virtio-net device (virtio-mmio, version 2) for inter-guest
//! networking (task 2).
//!
//! Scope (P1): implement the virtio-mmio register state machine so a guest's
//! virtio-net driver detects the device, negotiates features, and sets up its
//! virtqueues (Linux then creates `eth0`). Actual frame movement across the
//! virtqueues and the inter-VM software switch are added in later phases (P2/P3);
//! for now `QueueNotify` is accepted but no buffers are processed.

use alloc::{vec, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceResult, EmuDeviceType};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

use crate::DeviceManagerResult;

// Split-virtqueue layout constants (virtio 1.x, 2.6).
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;
const VIRTIO_NET_HDR_LEN: usize = 12; // virtio_net_hdr_v1 (VERSION_1)
const RX_QUEUE: usize = 0; // receiveq
const TX_QUEUE: usize = 1; // transmitq

/// A closure that reads `buf.len()` bytes of guest physical memory at `gpa`.
type GuestRead<'a> = &'a dyn Fn(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult;
/// A closure that writes `data` to guest physical memory at `gpa`.
type GuestWrite<'a> = &'a dyn Fn(GuestPhysAddr, &[u8]) -> DeviceManagerResult;

fn rd_u16(read: GuestRead, gpa: u64) -> DeviceManagerResult<u16> {
    let mut b = [0u8; 2];
    read(GuestPhysAddr::from_usize(gpa as usize), &mut b)?;
    Ok(u16::from_le_bytes(b))
}
fn rd_u32(read: GuestRead, gpa: u64) -> DeviceManagerResult<u32> {
    let mut b = [0u8; 4];
    read(GuestPhysAddr::from_usize(gpa as usize), &mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn rd_u64(read: GuestRead, gpa: u64) -> DeviceManagerResult<u64> {
    let mut b = [0u8; 8];
    read(GuestPhysAddr::from_usize(gpa as usize), &mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn wr_u16(write: GuestWrite, gpa: u64, v: u16) -> DeviceManagerResult {
    write(GuestPhysAddr::from_usize(gpa as usize), &v.to_le_bytes())
}
fn wr_u32(write: GuestWrite, gpa: u64, v: u32) -> DeviceManagerResult {
    write(GuestPhysAddr::from_usize(gpa as usize), &v.to_le_bytes())
}

// virtio-mmio register offsets (see virtio spec 1.x, 4.2.2).
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

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976; // "virt"
const VIRTIO_MMIO_VERSION_2: u32 = 2;
const VIRTIO_ID_NET: u32 = 1;
const VIRTIO_VENDOR_ID: u32 = 0x1AF4;

// Feature bits we advertise.
const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const DEVICE_FEATURES: u64 = VIRTIO_NET_F_MAC | VIRTIO_F_VERSION_1;

const QUEUE_NUM_MAX: u32 = 256;
/// receiveq(0) + transmitq(1).
const NUM_QUEUES: usize = 2;
/// MMIO window size: registers + net config space.
pub const VIRTIO_NET_MMIO_SIZE: usize = 0x200;

#[derive(Clone, Copy, Default)]
struct QueueState {
    num: u32,
    ready: u32,
    desc: u64,
    driver: u64,
    device: u64,
    /// Next avail-ring index this device has not yet consumed.
    last_avail: u16,
}

struct VirtioNetState {
    device_features_sel: u32,
    driver_features: [u32; 2],
    driver_features_sel: u32,
    queue_sel: u32,
    queues: [QueueState; NUM_QUEUES],
    status: u32,
    interrupt_status: u32,
    mac: [u8; 6],
}

/// An emulated virtio-net device exposed to one guest over virtio-mmio.
pub struct VirtioNet {
    base: GuestPhysAddr,
    size: usize,
    irq: usize,
    state: Mutex<VirtioNetState>,
}

impl VirtioNet {
    /// Creates a virtio-net device at `base` with the given `mac` and IRQ line.
    pub fn new(base: GuestPhysAddr, mac: [u8; 6], irq: usize) -> Self {
        Self {
            base,
            size: VIRTIO_NET_MMIO_SIZE,
            irq,
            state: Mutex::new(VirtioNetState {
                device_features_sel: 0,
                driver_features: [0; 2],
                driver_features_sel: 0,
                queue_sel: 0,
                queues: [QueueState::default(); NUM_QUEUES],
                status: 0,
                interrupt_status: 0,
                mac,
            }),
        }
    }

    /// The guest IRQ line this device injects on TX completion / RX arrival.
    pub fn irq(&self) -> usize {
        self.irq
    }

    /// The base guest-physical address of this device's MMIO window.
    pub fn base(&self) -> GuestPhysAddr {
        self.base
    }

    /// Returns whether the guest has notified the transmit queue at `offset`.
    pub fn is_tx_notify(&self, offset: usize, val: u32) -> bool {
        offset == VIRTIO_MMIO_QUEUE_NOTIFY && val as usize == TX_QUEUE
    }

    /// Drains the transmit virtqueue, returning the Ethernet frames the guest sent
    /// (virtio-net header stripped). Marks the consumed descriptors used and raises
    /// the device interrupt-status bit (the caller injects the IRQ).
    pub fn process_tx(
        &self,
        read: GuestRead,
        write: GuestWrite,
    ) -> DeviceManagerResult<Vec<Vec<u8>>> {
        let mut state = self.state.lock();
        let q = state.queues[TX_QUEUE];
        let mut frames = Vec::new();
        if q.ready == 0 || q.num == 0 {
            return Ok(frames);
        }
        let num = q.num as u16;
        let avail_idx = rd_u16(read, q.driver + 2)?;
        while state.queues[TX_QUEUE].last_avail != avail_idx {
            let slot = (state.queues[TX_QUEUE].last_avail % num) as u64;
            let head = rd_u16(read, q.driver + 4 + slot * 2)?;
            let mut frame = Vec::new();
            let mut idx = head;
            loop {
                let d = q.desc + (idx as u64) * 16;
                let addr = rd_u64(read, d)?;
                let len = rd_u32(read, d + 8)?;
                let flags = rd_u16(read, d + 12)?;
                let next = rd_u16(read, d + 14)?;
                let mut buf = vec![0u8; len as usize];
                read(GuestPhysAddr::from_usize(addr as usize), &mut buf)?;
                frame.extend_from_slice(&buf);
                if flags & VRING_DESC_F_NEXT != 0 {
                    idx = next;
                } else {
                    break;
                }
            }
            // mark the chain used
            let used_idx = rd_u16(read, q.device + 2)?;
            let uslot = (used_idx % num) as u64;
            wr_u32(write, q.device + 4 + uslot * 8, head as u32)?;
            wr_u32(write, q.device + 4 + uslot * 8 + 4, frame.len() as u32)?;
            wr_u16(write, q.device + 2, used_idx.wrapping_add(1))?;
            state.queues[TX_QUEUE].last_avail = state.queues[TX_QUEUE].last_avail.wrapping_add(1);
            if frame.len() > VIRTIO_NET_HDR_LEN {
                frames.push(frame[VIRTIO_NET_HDR_LEN..].to_vec());
            }
        }
        if !frames.is_empty() {
            state.interrupt_status |= 1;
        }
        Ok(frames)
    }

    /// Delivers one received Ethernet `frame` into the receive virtqueue (prepends a
    /// zeroed virtio-net header). Returns `true` if a buffer was available and the
    /// device interrupt should be injected; `false` drops the frame (no RX buffer).
    pub fn deliver_rx(
        &self,
        read: GuestRead,
        write: GuestWrite,
        frame: &[u8],
    ) -> DeviceManagerResult<bool> {
        let mut state = self.state.lock();
        let q = state.queues[RX_QUEUE];
        if q.ready == 0 || q.num == 0 {
            return Ok(false);
        }
        let num = q.num as u16;
        let avail_idx = rd_u16(read, q.driver + 2)?;
        if state.queues[RX_QUEUE].last_avail == avail_idx {
            return Ok(false); // no RX buffer posted by the driver
        }
        let slot = (state.queues[RX_QUEUE].last_avail % num) as u64;
        let head = rd_u16(read, q.driver + 4 + slot * 2)?;

        let mut payload = vec![0u8; VIRTIO_NET_HDR_LEN];
        payload.extend_from_slice(frame);
        let mut written = 0usize;
        let mut idx = head;
        loop {
            let d = q.desc + (idx as u64) * 16;
            let addr = rd_u64(read, d)?;
            let len = rd_u32(read, d + 8)? as usize;
            let flags = rd_u16(read, d + 12)?;
            let next = rd_u16(read, d + 14)?;
            if flags & VRING_DESC_F_WRITE != 0 && written < payload.len() {
                let n = core::cmp::min(len, payload.len() - written);
                write(
                    GuestPhysAddr::from_usize(addr as usize),
                    &payload[written..written + n],
                )?;
                written += n;
            }
            if flags & VRING_DESC_F_NEXT != 0 {
                idx = next;
            } else {
                break;
            }
        }
        let used_idx = rd_u16(read, q.device + 2)?;
        let uslot = (used_idx % num) as u64;
        wr_u32(write, q.device + 4 + uslot * 8, head as u32)?;
        wr_u32(write, q.device + 4 + uslot * 8 + 4, written as u32)?;
        wr_u16(write, q.device + 2, used_idx.wrapping_add(1))?;
        state.queues[RX_QUEUE].last_avail = state.queues[RX_QUEUE].last_avail.wrapping_add(1);
        state.interrupt_status |= 1;
        Ok(true)
    }

    fn read_reg(&self, offset: usize) -> u32 {
        let state = self.state.lock();
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_NET,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_VENDOR_ID,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if state.device_features_sel == 0 {
                    DEVICE_FEATURES as u32
                } else {
                    (DEVICE_FEATURES >> 32) as u32
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => QUEUE_NUM_MAX,
            VIRTIO_MMIO_QUEUE_READY => self.cur_queue(&state).ready,
            VIRTIO_MMIO_INTERRUPT_STATUS => state.interrupt_status,
            VIRTIO_MMIO_STATUS => state.status,
            VIRTIO_MMIO_CONFIG_GENERATION => 0,
            // net config space: mac[0..6]
            off if (VIRTIO_MMIO_CONFIG..VIRTIO_MMIO_CONFIG + 6).contains(&off) => {
                state.mac[off - VIRTIO_MMIO_CONFIG] as u32
            }
            _ => 0,
        }
    }

    fn write_reg(&self, offset: usize, val: u32) {
        let mut state = self.state.lock();
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => state.device_features_sel = val,
            VIRTIO_MMIO_DRIVER_FEATURES => {
                let sel = state.driver_features_sel.min(1) as usize;
                state.driver_features[sel] = val;
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => state.driver_features_sel = val,
            VIRTIO_MMIO_QUEUE_SEL => state.queue_sel = val,
            VIRTIO_MMIO_QUEUE_NUM => self.cur_queue_mut(&mut state).num = val,
            VIRTIO_MMIO_QUEUE_READY => self.cur_queue_mut(&mut state).ready = val,
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                // P2/P3: process the notified virtqueue and forward frames via the switch.
            }
            VIRTIO_MMIO_INTERRUPT_ACK => state.interrupt_status &= !val,
            VIRTIO_MMIO_STATUS => state.status = val,
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                self.set_queue_addr(&mut state, QueueAddr::Desc, false, val)
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                self.set_queue_addr(&mut state, QueueAddr::Desc, true, val)
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                self.set_queue_addr(&mut state, QueueAddr::Driver, false, val)
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                self.set_queue_addr(&mut state, QueueAddr::Driver, true, val)
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                self.set_queue_addr(&mut state, QueueAddr::Device, false, val)
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                self.set_queue_addr(&mut state, QueueAddr::Device, true, val)
            }
            _ => {}
        }
    }

    fn cur_queue<'a>(&self, state: &'a VirtioNetState) -> &'a QueueState {
        let idx = (state.queue_sel as usize).min(NUM_QUEUES - 1);
        &state.queues[idx]
    }
    fn cur_queue_mut<'a>(&self, state: &'a mut VirtioNetState) -> &'a mut QueueState {
        let idx = (state.queue_sel as usize).min(NUM_QUEUES - 1);
        &mut state.queues[idx]
    }
    fn set_queue_addr(&self, state: &mut VirtioNetState, which: QueueAddr, high: bool, val: u32) {
        let q = self.cur_queue_mut(state);
        let field = match which {
            QueueAddr::Desc => &mut q.desc,
            QueueAddr::Driver => &mut q.driver,
            QueueAddr::Device => &mut q.device,
        };
        if high {
            *field = (*field & 0x0000_0000_ffff_ffff) | ((val as u64) << 32);
        } else {
            *field = (*field & 0xffff_ffff_0000_0000) | (val as u64);
        }
    }
}

enum QueueAddr {
    Desc,
    Driver,
    Device,
}

impl BaseDeviceOps<GuestPhysAddrRange> for VirtioNet {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::VirtioNet
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        let offset = addr.as_usize() - self.base.as_usize();
        let value = match width {
            AccessWidth::Byte => self.read_reg(offset) & 0xff,
            AccessWidth::Word => self.read_reg(offset) & 0xffff,
            AccessWidth::Dword => self.read_reg(offset),
            AccessWidth::Qword => self.read_reg(offset), // 64-bit MMIO not used by virtio-mmio
        };
        Ok(value as usize)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> DeviceResult {
        let offset = addr.as_usize() - self.base.as_usize();
        let val = match width {
            AccessWidth::Byte => (val & 0xff) as u32,
            AccessWidth::Word => (val & 0xffff) as u32,
            _ => val as u32,
        };
        self.write_reg(offset, val);
        Ok(())
    }
}
