//! VirtIO MMIO network device.

use alloc::{sync::Arc, vec::Vec};

use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    self, DescriptorChain, VirtioError, VirtioQueue, VirtioResult, constants as vc, mmio::transport,
};
use axvm_types::{AccessWidth, GuestPhysAddr};
use spin::Mutex;

use crate::{
    NetError, NetworkBackend, VirtioNetConfig, VirtioNetHdr, config::LinkStatus, constants::*,
};

/// Outcome of an MMIO write, reported to the VMM so it can drive slow paths
/// (IRQ injection, reset) outside any device-internal lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEvent {
    /// Nothing for the VMM to do.
    None,
    /// The device raised an interrupt bit; the VMM may inject a virtual IRQ.
    InterruptPending,
    /// The guest reset the device (wrote status 0).
    Reset,
}

/// Result of a host-driven RX delivery (plan section 7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxOutcome {
    /// The frame was written into a guest buffer.
    Delivered { frame_len: usize },
    /// No guest RX buffer was available; the VMM decides whether to
    /// cache/retry/drop. This is flow control, not an error.
    NoGuestBuffer,
}

/// A VirtIO 1.x MMIO network device with one RX/TX queue pair.
///
/// `B` is the host transmit backend; `T` translates guest physical addresses.
/// The portable device model owns only the protocol state; actual IRQ
/// injection and TAP/switch lifecycle belong to the VMM glue.
pub struct VirtioMmioNetDevice<B: NetworkBackend, T: GuestMemoryAccessor + Clone> {
    base_ipa: GuestPhysAddr,
    length: usize,
    mac: [u8; 6],
    mtu: Option<u16>,
    link: Mutex<LinkStatus>,
    device_features: u64,
    status: Mutex<u32>,
    driver_features: Mutex<u64>,
    device_features_sel: Mutex<u32>,
    driver_features_sel: Mutex<u32>,
    queue_sel: Mutex<u16>,
    queues: Mutex<Vec<VirtioQueue<T>>>,
    interrupt_status: Mutex<u32>,
    config_generation: Mutex<u32>,
    backend: B,
    accessor: Arc<T>,
}

impl<B: NetworkBackend, T: GuestMemoryAccessor + Clone> VirtioMmioNetDevice<B, T> {
    /// Create a network device covering `[base_ipa, base_ipa + length)`.
    pub fn new(
        base_ipa: GuestPhysAddr,
        length: usize,
        backend: B,
        net_config: VirtioNetConfig,
        accessor: T,
    ) -> VirtioResult<Self> {
        let accessor = Arc::new(accessor);
        let mut queues = Vec::with_capacity(NUM_QUEUES as usize);
        queues.push(VirtioQueue::new(
            RX_QUEUE_INDEX,
            vc::DEFAULT_QUEUE_SIZE,
            accessor.clone(),
        ));
        queues.push(VirtioQueue::new(
            TX_QUEUE_INDEX,
            vc::DEFAULT_QUEUE_SIZE,
            accessor.clone(),
        ));

        Ok(Self {
            base_ipa,
            length,
            mac: net_config.mac,
            mtu: net_config.mtu,
            link: Mutex::new(net_config.link),
            device_features: AXVIRTIO_NET_FEATURES,
            status: Mutex::new(0),
            driver_features: Mutex::new(0),
            device_features_sel: Mutex::new(0),
            driver_features_sel: Mutex::new(0),
            queue_sel: Mutex::new(0),
            queues: Mutex::new(queues),
            interrupt_status: Mutex::new(0),
            config_generation: Mutex::new(0),
            backend,
            accessor,
        })
    }

    /// Whether the driver has set `DRIVER_OK`.
    pub fn is_driver_ok(&self) -> bool {
        (*self.status.lock() & vc::VIRTIO_STATUS_DRIVER_OK) != 0
    }

    /// Current interrupt status bits.
    pub fn interrupt_status(&self) -> u32 {
        *self.interrupt_status.lock()
    }

    fn set_vring_interrupt(&self) {
        *self.interrupt_status.lock() |= vc::VIRTIO_MMIO_INT_VRING;
    }

    /// Build the 12-byte config-space image: mac | status | max_vq_pairs | mtu.
    fn config_image(&self) -> [u8; 12] {
        let mut img = [0u8; 12];
        img[0..6].copy_from_slice(&self.mac);
        let status = self.link.lock().status_bits();
        img[6..8].copy_from_slice(&status.to_le_bytes());
        img[8..10].copy_from_slice(&1u16.to_le_bytes()); // one RX/TX pair
        let mtu = self.mtu.unwrap_or(DEFAULT_MTU);
        img[10..12].copy_from_slice(&mtu.to_le_bytes());
        img
    }

    fn read_config_space(&self, offset: u64, width: AccessWidth) -> VirtioResult<usize> {
        let img = self.config_image();
        let off = offset as usize;
        let n = width.size();
        let Some(end) = off.checked_add(n) else {
            return Ok(0);
        };
        if end > img.len() {
            return Ok(0);
        }
        let mut value = 0usize;
        for i in 0..n {
            value |= (img[off + i] as usize) << (8 * i);
        }
        Ok(value)
    }

    /// Handle an MMIO read. Out-of-range and non-dword standard-register reads
    /// are handled per the transport helpers.
    pub fn mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> VirtioResult<usize> {
        let offset = transport::validate_read_access(addr, width, self.base_ipa, self.length)?;
        let value = match offset {
            vc::VIRTIO_MMIO_MAGIC_VALUE => vc::MMIO_MAGIC_VALUE,
            vc::VIRTIO_MMIO_VERSION => vc::MMIO_VERSION,
            vc::VIRTIO_MMIO_DEVICE_ID => axvirtio_common::VirtioDeviceID::Network.to_device_id(),
            vc::VIRTIO_MMIO_VENDOR_ID => vc::VIRTIO_VENDOR_ID,
            vc::VIRTIO_MMIO_DEVICE_FEATURES => {
                let sel = *self.device_features_sel.lock();
                (self.device_features >> ((sel as u64) * 32)) as u32
            }
            vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL => *self.device_features_sel.lock(),
            vc::VIRTIO_MMIO_DRIVER_FEATURES => {
                let sel = *self.driver_features_sel.lock();
                (*self.driver_features.lock() >> ((sel as u64) * 32)) as u32
            }
            vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL => *self.driver_features_sel.lock(),
            vc::VIRTIO_MMIO_QUEUE_SEL => *self.queue_sel.lock() as u32,
            vc::VIRTIO_MMIO_QUEUE_NUM_MAX => vc::DEFAULT_QUEUE_SIZE as u32,
            vc::VIRTIO_MMIO_QUEUE_NUM => {
                let sel = *self.queue_sel.lock();
                self.queues
                    .lock()
                    .get(sel as usize)
                    .map_or(0, |q| q.size as u32)
            }
            vc::VIRTIO_MMIO_QUEUE_READY => {
                let sel = *self.queue_sel.lock();
                self.queues
                    .lock()
                    .get(sel as usize)
                    .map_or(0, |q| if q.ready { 1 } else { 0 })
            }
            vc::VIRTIO_MMIO_INTERRUPT_STATUS => *self.interrupt_status.lock(),
            vc::VIRTIO_MMIO_STATUS => *self.status.lock(),
            vc::VIRTIO_MMIO_CONFIG_GENERATION => *self.config_generation.lock(),
            _ => {
                if offset >= vc::VIRTIO_MMIO_CONFIG_OFFSET {
                    return self
                        .read_config_space((offset - vc::VIRTIO_MMIO_CONFIG_OFFSET) as u64, width);
                }
                return Err(VirtioError::InvalidRegister);
            }
        };
        Ok(value as usize)
    }

    /// Handle an MMIO write and report the resulting event to the VMM.
    pub fn mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> VirtioResult<DeviceEvent> {
        let offset = transport::validate_write_access(addr, width, self.base_ipa, self.length)?;
        let val = val as u32;

        match offset {
            vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL => *self.device_features_sel.lock() = val,
            vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL => *self.driver_features_sel.lock() = val,
            vc::VIRTIO_MMIO_DRIVER_FEATURES => {
                let sel = *self.driver_features_sel.lock() as u64;
                let mask: u64 = (val as u64) << (sel * 32);
                let clear: u64 = !(((1u64) << 32) - 1).wrapping_shl((sel * 32) as u32);
                let mut f = self.driver_features.lock();
                *f = (*f & clear) | mask;
            }
            vc::VIRTIO_MMIO_QUEUE_SEL => {
                let sel = val as u16;
                if (sel as usize) < self.queues.lock().len() {
                    *self.queue_sel.lock() = sel;
                }
            }
            vc::VIRTIO_MMIO_QUEUE_NUM => {
                let sel = *self.queue_sel.lock();
                if let Some(q) = self.queues.lock().get_mut(sel as usize) {
                    let _ = q.set_size(val as u16);
                }
            }
            vc::VIRTIO_MMIO_QUEUE_READY => {
                let sel = *self.queue_sel.lock();
                if let Some(q) = self.queues.lock().get_mut(sel as usize) {
                    q.set_ready(val != 0);
                }
            }
            vc::VIRTIO_MMIO_QUEUE_NOTIFY => return self.handle_queue_notify(val as u16),
            vc::VIRTIO_MMIO_INTERRUPT_ACK => *self.interrupt_status.lock() &= !val,
            vc::VIRTIO_MMIO_STATUS => return self.handle_status_write(val),
            addr_reg @ (vc::VIRTIO_MMIO_QUEUE_DESC_LOW
            | vc::VIRTIO_MMIO_QUEUE_DESC_HIGH
            | vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW
            | vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH
            | vc::VIRTIO_MMIO_QUEUE_USED_LOW
            | vc::VIRTIO_MMIO_QUEUE_USED_HIGH) => {
                self.write_queue_address(addr_reg, val);
            }
            _ => return Err(VirtioError::InvalidRegister),
        }
        Ok(DeviceEvent::None)
    }

    /// Compose a 64-bit queue address from LOW/HIGH writes (overwrite semantics).
    fn write_queue_address(&self, reg: usize, val: u32) {
        let sel = *self.queue_sel.lock();
        let mut queues = self.queues.lock();
        let Some(q) = queues.get_mut(sel as usize) else {
            return;
        };
        match reg {
            vc::VIRTIO_MMIO_QUEUE_DESC_LOW => {
                let addr = combine_addr(q.desc_table_addr.as_usize(), val, /* low= */ true);
                let _ = q.set_desc_table_addr(GuestPhysAddr::from(addr));
            }
            vc::VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                let addr = combine_addr(q.desc_table_addr.as_usize(), val, false);
                let _ = q.set_desc_table_addr(GuestPhysAddr::from(addr));
            }
            vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW => {
                let addr = combine_addr(q.avail_ring_addr.as_usize(), val, true);
                let _ = q.set_avail_ring_addr(GuestPhysAddr::from(addr));
            }
            vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH => {
                let addr = combine_addr(q.avail_ring_addr.as_usize(), val, false);
                let _ = q.set_avail_ring_addr(GuestPhysAddr::from(addr));
            }
            vc::VIRTIO_MMIO_QUEUE_USED_LOW => {
                let addr = combine_addr(q.used_ring_addr.as_usize(), val, true);
                let _ = q.set_used_ring_addr(GuestPhysAddr::from(addr));
            }
            vc::VIRTIO_MMIO_QUEUE_USED_HIGH => {
                let addr = combine_addr(q.used_ring_addr.as_usize(), val, false);
                let _ = q.set_used_ring_addr(GuestPhysAddr::from(addr));
            }
            _ => {}
        }
    }

    fn handle_queue_notify(&self, queue_index: u16) -> VirtioResult<DeviceEvent> {
        if queue_index == TX_QUEUE_INDEX {
            // Only TX is driven by guest notifications; RX is host-driven.
            return Ok(self.handle_tx_notify());
        }
        Ok(DeviceEvent::None)
    }

    /// Drain all currently-visible TX requests on queue 1.
    ///
    /// Holds the per-device queue lock across the synchronous backend call; the
    /// backend must not re-enter the device (plan section 16.7).
    fn handle_tx_notify(&self) -> DeviceEvent {
        if !self.is_driver_ok() {
            return DeviceEvent::None;
        }
        let mut event = DeviceEvent::None;
        let mut queues = self.queues.lock();
        let Some(tx) = queues.get_mut(TX_QUEUE_INDEX as usize) else {
            return DeviceEvent::None;
        };
        if !tx.ready {
            return DeviceEvent::None;
        }

        // Bound the loop to the snapshot of pending requests (and the queue size).
        let avail_idx = tx.read_avail_idx().unwrap_or(0);
        let last = tx.get_last_avail_idx();
        let pending = avail_idx.wrapping_sub(last).min(tx.size);
        for _ in 0..pending {
            let head = match tx.pop_available_head() {
                Ok(Some(h)) => h,
                Ok(None) => break,
                Err(_) => break, // ring corruption; stop draining
            };
            let notify = match self.process_one_tx(tx, head) {
                Ok(n) => n,
                Err(_) => tx.complete(head, 0).unwrap_or(false),
            };
            if notify {
                event = DeviceEvent::InterruptPending;
            }
        }
        if let DeviceEvent::InterruptPending = event {
            self.set_vring_interrupt();
        }
        event
    }

    /// Process a single TX head. On success completes the chain and returns
    /// whether to notify; on error does not complete (caller completes len 0).
    fn process_one_tx(&self, tx: &mut VirtioQueue<T>, head: u16) -> Result<bool, NetError> {
        let chain = tx.descriptor_chain(head)?;

        // Aggregate all device-readable bytes (header + payload).
        let mut buf: Vec<u8> = Vec::new();
        for d in chain.readable() {
            let start = buf.len();
            buf.resize(start + d.len as usize, 0);
            self.accessor
                .read_buffer(d.base_addr, &mut buf[start..])
                .map_err(|_| NetError::GuestMemoryFault)?;
        }

        if buf.len() < VirtioNetHdr::SIZE {
            return Err(NetError::InvalidDescriptor);
        }
        let hdr = VirtioNetHdr::from_le_bytes(&buf).ok_or(NetError::InvalidDescriptor)?;
        if hdr.requests_offload() {
            return Err(NetError::UnsupportedOffload);
        }

        // Payload is everything after the header; the header is not transmitted.
        let frame = &buf[VirtioNetHdr::SIZE..];
        self.backend.transmit(frame)?;

        let notify = tx.complete(head, 0)?;
        Ok(notify)
    }

    /// Deliver a host RX frame into a guest-provided RX buffer (queue 0).
    ///
    /// Flow control: returns [`RxOutcome::NoGuestBuffer`] when the guest has not
    /// posted an RX buffer, leaving the ring unmodified. Capacity/descriptor
    /// problems are reported via [`NetError`] and also leave the ring modified
    /// only by consuming the offending head is avoided: validation happens before
    /// the head is consumed, so a too-small buffer or bad chain advances nothing.
    pub fn receive_frame(&self, frame: &[u8]) -> Result<RxOutcome, NetError> {
        if !self.is_driver_ok() {
            return Err(NetError::NotReady);
        }
        if *self.link.lock() == LinkStatus::Down {
            return Err(NetError::LinkDown);
        }
        if frame.len() > MAX_FRAME_SIZE {
            return Err(NetError::FrameTooLarge);
        }

        let needed = VirtioNetHdr::SIZE + frame.len();
        let mut queues = self.queues.lock();
        let Some(rx) = queues.get_mut(RX_QUEUE_INDEX as usize) else {
            return Err(NetError::NotReady);
        };
        if !rx.ready {
            return Err(NetError::NotReady);
        }

        // Peek before consuming so capacity/chain problems leave the ring intact.
        let last = rx.get_last_avail_idx();
        let avail_idx = rx.read_avail_idx().map_err(NetError::from)?;
        if avail_idx == last {
            return Ok(RxOutcome::NoGuestBuffer);
        }
        let head = rx
            .read_avail_entry(last % rx.size)
            .map_err(NetError::from)?;
        let chain = rx.descriptor_chain(head)?;

        // The whole chain must be device-writable for RX.
        if chain.readable().next().is_some() {
            return Err(NetError::InvalidDescriptor);
        }
        let capacity = chain.writable_len()?;
        if capacity < needed {
            return Err(NetError::FrameTooLarge);
        }

        // All checks passed: consume the head and write header + frame.
        rx.update_last_avail_idx(last.wrapping_add(1));
        self.write_rx_payload(&chain, frame)?;

        let notify = rx.complete(head, needed as u32).map_err(NetError::from)?;
        if notify {
            self.set_vring_interrupt();
        }
        Ok(RxOutcome::Delivered {
            frame_len: frame.len(),
        })
    }

    /// Write a zero `virtio_net_hdr` followed by `frame` across the chain's
    /// writable descriptors, in order.
    fn write_rx_payload(&self, chain: &DescriptorChain, frame: &[u8]) -> Result<(), NetError> {
        let mut output: Vec<u8> = Vec::with_capacity(VirtioNetHdr::SIZE + frame.len());
        output.resize(VirtioNetHdr::SIZE, 0); // zero header
        output.extend_from_slice(frame);

        let mut off = 0usize;
        for d in chain.writable() {
            if off >= output.len() {
                break;
            }
            let n = (output.len() - off).min(d.len as usize);
            self.accessor
                .write_buffer(d.base_addr, &output[off..off + n])
                .map_err(|_| NetError::GuestMemoryFault)?;
            off += n;
        }
        Ok(())
    }

    fn handle_status_write(&self, val: u32) -> VirtioResult<DeviceEvent> {
        if val == 0 {
            self.reset();
            return Ok(DeviceEvent::Reset);
        }
        let mut new_status = val;
        // Validate negotiated features when the driver seals them.
        if (new_status & vc::VIRTIO_STATUS_FEATURES_OK) != 0 {
            let driver_feats = *self.driver_features.lock();
            if (driver_feats & !self.device_features) != 0 {
                new_status &= !vc::VIRTIO_STATUS_FEATURES_OK;
                new_status |= vc::VIRTIO_STATUS_FAILED;
            }
        }
        *self.status.lock() = new_status;
        Ok(DeviceEvent::None)
    }

    /// Reset the device: clears driver features, selectors, interrupt status,
    /// queues (ready/addresses/indexes); keeps construction-time MAC/MTU and the
    /// advertised device features.
    pub fn reset(&self) {
        *self.driver_features.lock() = 0;
        *self.driver_features_sel.lock() = 0;
        *self.device_features_sel.lock() = 0;
        *self.queue_sel.lock() = 0;
        *self.interrupt_status.lock() = 0;
        *self.status.lock() = 0;
        for q in self.queues.lock().iter_mut() {
            q.reset();
        }
    }

    /// Change the link status. Bumps config generation and raises the
    /// config-change interrupt bit so a watching driver re-reads config space.
    pub fn set_link_status(&self, link: LinkStatus) -> DeviceEvent {
        *self.link.lock() = link;
        {
            let mut generation = self.config_generation.lock();
            *generation = generation.wrapping_add(1);
        }
        *self.interrupt_status.lock() |= vc::VIRTIO_MMIO_INT_CONFIG;
        DeviceEvent::InterruptPending
    }
}

/// Combine a 32-bit LOW/HIGH half with the current address into a 64-bit value.
fn combine_addr(current: usize, half: u32, low: bool) -> usize {
    let cur = current as u64;
    let h = half as u64;
    let combined = if low {
        (cur & 0xffff_ffff_0000_0000) | h
    } else {
        (cur & 0x0000_0000_ffff_ffff) | (h << 32)
    };
    combined as usize
}
