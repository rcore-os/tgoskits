//! Transport selection and ivshmem Message V1 support for `/dev/axivc`.
//!
//! The topology-free `/dev/axivc0` endpoint selects a uniquely discovered PCI
//! function and derives its role from the peer ID. A present
//! `axivc.link=<key>:<publisher-vm-id>` token remains a hard compatibility
//! boundary for the legacy manager path: malformed, duplicate or unavailable
//! attachments never fall back to HVC.
//!
//! The ivshmem backend rejects cache-maintenance ioctls with `ENOTTY`.
//! Userspace may map only the payload area after the page-aligned Message V1
//! region: the local owner output is writable and the peer output is read-only.
//! Protocol pages therefore never become userspace-accessible.

#[cfg(feature = "axivc-ivshmem")]
use alloc::sync::Arc;
#[cfg(feature = "axivc-ivshmem")]
use core::{mem::size_of, ops::Range, sync::atomic};

#[cfg(feature = "axivc-ivshmem")]
use ax_driver::pci::ivshmem::{IvshmemPciEndpoint, take_single_endpoint};
#[cfg(feature = "axivc-ivshmem")]
use ax_hal::mem::PhysAddr;
#[cfg(feature = "axivc-ivshmem")]
use axfs_ng_vfs::{VfsError, VfsResult};
#[cfg(feature = "axivc-ivshmem")]
use axivc::{
    IVC_SLOT_FRAGMENT_CAPACITY, IvcMessageError, IvcMessageReceiver, IvcMessageSender,
    IvshmemMessagePort, IvshmemSessionState,
};
#[cfg(feature = "axivc-ivshmem")]
use axpoll::IoEvents;

#[cfg(feature = "axivc-ivshmem")]
use crate::sync::Mutex;

const LINK_ARG: &str = "axivc.link";
const LINK_ARG_PREFIX: &str = "axivc.link=";
#[cfg(feature = "axivc-ivshmem")]
pub(super) const LEGACY_PUBLISH_SIZE: u64 = 0x1_0000;
pub(super) use super::ivc_legacy::IVC_SLOT_PAYLOAD_SIZE as APP_PAYLOAD_LIMIT;

#[cfg(feature = "axivc-ivshmem")]
const PAGE_SIZE: usize = 0x1000;
#[cfg(feature = "axivc-ivshmem")]
const STATE_TABLE_SIZE: usize = PAGE_SIZE;
#[cfg(feature = "axivc-ivshmem")]
const MESSAGE_REGION_SIZE: usize = 0x2100;
#[cfg(feature = "axivc-ivshmem")]
const PAYLOAD_OFFSET: usize = 0x3000;
#[cfg(feature = "axivc-ivshmem")]
const PEER_EPOCH_BASE: usize = 0x100;
#[cfg(feature = "axivc-ivshmem")]
const PEER_EPOCH_STRIDE: usize = size_of::<u64>();
/// Read-only peer-payload selector returned by `IVC_PAYLOAD_INFO`; it is an
/// mmap namespace value, not an address inside BAR2.
#[cfg(feature = "axivc-ivshmem")]
pub(super) const PAYLOAD_PEER_MMAP_OFFSET: u64 = 0x1000_0000;
#[cfg(feature = "axivc-ivshmem")]
const _: () = assert!(MESSAGE_REGION_SIZE <= PAYLOAD_OFFSET);
#[cfg(feature = "axivc-ivshmem")]
const MIN_SHARED_SIZE: usize = 0x1_0000;
#[cfg(feature = "axivc-ivshmem")]
const MAX_SHARED_SIZE: usize = 0x0200_0000;
#[cfg(feature = "axivc-ivshmem")]
const REQUIRED_PEERS: u32 = 2;
#[cfg(feature = "axivc-ivshmem")]
const PEER_ID_OFFSET: usize = 0x00;
#[cfg(feature = "axivc-ivshmem")]
const MAXIMUM_PEERS_OFFSET: usize = 0x04;
#[cfg(feature = "axivc-ivshmem")]
const INTERRUPT_CONTROL_OFFSET: usize = 0x08;
#[cfg(feature = "axivc-ivshmem")]
const DOORBELL_OFFSET: usize = 0x0c;
#[cfg(feature = "axivc-ivshmem")]
const EVENT_STATUS_OFFSET: usize = 0x14;
#[cfg(feature = "axivc-ivshmem")]
const COMMON_SIZE_OFFSET: usize = 0x18;
#[cfg(feature = "axivc-ivshmem")]
const OUTPUT_SIZE_OFFSET: usize = 0x1c;
#[cfg(feature = "axivc-ivshmem")]
const NOTIFICATIONS_ENABLE: u32 = 1;
#[cfg(feature = "axivc-ivshmem")]
const EVENT_VECTOR_0: u32 = 1;

/// Legacy manager transport selected from the kernel command line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AxivcLinkSelection {
    Legacy,
    Ivshmem(IvshmemLink),
    Invalid,
}

impl AxivcLinkSelection {
    pub(super) fn from_bootargs() -> Self {
        let Some(bootargs) = ax_runtime::hal::boot::bootargs() else {
            return Self::Legacy;
        };
        let mut selected = None;
        let mut invalid = false;
        for argument in bootargs.split_ascii_whitespace() {
            if argument == LINK_ARG {
                invalid = true;
                continue;
            }
            let Some(value) = argument.strip_prefix(LINK_ARG_PREFIX) else {
                continue;
            };
            if selected.is_some() {
                invalid = true;
                continue;
            }
            match IvshmemLink::parse(value) {
                Some(link) => selected = Some(link),
                None => invalid = true,
            }
        }
        if invalid {
            Self::Invalid
        } else {
            selected.map(Self::Ivshmem).unwrap_or(Self::Legacy)
        }
    }

    pub(super) const fn is_legacy(self) -> bool {
        matches!(self, Self::Legacy)
    }

    pub(super) const fn uses_ivshmem(self) -> bool {
        matches!(self, Self::Ivshmem(_))
    }
}

/// Exact application identity pinned by `axivc.link`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct IvshmemLink {
    key: u64,
    publisher_vm_id: u64,
}

impl IvshmemLink {
    fn parse(value: &str) -> Option<Self> {
        let (key, publisher_vm_id) = value.split_once(':')?;
        if !is_decimal(key) || !is_decimal(publisher_vm_id) {
            return None;
        }
        Some(Self {
            key: key.parse().ok()?,
            publisher_vm_id: publisher_vm_id.parse().ok()?,
        })
    }

    pub(super) const fn key(self) -> u64 {
        self.key
    }

    pub(super) const fn publisher_vm_id(self) -> u64 {
        self.publisher_vm_id
    }
}

fn is_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(feature = "axivc-ivshmem")]
#[derive(Clone, Copy)]
struct MappedRegion {
    mapped_base: usize,
    physical_base: usize,
    len: usize,
}

#[cfg(feature = "axivc-ivshmem")]
impl MappedRegion {
    fn device(range: Range<usize>) -> VfsResult<Self> {
        Self::map(range, true)
    }

    fn uncached(range: Range<usize>) -> VfsResult<Self> {
        Self::map(range, false)
    }

    fn map(range: Range<usize>, device: bool) -> VfsResult<Self> {
        let len = range
            .end
            .checked_sub(range.start)
            .filter(|len| *len != 0)
            .ok_or(VfsError::InvalidInput)?;
        if !range.start.is_multiple_of(PAGE_SIZE) {
            return Err(VfsError::InvalidInput);
        }
        let phys = PhysAddr::from_usize(range.start);
        let mapped = if device {
            ax_mm::iomap(phys, len)
        } else {
            ax_mm::iomap_uncached(phys, len)
        }
        .map_err(|_| VfsError::NoMemory)?;
        if mapped.as_usize() == 0 {
            return Err(VfsError::NoMemory);
        }
        Ok(Self {
            mapped_base: mapped.as_usize(),
            physical_base: range.start,
            len,
        })
    }

    fn checked_address(&self, offset: usize, width: usize) -> VfsResult<usize> {
        let end = offset
            .checked_add(width)
            .filter(|end| *end <= self.len)
            .ok_or(VfsError::InvalidInput)?;
        if !offset.is_multiple_of(width) {
            return Err(VfsError::InvalidInput);
        }
        self.mapped_base
            .checked_add(end - width)
            .ok_or(VfsError::InvalidInput)
    }

    fn read_u32(&self, offset: usize) -> VfsResult<u32> {
        let address = self.checked_address(offset, size_of::<u32>())?;
        // SAFETY: the offset was checked inside this mapping, and the mapping
        // remains installed for the lifetime of the registry. MMIO must use a
        // volatile access rather than an ordinary reference.
        Ok(unsafe { core::ptr::read_volatile(address as *const u32) })
    }

    fn write_u32(&self, offset: usize, value: u32) -> VfsResult<()> {
        let address = self.checked_address(offset, size_of::<u32>())?;
        // SAFETY: the offset was checked inside this mapping, and the mapping
        // remains installed for the lifetime of the registry.
        unsafe {
            core::ptr::write_volatile(address as *mut u32, value);
        }
        Ok(())
    }

    fn read_u64_acquire(&self, offset: usize) -> VfsResult<u64> {
        let address = self.checked_address(offset, size_of::<u64>())?;
        // SAFETY: `checked_address` proves the aligned eight-byte access is
        // inside the live Normal non-cacheable BAR2 mapping. The host publishes
        // this little-endian epoch with release semantics and never requires a
        // guest read-modify-write.
        let value =
            unsafe { (*(address as *const atomic::AtomicU64)).load(atomic::Ordering::Acquire) };
        Ok(u64::from_le(value))
    }

    fn take_events(&self) -> VfsResult<u32> {
        let pending = self.read_u32(EVENT_STATUS_OFFSET)? & EVENT_VECTOR_0;
        if pending != 0 {
            self.write_u32(EVENT_STATUS_OFFSET, pending)?;
        }
        Ok(pending)
    }

    fn enable_notifications(&self) -> VfsResult<()> {
        self.write_u32(INTERRUPT_CONTROL_OFFSET, NOTIFICATIONS_ENABLE)
    }

    fn disable_notifications(&self) -> VfsResult<()> {
        self.write_u32(INTERRUPT_CONTROL_OFFSET, 0)
    }

    fn ring_peer(&self, target: u32, vector: u16) -> VfsResult<()> {
        if target >= REQUIRED_PEERS {
            return Err(VfsError::InvalidInput);
        }
        // Publish all prior shared-memory writes before the device observes the
        // doorbell. The host-side sink treats the register write as one event.
        atomic::fence(atomic::Ordering::SeqCst);
        self.write_u32(DOORBELL_OFFSET, (target << 16) | u32::from(vector))
    }
}

/// One validated ivshmem function and its permanent kernel mappings.
///
/// BAR0 is mapped as Device memory. BAR2 is mapped Normal non-cacheable because
/// it is CPU-owned shared RAM consumed through the Message V1 atomics. Explicit
/// teardown rewrites only the local output section; the peer output section is
/// never writable by this endpoint.
#[cfg(feature = "axivc-ivshmem")]
pub(super) struct IvshmemAttachment {
    peer_id: u32,
    common_size: usize,
    output_size: usize,
    registers: MappedRegion,
    shared: MappedRegion,
    endpoint: Mutex<IvshmemPciEndpoint>,
}

#[cfg(feature = "axivc-ivshmem")]
#[derive(Clone, Copy)]
pub(super) struct IvshmemIrqRegisters(MappedRegion);

#[cfg(feature = "axivc-ivshmem")]
impl IvshmemIrqRegisters {
    pub(super) fn take_events(self) -> VfsResult<u32> {
        self.0.take_events()
    }
}

#[cfg(feature = "axivc-ivshmem")]
impl IvshmemAttachment {
    pub(super) fn probe() -> VfsResult<Self> {
        let endpoint = take_single_endpoint().map_err(|error| {
            warn!("axivc: ivshmem PCI attachment unavailable: {error:?}");
            VfsError::NoSuchDevice
        })?;
        let register_range = endpoint.registers();
        let shared_range = endpoint.shared();
        let registers = MappedRegion::device(register_range)?;
        let shared = MappedRegion::uncached(shared_range)?;
        if registers.len < OUTPUT_SIZE_OFFSET + size_of::<u32>() {
            return Err(VfsError::InvalidInput);
        }
        if !shared.len.is_multiple_of(PAGE_SIZE)
            || !(MIN_SHARED_SIZE..=MAX_SHARED_SIZE).contains(&shared.len)
            || shared.len < STATE_TABLE_SIZE
        {
            return Err(VfsError::InvalidInput);
        }

        let peer_id = registers.read_u32(PEER_ID_OFFSET)?;
        let max_peers = registers.read_u32(MAXIMUM_PEERS_OFFSET)?;
        let common_size = usize::try_from(registers.read_u32(COMMON_SIZE_OFFSET)?)
            .map_err(|_| VfsError::InvalidInput)?;
        let output_size = usize::try_from(registers.read_u32(OUTPUT_SIZE_OFFSET)?)
            .map_err(|_| VfsError::InvalidInput)?;
        if max_peers != REQUIRED_PEERS
            || peer_id >= REQUIRED_PEERS
            || !common_size.is_multiple_of(PAGE_SIZE)
            || output_size == 0
            || !output_size.is_multiple_of(PAGE_SIZE)
            || output_size < PAYLOAD_OFFSET + PAGE_SIZE
        {
            return Err(VfsError::InvalidInput);
        }
        let output0 = STATE_TABLE_SIZE
            .checked_add(common_size)
            .ok_or(VfsError::InvalidInput)?;
        let output0_end = output0
            .checked_add(output_size)
            .ok_or(VfsError::InvalidInput)?;
        let output1_end = output0_end
            .checked_add(output_size)
            .ok_or(VfsError::InvalidInput)?;
        if output1_end > shared.len {
            return Err(VfsError::InvalidInput);
        }

        Ok(Self {
            peer_id,
            common_size,
            output_size,
            registers,
            shared,
            endpoint: Mutex::new(endpoint),
        })
    }

    pub(super) const fn role_matches(&self, publisher: bool) -> bool {
        self.peer_id == if publisher { 0 } else { 1 }
    }

    pub(super) fn irq_binding(&self) -> VfsResult<ax_driver::BindingIrq> {
        let mut bindings = self.endpoint.lock().irq().irq_bindings();
        if bindings.len() != 1 {
            return Err(VfsError::NoSuchDevice);
        }
        Ok(bindings.remove(0).irq)
    }

    pub(super) fn enable_interrupts(&self) -> VfsResult<()> {
        self.registers.enable_notifications()
    }

    pub(super) fn disable_interrupts(&self) -> VfsResult<()> {
        self.registers.disable_notifications()
    }

    pub(super) const fn irq_registers(&self) -> IvshmemIrqRegisters {
        IvshmemIrqRegisters(self.registers)
    }

    pub(super) fn enable_irq(&self) {
        self.endpoint.lock().irq().enable();
    }

    pub(super) fn disable_irq(&self) {
        self.endpoint.lock().irq().disable();
    }

    fn output_offset(&self, peer: u32) -> VfsResult<usize> {
        if peer >= REQUIRED_PEERS {
            return Err(VfsError::InvalidInput);
        }
        let peer = usize::try_from(peer).map_err(|_| VfsError::InvalidInput)?;
        STATE_TABLE_SIZE
            .checked_add(self.common_size)
            .and_then(|offset| offset.checked_add(peer.checked_mul(self.output_size)?))
            .ok_or(VfsError::InvalidInput)
    }

    fn output_address(&self, peer: u32) -> VfsResult<usize> {
        let offset = self.output_offset(peer)?;
        self.shared
            .mapped_base
            .checked_add(offset)
            .ok_or(VfsError::InvalidInput)
    }

    fn output_phys_address(&self, peer: u32) -> VfsResult<usize> {
        let offset = self.output_offset(peer)?;
        self.shared
            .physical_base
            .checked_add(offset)
            .ok_or(VfsError::InvalidInput)
    }

    pub(super) fn payload_capacity(&self) -> usize {
        self.output_size - PAYLOAD_OFFSET
    }

    pub(super) fn payload_range(&self, peer: bool) -> VfsResult<Range<usize>> {
        let peer_id = if peer { self.peer_id ^ 1 } else { self.peer_id };
        let output = self.output_phys_address(peer_id)?;
        let start = output
            .checked_add(PAYLOAD_OFFSET)
            .ok_or(VfsError::InvalidInput)?;
        let end = output
            .checked_add(self.output_size)
            .ok_or(VfsError::InvalidInput)?;
        if end > self.shared.physical_base.saturating_add(self.shared.len) {
            return Err(VfsError::InvalidInput);
        }
        Ok(start..end)
    }

    pub(super) fn peer_epoch(&self) -> VfsResult<u64> {
        let peer = usize::try_from(self.peer_id ^ 1).map_err(|_| VfsError::InvalidInput)?;
        let offset = PEER_EPOCH_BASE
            .checked_add(
                peer.checked_mul(PEER_EPOCH_STRIDE)
                    .ok_or(VfsError::InvalidInput)?,
            )
            .ok_or(VfsError::InvalidInput)?;
        let epoch = self.shared.read_u64_acquire(offset)?;
        if epoch == 0 {
            return Err(VfsError::NoSuchDevice);
        }
        Ok(epoch)
    }

    fn ring_peer(&self) -> VfsResult<()> {
        let peer = self.peer_id ^ 1;
        self.registers.ring_peer(peer, 0)
    }

    fn open_port(&self, publisher: bool) -> VfsResult<IvshmemMessagePort<'static>> {
        if !self.role_matches(publisher) {
            return Err(VfsError::InvalidInput);
        }
        let local_peer = self.peer_id;
        let peer = local_peer ^ 1;
        let local = self.output_address(local_peer)?;
        let peer = self.output_address(peer)?;
        // SAFETY: both page-aligned output ranges were checked against the
        // live Normal non-cacheable BAR2 mapping, local protocol ownership is
        // serialized by the session mutex, and the peer range is passed
        // read-only. The kernel mapping is never unmapped while the registry
        // exists, satisfying the port's lifetime requirement.
        unsafe {
            IvshmemMessagePort::attach(
                local as *mut u8,
                self.output_size,
                peer as *const u8,
                self.output_size,
            )
        }
        .map_err(|error| {
            warn!("axivc: failed to attach ivshmem Message V1 port: {error:?}");
            VfsError::InvalidInput
        })
    }
}

#[cfg(feature = "axivc-ivshmem")]
impl Drop for IvshmemAttachment {
    fn drop(&mut self) {
        if let Err(error) = self.disable_interrupts() {
            warn!("axivc: failed to disable ivshmem notifications: {error:?}");
        }
    }
}

#[cfg(feature = "axivc-ivshmem")]
pub(super) struct IvshmemAdvance {
    pub(super) ready: bool,
    pub(super) lost: bool,
    pub(super) progressed: bool,
}

#[cfg(feature = "axivc-ivshmem")]
struct IoFailure {
    error: VfsError,
    fatal: bool,
}

#[cfg(feature = "axivc-ivshmem")]
impl IoFailure {
    fn transient(error: VfsError) -> Self {
        Self {
            error,
            fatal: false,
        }
    }

    fn fatal(error: VfsError) -> Self {
        Self { error, fatal: true }
    }
}

#[cfg(feature = "axivc-ivshmem")]
struct ReadOutcome {
    len: usize,
    consumed_slots: usize,
    more_messages: bool,
    frame: [u8; APP_PAYLOAD_LIMIT],
}

/// One role's Message V1 state, serialized by the registry state mutex.
#[cfg(feature = "axivc-ivshmem")]
pub(super) struct IvshmemChannelState {
    attachment: Arc<IvshmemAttachment>,
    peer_epoch: u64,
    port: Option<IvshmemMessagePort<'static>>,
    endpoints: Option<(IvcMessageSender<'static>, IvcMessageReceiver<'static>)>,
    previous_session: Option<IvshmemSessionState>,
    failed: bool,
}

#[cfg(feature = "axivc-ivshmem")]
impl IvshmemChannelState {
    pub(super) fn attach(attachment: Arc<IvshmemAttachment>, publisher: bool) -> VfsResult<Self> {
        let peer_epoch = attachment.peer_epoch()?;
        Self::attach_with_epoch(attachment, publisher, peer_epoch)
    }

    pub(super) fn attach_with_epoch(
        attachment: Arc<IvshmemAttachment>,
        publisher: bool,
        peer_epoch: u64,
    ) -> VfsResult<Self> {
        let mut port = attachment.open_port(publisher)?;
        // SAFETY: the session mutex makes this the sole initializer of the
        // local output section for this generation, and no old endpoint from
        // this role remains live.
        unsafe { port.begin_session() }.map_err(|error| {
            warn!("axivc: failed to begin ivshmem session: {error:?}");
            VfsError::Io
        })?;
        let mut state = Self {
            attachment,
            peer_epoch,
            port: Some(port),
            endpoints: None,
            previous_session: None,
            failed: false,
        };
        if state.advance().lost {
            let _ = state.invalidate();
            return Err(VfsError::Io);
        }
        Ok(state)
    }

    pub(super) fn epoch_valid(&mut self) -> bool {
        if self.failed {
            return false;
        }
        match self.attachment.peer_epoch() {
            Ok(epoch) if epoch == self.peer_epoch => true,
            Ok(epoch) => {
                warn!(
                    "axivc: ivshmem peer epoch changed from {:#x} to {:#x}; ending session",
                    self.peer_epoch, epoch
                );
                self.failed = true;
                false
            }
            Err(error) => {
                warn!("axivc: ivshmem peer epoch became unavailable: {error:?}");
                self.failed = true;
                false
            }
        }
    }

    pub(super) fn write(&mut self, input: &[u8]) -> VfsResult<(usize, IoEvents)> {
        let advance = self.advance();
        if advance.lost {
            return Err(VfsError::Io);
        }
        if !advance.ready {
            return Err(VfsError::WouldBlock);
        }
        let required_slots = input.len().div_ceil(IVC_SLOT_FRAGMENT_CAPACITY).max(1);
        let published = match self.endpoints.as_mut() {
            None => Err(IoFailure::fatal(VfsError::Io)),
            Some((sender, _)) => {
                if sender.available_slots() < required_slots {
                    Err(IoFailure::transient(VfsError::WouldBlock))
                } else {
                    match sender
                        .start_message(input.len() as u64)
                        .and_then(|_| sender.try_write(input))
                    {
                        Ok(progress)
                            if progress.is_complete() && progress.consumed() == input.len() =>
                        {
                            Ok(progress)
                        }
                        Ok(_) => {
                            let _ = sender.try_abort();
                            Err(IoFailure::transient(VfsError::WouldBlock))
                        }
                        Err(error) => Err(message_error(error)),
                    }
                }
            }
        };
        let progress = match published {
            Ok(progress) => progress,
            Err(failure) => {
                self.failed |= failure.fatal;
                return Err(failure.error);
            }
        };
        if !self.epoch_valid() {
            return Err(VfsError::Io);
        }
        if progress.published_slots() != 0
            && let Err(error) = self.attachment.ring_peer()
        {
            self.failed = true;
            return Err(error);
        }
        if !self.epoch_valid() {
            return Err(VfsError::Io);
        }
        let events = if advance.progressed {
            IoEvents::IN | IoEvents::OUT
        } else {
            IoEvents::empty()
        };
        Ok((input.len(), events))
    }

    pub(super) fn read(&mut self, output: &mut [u8]) -> VfsResult<(usize, IoEvents)> {
        let advance = self.advance();
        if advance.lost {
            return Err(VfsError::Io);
        }
        if !advance.ready {
            return Err(VfsError::WouldBlock);
        }
        let received = match self.endpoints.as_mut() {
            None => Err(IoFailure::fatal(VfsError::Io)),
            Some((_, receiver)) => (|| -> Result<ReadOutcome, IoFailure> {
                match receiver.complete_message_available() {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(IoFailure::transient(VfsError::WouldBlock));
                    }
                    Err(IvcMessageError::StreamingRequired) => {
                        return Err(IoFailure::fatal(VfsError::Io));
                    }
                    Err(error) => return Err(message_error(error)),
                }
                let meta = match receiver.peek_message_meta() {
                    Ok(Some(meta)) => meta,
                    Ok(None) => {
                        return Err(IoFailure::transient(VfsError::WouldBlock));
                    }
                    Err(error) => return Err(message_error(error)),
                };
                let len = match usize::try_from(meta.len()) {
                    Ok(len) => len,
                    Err(_) => {
                        return Err(IoFailure::transient(VfsError::InvalidInput));
                    }
                };
                if len > APP_PAYLOAD_LIMIT {
                    return Err(IoFailure::fatal(VfsError::Io));
                }
                if len > output.len() {
                    return Err(IoFailure::transient(VfsError::InvalidInput));
                }

                let mut frame = [0u8; APP_PAYLOAD_LIMIT];
                let progress = match receiver.try_read(&mut frame[..len]) {
                    Ok(progress) => progress,
                    Err(error) => return Err(message_error(error)),
                };
                if !progress.is_complete() || progress.written() != len {
                    return Err(IoFailure::fatal(VfsError::Io));
                }
                Ok(ReadOutcome {
                    len,
                    consumed_slots: progress.consumed_slots(),
                    more_messages: matches!(receiver.complete_message_available(), Ok(true)),
                    frame,
                })
            })(),
        };
        let ReadOutcome {
            len,
            consumed_slots,
            more_messages,
            frame,
        } = match received {
            Ok(received) => received,
            Err(failure) => {
                self.failed |= failure.fatal;
                return Err(failure.error);
            }
        };
        if !self.epoch_valid() {
            return Err(VfsError::Io);
        }
        output[..len].copy_from_slice(&frame[..len]);
        if consumed_slots != 0
            && let Err(error) = self.attachment.ring_peer()
        {
            self.failed = true;
            return Err(error);
        }
        if !self.epoch_valid() {
            return Err(VfsError::Io);
        }
        let mut events = if advance.progressed {
            IoEvents::IN | IoEvents::OUT
        } else {
            IoEvents::empty()
        };
        if more_messages {
            events |= IoEvents::IN;
        }
        Ok((len, events))
    }

    pub(super) fn ready_events(&mut self) -> IoEvents {
        let advance = self.advance();
        if advance.lost {
            return IoEvents::ERR | IoEvents::HUP;
        }
        if !advance.ready {
            return IoEvents::empty();
        }
        let Some((sender, receiver)) = self.endpoints.as_ref() else {
            self.failed = true;
            return IoEvents::ERR | IoEvents::HUP;
        };
        let receive = receiver.complete_message_available();
        let outbound = sender.available_slots() != 0;
        let mut events = IoEvents::empty();
        match receive {
            Ok(true) => events |= IoEvents::IN,
            Ok(false) => {}
            Err(_) => {
                self.failed = true;
                return IoEvents::ERR | IoEvents::HUP;
            }
        }
        if outbound {
            events |= IoEvents::OUT;
        }
        if !self.epoch_valid() {
            return IoEvents::ERR | IoEvents::HUP;
        }
        events
    }

    pub(super) fn invalidate(&mut self) -> VfsResult<()> {
        self.endpoints = None;
        self.failed = true;
        let Some(mut port) = self.port.take() else {
            return Ok(());
        };
        // SAFETY: all locally derived endpoints were dropped above, and the
        // registry state mutex excludes another role session from touching the
        // same local output. Peer endpoints can remain live and observe this
        // invalidation through their read-only owner-side atomic loads.
        unsafe { port.invalidate_session() };
        let result = self.attachment.ring_peer();
        if let Err(error) = &result {
            warn!("axivc: failed to invalidate ivshmem session during teardown: {error:?}");
        }
        result
    }

    pub(super) fn advance(&mut self) -> IvshmemAdvance {
        if self.failed {
            return IvshmemAdvance {
                ready: false,
                lost: true,
                progressed: false,
            };
        }
        if !self.epoch_valid() {
            return IvshmemAdvance {
                ready: false,
                lost: true,
                progressed: false,
            };
        }
        let Some(port) = self.port.as_mut() else {
            self.failed = true;
            return IvshmemAdvance {
                ready: false,
                lost: true,
                progressed: false,
            };
        };

        if self.endpoints.is_some() {
            let state = match port.session_state() {
                Ok(state @ IvshmemSessionState::Ready { .. }) => state,
                Ok(_) => {
                    self.failed = true;
                    return IvshmemAdvance {
                        ready: false,
                        lost: true,
                        progressed: false,
                    };
                }
                Err(error) => {
                    warn!("axivc: ivshmem session lost: {error:?}");
                    self.failed = true;
                    return IvshmemAdvance {
                        ready: false,
                        lost: true,
                        progressed: false,
                    };
                }
            };
            let progressed = self.previous_session != Some(state);
            self.previous_session = Some(state);
            if progressed && let Err(error) = self.attachment.ring_peer() {
                warn!("axivc: failed to ring peer after session refresh: {error:?}");
                self.failed = true;
                return IvshmemAdvance {
                    ready: false,
                    lost: true,
                    progressed,
                };
            }
            return IvshmemAdvance {
                ready: true,
                lost: false,
                progressed,
            };
        }

        let next = port.poll_handshake();
        if let Ok(next_state) = next
            && peer_session_regressed(self.previous_session, next_state)
        {
            self.failed = true;
            return IvshmemAdvance {
                ready: false,
                lost: true,
                progressed: false,
            };
        }

        match next {
            Ok(state @ IvshmemSessionState::Ready { .. }) => {
                let progressed = self.previous_session != Some(state);
                self.previous_session = Some(state);
                if self.endpoints.is_none() {
                    let endpoints = match port.try_connect() {
                        Ok(endpoints) => endpoints.into_parts(),
                        Err(error) => {
                            warn!("axivc: failed to connect ivshmem session: {error:?}");
                            self.failed = true;
                            return IvshmemAdvance {
                                ready: false,
                                lost: true,
                                progressed,
                            };
                        }
                    };
                    self.endpoints = Some(endpoints);
                }
                if progressed && let Err(error) = self.attachment.ring_peer() {
                    warn!("axivc: failed to ring peer after handshake: {error:?}");
                    self.failed = true;
                    return IvshmemAdvance {
                        ready: false,
                        lost: true,
                        progressed,
                    };
                }
                IvshmemAdvance {
                    ready: true,
                    lost: false,
                    progressed,
                }
            }
            Ok(
                state @ (IvshmemSessionState::WaitingForPeer { .. }
                | IvshmemSessionState::WaitingForPeerAcknowledgement { .. }),
            ) => {
                let progressed = self.previous_session != Some(state);
                self.previous_session = Some(state);
                if progressed && let Err(error) = self.attachment.ring_peer() {
                    warn!("axivc: failed to ring peer during handshake: {error:?}");
                    self.failed = true;
                    return IvshmemAdvance {
                        ready: false,
                        lost: true,
                        progressed,
                    };
                }
                IvshmemAdvance {
                    ready: false,
                    lost: false,
                    progressed,
                }
            }
            Ok(_) => {
                self.failed = true;
                IvshmemAdvance {
                    ready: false,
                    lost: true,
                    progressed: false,
                }
            }
            Err(error) => {
                warn!("axivc: ivshmem session lost: {error:?}");
                self.failed = true;
                IvshmemAdvance {
                    ready: false,
                    lost: true,
                    progressed: false,
                }
            }
        }
    }
}

#[cfg(feature = "axivc-ivshmem")]
fn message_error(error: IvcMessageError) -> IoFailure {
    match error {
        IvcMessageError::SlotFull => IoFailure::transient(VfsError::WouldBlock),
        IvcMessageError::SessionLost => IoFailure::fatal(VfsError::Io),
        _ => {
            warn!("axivc: ivshmem Message V1 error: {error:?}");
            IoFailure::fatal(VfsError::Io)
        }
    }
}

#[cfg(feature = "axivc-ivshmem")]
fn peer_session_regressed(
    previous: Option<IvshmemSessionState>,
    next: IvshmemSessionState,
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    let IvshmemSessionState::WaitingForPeerAcknowledgement {
        peer: previous_peer,
        ..
    } = previous
    else {
        return false;
    };
    match next {
        IvshmemSessionState::WaitingForPeer { .. } => true,
        IvshmemSessionState::WaitingForPeerAcknowledgement { peer, .. } => peer != previous_peer,
        _ => false,
    }
}
