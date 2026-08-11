//! VirtIO MMIO network device.

use alloc::{sync::Arc, vec::Vec};

use ax_sync::SpinLock as Mutex;
use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    DescriptorChain, MmioReadOutcome, MmioWriteAction, VirtioMmioState, VirtioQueue, VirtioResult,
    constants as vc,
};
use axvm_types::{AccessWidth, GuestPhysAddr};
use log::warn;

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
    Delivered {
        frame_len: usize,
        /// Whether the used-ring update requires a guest interrupt.
        notify: bool,
    },
    /// No guest RX buffer was available; the VMM decides whether to
    /// cache/retry/drop. This is flow control, not an error.
    NoGuestBuffer,
}

/// A VirtIO 1.x MMIO network device with one RX/TX queue pair.
///
/// `B` is the host transmit backend; `T` translates guest physical addresses.
/// Standard MMIO register handling is delegated to [`VirtioMmioState`]; this
/// type owns only the net-specific config space and RX/TX data paths.
pub struct VirtioMmioNetDevice<B: NetworkBackend, T: GuestMemoryAccessor + Clone> {
    state: VirtioMmioState<T>,
    mac: [u8; 6],
    mtu: Option<u16>,
    link: Mutex<LinkStatus>,
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
        let state = VirtioMmioState::new(
            base_ipa,
            length,
            axvirtio_common::VirtioDeviceID::Network.to_device_id(),
            vc::VIRTIO_VENDOR_ID,
            AXVIRTIO_NET_FEATURES,
            queues,
        );

        Ok(Self {
            state,
            mac: net_config.mac,
            mtu: net_config.mtu,
            link: Mutex::new(net_config.link),
            backend,
            accessor,
        })
    }

    /// Whether the driver has set `DRIVER_OK`.
    pub fn is_driver_ok(&self) -> bool {
        self.state.is_driver_ok()
    }

    /// Current interrupt status bits.
    pub fn interrupt_status(&self) -> u32 {
        self.state.interrupt_status()
    }

    /// Build the 12-byte config-space image: mac | status | max_vq_pairs | mtu.
    fn config_image(&self) -> [u8; 12] {
        let mut img = [0u8; 12];
        img[0..6].copy_from_slice(&self.mac);
        let status = self.link.lock_irqsave().status_bits();
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

    /// Handle an MMIO read. Standard registers are served by the shared state;
    /// the device-specific config region is interpreted here.
    pub fn mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> VirtioResult<usize> {
        match self.state.mmio_read(addr, width)? {
            MmioReadOutcome::Standard(v) => Ok(v as usize),
            MmioReadOutcome::DeviceConfig { offset, width } => {
                self.read_config_space(offset, width)
            }
        }
    }

    /// Handle an MMIO write and report the resulting event to the VMM.
    pub fn mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> VirtioResult<DeviceEvent> {
        let mut memory = axvirtio_common::AddressSpaceMemory::new(&*self.accessor);
        self.mmio_write_with_memory(addr, width, val, &mut memory)
    }

    /// Handles an MMIO write using a guest-memory capability scoped to this
    /// device access.
    pub fn mmio_write_with_memory(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<DeviceEvent> {
        match self.state.mmio_write(addr, width, val)? {
            MmioWriteAction::None => Ok(DeviceEvent::None),
            MmioWriteAction::Reset => Ok(DeviceEvent::Reset),
            MmioWriteAction::InterruptPending => Ok(DeviceEvent::InterruptPending),
            MmioWriteAction::QueueNotified(idx) => {
                if idx == TX_QUEUE_INDEX {
                    Ok(self.handle_tx_notify(memory))
                } else if idx == RX_QUEUE_INDEX {
                    self.backend.rx_queue_notified();
                    Ok(DeviceEvent::None)
                } else {
                    Ok(DeviceEvent::None)
                }
            }
        }
    }

    /// Drain all currently-visible TX requests on queue 1.
    ///
    /// Holds the per-device queue lock across the synchronous backend call; the
    /// backend must not re-enter the device (plan section 16.7).
    fn handle_tx_notify(&self, memory: &mut dyn axvirtio_common::GuestMemory) -> DeviceEvent {
        if !self.state.is_driver_ok() {
            return DeviceEvent::None;
        }
        let mut event = DeviceEvent::None;
        let mut queues = self.state.queues_lock();
        let Some(tx) = queues.get_mut(TX_QUEUE_INDEX as usize) else {
            return DeviceEvent::None;
        };
        if !tx.ready {
            return DeviceEvent::None;
        }

        let avail_idx = tx.read_avail_idx_with_memory(memory).unwrap_or(0);
        let last = tx.get_last_avail_idx();
        let pending = avail_idx.wrapping_sub(last).min(tx.size);
        for _ in 0..pending {
            let head = match tx.pop_available_head_with_memory(memory) {
                Ok(Some(h)) => h,
                Ok(None) => break,
                Err(_) => break, // ring corruption; stop draining
            };
            let notify = match self.process_one_tx(tx, head, memory) {
                Ok(notify) => notify,
                Err(error) => {
                    warn!("virtio-net failed to process TX descriptor {head}: {error:?}");
                    tx.complete_with_memory(head, 0, memory).unwrap_or(false)
                }
            };
            if notify {
                event = DeviceEvent::InterruptPending;
            }
        }
        if event == DeviceEvent::InterruptPending {
            self.state.set_interrupt(vc::VIRTIO_MMIO_INT_VRING);
        }
        event
    }

    /// Process a single TX head. On success completes the chain and returns
    /// whether to notify; on error does not complete (caller completes len 0).
    fn process_one_tx(
        &self,
        tx: &mut axvirtio_common::VirtioQueue<T>,
        head: u16,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> Result<bool, NetError> {
        let chain = tx.descriptor_chain_with_memory(head, memory)?;

        // Aggregate all device-readable bytes (header + payload).
        let readable_len = chain.readable_len()?;
        let header_len = self.header_len();
        if readable_len < header_len || readable_len - header_len > MAX_FRAME_SIZE {
            return Err(NetError::FrameTooLarge);
        }
        let mut buf = Vec::new();
        buf.try_reserve_exact(readable_len)
            .map_err(|_| NetError::FrameTooLarge)?;
        for d in chain.readable() {
            let start = buf.len();
            buf.resize(
                start
                    .checked_add(d.len as usize)
                    .ok_or(NetError::FrameTooLarge)?,
                0,
            );
            memory
                .read(d.base_addr, &mut buf[start..])
                .map_err(|_| NetError::GuestMemoryFault)?;
        }

        let hdr = VirtioNetHdr::from_le_bytes(&buf).ok_or(NetError::InvalidDescriptor)?;
        if hdr.requests_offload() {
            return Err(NetError::UnsupportedOffload);
        }

        // Payload is everything after the header; the header is not transmitted.
        let frame = &buf[header_len..];
        self.backend.transmit(frame)?;

        let notify = tx.complete_with_memory(head, 0, memory)?;
        Ok(notify)
    }

    /// Deliver a host RX frame into a guest-provided RX buffer (queue 0).
    pub fn receive_frame(&self, frame: &[u8]) -> Result<RxOutcome, NetError> {
        let mut memory = axvirtio_common::AddressSpaceMemory::new(&*self.accessor);
        self.receive_frame_with_memory(frame, &mut memory)
    }

    /// Delivers a host frame using a scoped guest-memory capability.
    pub fn receive_frame_with_memory(
        &self,
        frame: &[u8],
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> Result<RxOutcome, NetError> {
        if !self.state.is_driver_ok() {
            return Err(NetError::NotReady);
        }
        if *self.link.lock_irqsave() == LinkStatus::Down {
            return Err(NetError::LinkDown);
        }
        if frame.len() > MAX_FRAME_SIZE {
            return Err(NetError::FrameTooLarge);
        }

        let header_len = self.header_len();
        let needed = header_len + frame.len();
        let mut queues = self.state.queues_lock();
        let Some(rx) = queues.get_mut(RX_QUEUE_INDEX as usize) else {
            return Err(NetError::NotReady);
        };
        if !rx.ready {
            return Err(NetError::NotReady);
        }

        // Peek before consuming so capacity/chain problems leave the ring intact.
        let last = rx.get_last_avail_idx();
        let avail_idx = rx
            .read_avail_idx_with_memory(memory)
            .map_err(NetError::from)?;
        if avail_idx == last {
            return Ok(RxOutcome::NoGuestBuffer);
        }
        let head = rx
            .read_avail_entry_with_memory(last % rx.size, memory)
            .map_err(NetError::from)?;
        let chain = rx.descriptor_chain_with_memory(head, memory)?;

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
        self.write_rx_payload(&chain, header_len, frame, memory)?;

        let notify = rx
            .complete_with_memory(head, needed as u32, memory)
            .map_err(NetError::from)?;
        if notify {
            self.state.set_interrupt(vc::VIRTIO_MMIO_INT_VRING);
        }
        Ok(RxOutcome::Delivered {
            frame_len: frame.len(),
            notify,
        })
    }

    /// Write a zero `virtio_net_hdr` followed by `frame` across the chain's
    /// writable descriptors, in order.
    fn write_rx_payload(
        &self,
        chain: &DescriptorChain,
        header_len: usize,
        frame: &[u8],
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> Result<(), NetError> {
        let mut output: Vec<u8> = Vec::with_capacity(header_len + frame.len());
        output.resize(header_len, 0); // zero header
        output.extend_from_slice(frame);

        let mut off = 0usize;
        for d in chain.writable() {
            if off >= output.len() {
                break;
            }
            let n = (output.len() - off).min(d.len as usize);
            memory
                .write(d.base_addr, &output[off..off + n])
                .map_err(|_| NetError::GuestMemoryFault)?;
            off += n;
        }
        Ok(())
    }

    fn header_len(&self) -> usize {
        if self.state.driver_features() & vc::VIRTIO_F_VERSION_1 != 0 {
            VIRTIO_NET_HDR_MODERN_SIZE
        } else {
            VirtioNetHdr::SIZE
        }
    }

    /// Reset the device (clears transport state and queues; keeps MAC/MTU and
    /// advertised features).
    pub fn reset(&self) {
        self.state.reset();
    }

    /// Change the link status. Bumps config generation and raises the
    /// config-change interrupt bit so a watching driver re-reads config space.
    pub fn set_link_status(&self, link: LinkStatus) -> DeviceEvent {
        *self.link.lock_irqsave() = link;
        self.state.bump_config_generation();
        self.state.set_interrupt(vc::VIRTIO_MMIO_INT_CONFIG);
        DeviceEvent::InterruptPending
    }
}
