//! Shared VirtIO MMIO transport state machine.
//!
//! Device implementations (block, net, ...) own only their device-specific
//! config space and data paths; the standard MMIO register set — magic,
//! version, feature selectors, driver features, queue selector/size/ready,
//! queue address LOW/HIGH, status, interrupt status/ack, config generation —
//! is handled here so it is not duplicated per device.

use alloc::vec::Vec;

use ax_sync::{SpinLock as Mutex, SpinLockIrqSaveGuard as MutexGuard};
use axaddrspace::GuestMemoryAccessor;
use axvm_types::{AccessWidth, GuestPhysAddr};

use crate::{VirtioQueue, VirtioResult, constants as vc, error::VirtioError, mmio::transport};

/// Result of a standard-register MMIO read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioReadOutcome {
    /// A standard register value.
    Standard(u32),
    /// A read inside the device-specific config region; the device interprets it.
    DeviceConfig { offset: u64, width: AccessWidth },
}

/// Side effect an MMIO write asks the device driver (block/net) to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioWriteAction {
    /// Nothing for the device to do.
    None,
    /// The guest kicked a queue; the device runs its data path.
    QueueNotified(u16),
    /// The guest wrote status 0; the device is fully reset.
    Reset,
    /// An acknowledged interrupt bit was raised again after the guest's last
    /// interrupt-status read and must be signalled again.
    InterruptPending,
}

#[derive(Default)]
struct InterruptState {
    pending: u32,
    raised_after_read: u32,
}

/// Shared VirtIO MMIO transport state plus the device's queues.
///
/// `device_id`, `vendor_id` and `device_features` are fixed at construction.
/// Feature negotiation is validated here (`driver_features` must be a subset of
/// `device_features` when the driver seals `FEATURES_OK`).
pub struct VirtioMmioState<T: GuestMemoryAccessor + Clone> {
    base_ipa: GuestPhysAddr,
    length: usize,
    device_id: u32,
    vendor_id: u32,
    device_features: u64,
    status: Mutex<u32>,
    driver_features: Mutex<u64>,
    features_sealed: Mutex<bool>,
    device_features_sel: Mutex<u32>,
    driver_features_sel: Mutex<u32>,
    queue_sel: Mutex<u16>,
    /// Serializes queue configuration register writes without blocking the data path.
    queue_config_transaction: Mutex<()>,
    queues: Mutex<Vec<VirtioQueue<T>>>,
    interrupt_status: Mutex<InterruptState>,
    config_generation: Mutex<u32>,
}

impl<T: GuestMemoryAccessor + Clone> VirtioMmioState<T> {
    /// Construct the transport state with the given device identity, advertised
    /// features and pre-created queues.
    pub fn new(
        base_ipa: GuestPhysAddr,
        length: usize,
        device_id: u32,
        vendor_id: u32,
        device_features: u64,
        queues: Vec<VirtioQueue<T>>,
    ) -> Self {
        Self {
            base_ipa,
            length,
            device_id,
            vendor_id,
            device_features,
            status: Mutex::new(0),
            driver_features: Mutex::new(0),
            features_sealed: Mutex::new(false),
            device_features_sel: Mutex::new(0),
            driver_features_sel: Mutex::new(0),
            queue_sel: Mutex::new(0),
            queue_config_transaction: Mutex::new(()),
            queues: Mutex::new(queues),
            interrupt_status: Mutex::new(InterruptState::default()),
            config_generation: Mutex::new(0),
        }
    }

    /// Base IPA of the MMIO region.
    pub fn base_ipa(&self) -> GuestPhysAddr {
        self.base_ipa
    }

    /// Number of queues.
    pub fn num_queues(&self) -> usize {
        self.queues.lock_irqsave().len()
    }

    /// Lock the queue vector for a device data path.
    pub fn queues_lock(&self) -> MutexGuard<'_, Vec<VirtioQueue<T>>> {
        self.queues.lock_irqsave()
    }

    /// Whether the driver has set `DRIVER_OK`.
    pub fn is_driver_ok(&self) -> bool {
        (*self.status.lock_irqsave() & vc::VIRTIO_STATUS_DRIVER_OK) != 0
    }

    /// Raw status register value.
    pub fn status(&self) -> u32 {
        *self.status.lock_irqsave()
    }

    /// Set the status register directly, bypassing validation.
    ///
    /// Intended only for device bring-up helpers that emulate the full driver
    /// sequence; normal status transitions must go through [`mmio_write`](Self::mmio_write).
    pub fn set_status(&self, status: u32) {
        *self.status.lock_irqsave() = status;
    }

    /// The currently selected queue index, if it is in range.
    pub fn selected_queue_index(&self) -> Option<u16> {
        let sel = *self.queue_sel.lock_irqsave();
        if (sel as usize) < self.queues.lock_irqsave().len() {
            Some(sel)
        } else {
            None
        }
    }

    /// Currently negotiated driver features.
    pub fn driver_features(&self) -> u64 {
        *self.driver_features.lock_irqsave()
    }

    /// Advertised device features.
    pub fn device_features(&self) -> u64 {
        self.device_features
    }

    /// Current interrupt status bits.
    pub fn interrupt_status(&self) -> u32 {
        self.interrupt_status.lock_irqsave().pending
    }

    /// OR interrupt bits in (used-ring or config-change notification).
    pub fn set_interrupt(&self, bits: u32) {
        let mut interrupt = self.interrupt_status.lock_irqsave();
        interrupt.pending |= bits;
        interrupt.raised_after_read |= bits;
    }

    /// Increment the config-space generation (call after changing config).
    pub fn bump_config_generation(&self) {
        let mut g = self.config_generation.lock_irqsave();
        *g = g.wrapping_add(1);
    }

    /// Full transport reset: clears driver features, selectors, interrupt
    /// status, status and every queue. Device identity and features are kept.
    pub fn reset(&self) {
        let _queue_config_guard = self.queue_config_transaction.lock();
        let mut features_sealed = self.features_sealed.lock_irqsave();
        *self.driver_features.lock_irqsave() = 0;
        *self.driver_features_sel.lock_irqsave() = 0;
        *self.device_features_sel.lock_irqsave() = 0;
        *self.queue_sel.lock_irqsave() = 0;
        *self.interrupt_status.lock_irqsave() = InterruptState::default();
        *self.status.lock_irqsave() = 0;
        for q in self.queues.lock_irqsave().iter_mut() {
            q.reset();
        }
        *features_sealed = false;
    }

    /// Handle a standard MMIO read. Out-of-range reads yield `Standard(0)`;
    /// reads inside the config region yield [`MmioReadOutcome::DeviceConfig`].
    pub fn mmio_read(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
    ) -> VirtioResult<MmioReadOutcome> {
        if !transport::is_address_in_range(addr, self.base_ipa, self.length) {
            return Ok(MmioReadOutcome::Standard(0));
        }
        let offset = transport::calculate_offset(addr, self.base_ipa);
        if offset < vc::VIRTIO_MMIO_CONFIG_OFFSET {
            transport::validate_access_width(width)?;
        }

        let value = match offset {
            vc::VIRTIO_MMIO_MAGIC_VALUE => vc::MMIO_MAGIC_VALUE,
            vc::VIRTIO_MMIO_VERSION => vc::MMIO_VERSION,
            vc::VIRTIO_MMIO_DEVICE_ID => self.device_id,
            vc::VIRTIO_MMIO_VENDOR_ID => self.vendor_id,
            vc::VIRTIO_MMIO_DEVICE_FEATURES => {
                let sel = *self.device_features_sel.lock_irqsave();
                if sel >= 2 {
                    0
                } else {
                    (self.device_features >> ((sel as u64) * 32)) as u32
                }
            }
            vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL => *self.device_features_sel.lock_irqsave(),
            vc::VIRTIO_MMIO_DRIVER_FEATURES => {
                let sel = *self.driver_features_sel.lock_irqsave();
                if sel >= 2 {
                    0
                } else {
                    (*self.driver_features.lock_irqsave() >> ((sel as u64) * 32)) as u32
                }
            }
            vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL => *self.driver_features_sel.lock_irqsave(),
            vc::VIRTIO_MMIO_QUEUE_SEL => *self.queue_sel.lock_irqsave() as u32,
            vc::VIRTIO_MMIO_QUEUE_NUM_MAX => vc::DEFAULT_QUEUE_SIZE as u32,
            vc::VIRTIO_MMIO_QUEUE_NUM => {
                let sel = *self.queue_sel.lock_irqsave();
                self.queues
                    .lock_irqsave()
                    .get(sel as usize)
                    .map_or(0, |q| q.size as u32)
            }
            vc::VIRTIO_MMIO_QUEUE_READY => {
                let sel = *self.queue_sel.lock_irqsave();
                self.queues
                    .lock_irqsave()
                    .get(sel as usize)
                    .map_or(0, |q| if q.ready { 1 } else { 0 })
            }
            vc::VIRTIO_MMIO_INTERRUPT_STATUS => {
                let mut interrupt = self.interrupt_status.lock_irqsave();
                let pending = interrupt.pending;
                interrupt.raised_after_read = 0;
                pending
            }
            vc::VIRTIO_MMIO_STATUS => *self.status.lock_irqsave(),
            vc::VIRTIO_MMIO_CONFIG_GENERATION => *self.config_generation.lock_irqsave(),
            _ => {
                if offset >= vc::VIRTIO_MMIO_CONFIG_OFFSET {
                    return Ok(MmioReadOutcome::DeviceConfig {
                        offset: (offset - vc::VIRTIO_MMIO_CONFIG_OFFSET) as u64,
                        width,
                    });
                }
                return Err(VirtioError::InvalidRegister);
            }
        };
        Ok(MmioReadOutcome::Standard(value))
    }

    /// Handle a standard MMIO write and report any action the device must take.
    ///
    /// The `QUEUE_READY` layout validation is screened against the selected
    /// queue's own guest-memory accessor. Runtimes whose real guest memory
    /// only exists as a scoped capability at MMIO access time must use
    /// [`mmio_write_with_memory`](Self::mmio_write_with_memory) instead.
    pub fn mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> VirtioResult<MmioWriteAction> {
        self.mmio_write_inner(addr, width, val, None)
    }

    /// Handles a standard MMIO write using a scoped guest-memory capability.
    ///
    /// The capability is used for the `QUEUE_READY` layout validation and
    /// must be backed by the same guest memory the queues' runtime accesses
    /// use; passing a capability over different memory makes the layout
    /// check vacuous.
    pub fn mmio_write_with_memory(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<MmioWriteAction> {
        self.mmio_write_inner(addr, width, val, Some(memory))
    }

    /// Shared MMIO write implementation.
    ///
    /// `ready_memory` is the scoped capability used to screen the selected
    /// queue's ring layout on a `QUEUE_READY` write; `None` falls back to a
    /// capability built from the queue's own accessor.
    fn mmio_write_inner(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        ready_memory: Option<&mut dyn crate::GuestMemory>,
    ) -> VirtioResult<MmioWriteAction> {
        if !transport::is_address_in_range(addr, self.base_ipa, self.length) {
            return Ok(MmioWriteAction::None);
        }
        let offset = transport::calculate_offset(addr, self.base_ipa);
        if offset < vc::VIRTIO_MMIO_CONFIG_OFFSET {
            transport::validate_access_width(width)?;
        }
        let val = val as u32;
        let _queue_config_guard =
            is_queue_config_register(offset).then(|| self.queue_config_transaction.lock());

        match offset {
            vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL => *self.device_features_sel.lock_irqsave() = val,
            vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL => *self.driver_features_sel.lock_irqsave() = val,
            vc::VIRTIO_MMIO_DRIVER_FEATURES => {
                let features_sealed = self.features_sealed.lock_irqsave();
                let sel = *self.driver_features_sel.lock_irqsave() as u64;
                if !*features_sealed && sel < 2 {
                    let mask: u64 = (val as u64) << (sel * 32);
                    let clear: u64 = !(((1u64) << 32) - 1).wrapping_shl((sel * 32) as u32);
                    let mut f = self.driver_features.lock_irqsave();
                    *f = (*f & clear) | mask;
                }
            }
            vc::VIRTIO_MMIO_QUEUE_SEL => {
                let sel = val as u16;
                if (sel as usize) < self.queues.lock_irqsave().len() {
                    *self.queue_sel.lock_irqsave() = sel;
                }
            }
            vc::VIRTIO_MMIO_QUEUE_NUM => {
                let sel = *self.queue_sel.lock_irqsave();
                if let Some(q) = self.queues.lock_irqsave().get_mut(sel as usize) {
                    let _ = q.set_size(val as u16);
                }
            }
            vc::VIRTIO_MMIO_QUEUE_READY => {
                let sel = *self.queue_sel.lock_irqsave();
                if val == 0 {
                    if let Some(queue) = self.queues.lock_irqsave().get_mut(sel as usize) {
                        queue.cancel_ready_preparation();
                    }
                    return Ok(MmioWriteAction::None);
                }
                let mut candidate = self
                    .queues
                    .lock_irqsave()
                    .get_mut(sel as usize)
                    .and_then(VirtioQueue::begin_ready_preparation);
                let prepared = if let Some(queue) = candidate.as_mut() {
                    let result = match ready_memory {
                        Some(memory) => queue.validate_layout_with_memory(memory).and_then(|()| {
                            queue.set_ready(true);
                            queue.rearm_available_event_with_memory(memory).map(|_| ())
                        }),
                        None => {
                            let accessor = queue.accessor().clone();
                            let mut memory = crate::AddressSpaceMemory::new(&*accessor);
                            queue
                                .validate_layout_with_memory(&mut memory)
                                .and_then(|()| {
                                    queue.set_ready(true);
                                    queue
                                        .rearm_available_event_with_memory(&mut memory)
                                        .map(|_| ())
                                })
                        }
                    };
                    result.is_ok()
                } else {
                    false
                };
                if let Some(snapshot) = candidate.as_ref()
                    && let Some(queue) = self.queues.lock_irqsave().get_mut(sel as usize)
                {
                    queue.finish_ready_preparation(snapshot, prepared);
                }
            }
            vc::VIRTIO_MMIO_QUEUE_NOTIFY => return Ok(MmioWriteAction::QueueNotified(val as u16)),
            vc::VIRTIO_MMIO_INTERRUPT_ACK => {
                let mut interrupt = self.interrupt_status.lock_irqsave();
                let raised_after_read = interrupt.raised_after_read & val;
                interrupt.pending &= !(val & !raised_after_read);
                if raised_after_read != 0 {
                    return Ok(MmioWriteAction::InterruptPending);
                }
            }
            vc::VIRTIO_MMIO_STATUS => return self.handle_status_write(val),
            reg @ (vc::VIRTIO_MMIO_QUEUE_DESC_LOW
            | vc::VIRTIO_MMIO_QUEUE_DESC_HIGH
            | vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW
            | vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH
            | vc::VIRTIO_MMIO_QUEUE_USED_LOW
            | vc::VIRTIO_MMIO_QUEUE_USED_HIGH) => self.write_queue_address(reg, val),
            _ => return Err(VirtioError::InvalidRegister),
        }
        Ok(MmioWriteAction::None)
    }

    /// Validate a status write. Writing 0 resets; sealing `FEATURES_OK` is
    /// rejected unless driver features are a subset of device features.
    fn handle_status_write(&self, val: u32) -> VirtioResult<MmioWriteAction> {
        if val == 0 {
            self.reset();
            return Ok(MmioWriteAction::Reset);
        }
        let mut features_sealed = self.features_sealed.lock_irqsave();
        let features_already_ok = *features_sealed;
        let mut new_status = if features_already_ok {
            val | vc::VIRTIO_STATUS_FEATURES_OK
        } else {
            val
        };
        if !features_already_ok && (new_status & vc::VIRTIO_STATUS_FEATURES_OK) != 0 {
            let driver_feats = *self.driver_features.lock_irqsave();
            if (driver_feats & !self.device_features) != 0 {
                new_status &= !vc::VIRTIO_STATUS_FEATURES_OK;
                new_status |= vc::VIRTIO_STATUS_FAILED;
            } else {
                let event_idx_enabled = driver_feats & vc::VIRTIO_F_RING_EVENT_IDX != 0;
                for queue in self.queues.lock_irqsave().iter_mut() {
                    queue.event_idx_enabled = event_idx_enabled;
                }
                *features_sealed = true;
            }
        }
        *self.status.lock_irqsave() = new_status;
        Ok(MmioWriteAction::None)
    }

    /// Combine a 32-bit LOW/HIGH half into a queue address (overwrite semantics).
    fn write_queue_address(&self, reg: usize, val: u32) {
        let sel = *self.queue_sel.lock_irqsave();
        let mut queues = self.queues.lock_irqsave();
        let Some(q) = queues.get_mut(sel as usize) else {
            return;
        };
        match reg {
            vc::VIRTIO_MMIO_QUEUE_DESC_LOW => {
                let _ = q.set_desc_table_addr(GuestPhysAddr::from(combine_addr(
                    q.desc_table_addr.as_usize(),
                    val,
                    true,
                )));
            }
            vc::VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                let _ = q.set_desc_table_addr(GuestPhysAddr::from(combine_addr(
                    q.desc_table_addr.as_usize(),
                    val,
                    false,
                )));
            }
            vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW => {
                let _ = q.set_avail_ring_addr(GuestPhysAddr::from(combine_addr(
                    q.avail_ring_addr.as_usize(),
                    val,
                    true,
                )));
            }
            vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH => {
                let _ = q.set_avail_ring_addr(GuestPhysAddr::from(combine_addr(
                    q.avail_ring_addr.as_usize(),
                    val,
                    false,
                )));
            }
            vc::VIRTIO_MMIO_QUEUE_USED_LOW => {
                let _ = q.set_used_ring_addr(GuestPhysAddr::from(combine_addr(
                    q.used_ring_addr.as_usize(),
                    val,
                    true,
                )));
            }
            vc::VIRTIO_MMIO_QUEUE_USED_HIGH => {
                let _ = q.set_used_ring_addr(GuestPhysAddr::from(combine_addr(
                    q.used_ring_addr.as_usize(),
                    val,
                    false,
                )));
            }
            _ => {}
        }
    }
}

const fn is_queue_config_register(offset: usize) -> bool {
    matches!(
        offset,
        vc::VIRTIO_MMIO_QUEUE_SEL
            | vc::VIRTIO_MMIO_QUEUE_NUM
            | vc::VIRTIO_MMIO_QUEUE_READY
            | vc::VIRTIO_MMIO_QUEUE_DESC_LOW
            | vc::VIRTIO_MMIO_QUEUE_DESC_HIGH
            | vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW
            | vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH
            | vc::VIRTIO_MMIO_QUEUE_USED_LOW
            | vc::VIRTIO_MMIO_QUEUE_USED_HIGH
    )
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
