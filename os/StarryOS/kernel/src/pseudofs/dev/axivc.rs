#[cfg(feature = "axivc-ivshmem")]
use alloc::sync::Weak;
use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_hal::mem::{DCacheOp, PhysAddr, VirtAddr, dcache_range, virt_to_phys};
use ax_lazyinit::OnceLock;
use ax_memory_addr::PhysAddrRange;
use ax_runtime::hal::irq::{
    AutoEnable, IrqError, IrqHandle, IrqId, IrqRequest, IrqReturn, ShareMode,
};
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsError, VfsResult};
use axhvc::ivc::{self, IvcGuestPhysAddr};
use axpoll::{IoEvents, Pollable, SharedRegistrationSink};
use axpoll_set::PollSet;
use bytemuck::{AnyBitPattern, NoUninit};

#[cfg(feature = "axivc-ivshmem")]
use super::axivc_ivshmem::LEGACY_PUBLISH_SIZE;
#[cfg(feature = "axivc-ivshmem")]
use super::axivc_ivshmem::{IvshmemAttachment, IvshmemChannelState, PAYLOAD_PEER_MMAP_OFFSET};
use super::{
    axivc_ivshmem::{APP_PAYLOAD_LIMIT, AxivcLinkSelection, IvshmemLink},
    ivc_legacy::{self as legacy_ivc, IvcConsumer, IvcMessageKind, IvcProducer, IvcRegion},
};
#[cfg(feature = "axivc-ivshmem")]
use crate::pseudofs::DeviceMmapAccess;
use crate::{
    mm::{VmMutPtr, VmPtr},
    pseudofs::{Device, DeviceMmap, DeviceOps, DirMapping, SimpleFs},
    sync::Mutex,
    task::future::IrqNotify,
};

const MAX_CHANNELS: usize = 16;
const AXIVC_MANAGER_MINOR: u32 = 240;
const AXIVC_PUBLISHER_MINOR_BASE: u32 = 241;
const AXIVC_SUBSCRIBER_MINOR_BASE: u32 = 257;
const REGION_READY_RETRIES: usize = 100;
const REGION_READY_DELAY_MS: u64 = 10;
const AXIVC_COMPATIBLES: &[&str] = &["axvisor,ivc-channel"];

const IVC_PUBLISH_CHANNEL: u32 = 0x4050_0000;
const IVC_UNPUBLISH_CHANNEL: u32 = 0x4050_0001;
const IVC_SUBSCRIBE_CHANNEL: u32 = 0x4050_0002;
const IVC_UNSUBSCRIBE_CHANNEL: u32 = 0x4050_0003;
const IVC_CACHE_FLUSH: u32 = 0x4010_0004;
const IVC_CACHE_INVALIDATE: u32 = 0x4010_0005;
#[cfg(feature = "axivc-ivshmem")]
// _IOR('I', 6, struct IvcPayloadInfo), whose stable payload is five u64 fields.
const IVC_PAYLOAD_INFO: u32 = 0x8028_4906;
#[cfg(feature = "axivc-ivshmem")]
const PAYLOAD_INFO_VERSION: u64 = 1;

#[cfg(feature = "axivc-ivshmem")]
const AXIVC_AUTO_MINOR: u32 = 239;
// Private registry keys for the single automatic endpoint. They are not part
// of the userspace ABI or matched against `axivc.link`; peer ID selects role.
#[cfg(feature = "axivc-ivshmem")]
const AUTO_CHANNEL_KEY: u64 = 1;
#[cfg(feature = "axivc-ivshmem")]
const AUTO_PUBLISHER_ID: usize = 1;

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, NoUninit)]
struct IvcPublishArg {
    channel_key: u64,
    channel_size: u64,
    device_name: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, NoUninit)]
struct IvcSubscribeArg {
    target_publisher_id: u64,
    channel_key: u64,
    device_name: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, NoUninit)]
struct IvcCacheOpArg {
    addr: u64,
    size: u64,
}

#[cfg(feature = "axivc-ivshmem")]
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, NoUninit)]
struct IvcPayloadInfo {
    version: u64,
    generation: u64,
    local_offset: u64,
    peer_offset: u64,
    payload_size: u64,
}
#[cfg(feature = "axivc-ivshmem")]
const _: () = assert!(core::mem::size_of::<IvcPayloadInfo>() == 0x28);

struct AxivcRegistry {
    transport: AxivcLinkSelection,
    #[cfg(feature = "axivc-ivshmem")]
    session_transition: Mutex<()>,
    inner: Mutex<RegistryInner>,
    publisher_poll_sets: Vec<Arc<PollSet>>,
    subscriber_poll_sets: Vec<Arc<PollSet>>,
    notify_irq: Option<IrqId>,
    irq_handle: OnceLock<IrqHandle>,
    #[cfg(feature = "axivc-ivshmem")]
    ivshmem_irq_handle: OnceLock<IrqHandle>,
    deferred_notify: Arc<IrqNotify>,
    notify_service_started: AtomicBool,
    #[cfg(feature = "axivc-ivshmem")]
    ivshmem_event_logged: AtomicBool,
    #[cfg(feature = "axivc-ivshmem")]
    ivshmem_attachment: Mutex<IvshmemAttachmentState>,
}

#[cfg(feature = "axivc-ivshmem")]
enum IvshmemAttachmentState {
    Unattempted,
    Ready(Arc<IvshmemAttachment>),
    Failed,
}

#[cfg(feature = "axivc-ivshmem")]
struct AutoPayloadLease {
    attachment: Arc<IvshmemAttachment>,
    generation: u64,
    peer_epoch: u64,
}

#[cfg(feature = "axivc-ivshmem")]
impl AutoPayloadLease {
    fn map(self: &Arc<Self>, peer: bool) -> VfsResult<DeviceMmap> {
        if self.attachment.peer_epoch()? != self.peer_epoch {
            return Err(VfsError::Io);
        }
        let range = self.attachment.payload_range(peer)?;
        let capacity = self.attachment.payload_capacity();
        let start = PhysAddr::from_usize(range.start);
        let range = PhysAddrRange::from_start_size(start, capacity);
        let retain: Arc<dyn Any + Send + Sync> = self.clone();
        if peer {
            Ok(DeviceMmap::PhysicalResolvedRestricted(
                range,
                retain,
                DeviceMmapAccess::ReadOnly,
            ))
        } else {
            Ok(DeviceMmap::PhysicalResolvedRestricted(
                range,
                retain,
                DeviceMmapAccess::ReadWrite,
            ))
        }
    }
}

#[cfg(feature = "axivc-ivshmem")]
#[derive(Clone)]
struct AutoChannel {
    role: ChannelRole,
    index: usize,
    lease: Arc<AutoPayloadLease>,
}

#[cfg(feature = "axivc-ivshmem")]
struct AutoEndpointState {
    channel: Option<AutoChannel>,
    retired: Vec<Weak<AutoPayloadLease>>,
    next_generation: u64,
}

#[cfg(feature = "axivc-ivshmem")]
struct AxivcAutoEndpoint {
    registry: Arc<AxivcRegistry>,
    state: Mutex<AutoEndpointState>,
}

impl AxivcRegistry {
    fn new(channel_count: usize, transport: AxivcLinkSelection) -> Self {
        let notify_irq = transport.is_legacy().then(probe_notify_irq).flatten();
        Self::new_with_notify_irq(channel_count, transport, notify_irq)
    }

    fn new_with_notify_irq(
        channel_count: usize,
        transport: AxivcLinkSelection,
        notify_irq: Option<IrqId>,
    ) -> Self {
        Self {
            transport,
            #[cfg(feature = "axivc-ivshmem")]
            session_transition: Mutex::new(()),
            inner: Mutex::new(RegistryInner::new(channel_count)),
            publisher_poll_sets: new_poll_sets(channel_count),
            subscriber_poll_sets: new_poll_sets(channel_count),
            notify_irq,
            irq_handle: OnceLock::new(),
            #[cfg(feature = "axivc-ivshmem")]
            ivshmem_irq_handle: OnceLock::new(),
            deferred_notify: Arc::new(IrqNotify::new()),
            notify_service_started: AtomicBool::new(false),
            #[cfg(feature = "axivc-ivshmem")]
            ivshmem_event_logged: AtomicBool::new(false),
            #[cfg(feature = "axivc-ivshmem")]
            ivshmem_attachment: Mutex::new(IvshmemAttachmentState::Unattempted),
        }
    }

    fn register_notify_irq(self: &Arc<Self>) {
        let Some(irq) = self.notify_irq else {
            debug!(
                "axivc: axvisor,ivc-channel notify IRQ not found; blocking poll waits need \
                 peer-side polling"
            );
            return;
        };

        let notify = Arc::clone(&self.deferred_notify);
        let request = IrqRequest::new(move |_| {
            notify.notify_irq();
            IrqReturn::Wake
        })
        .share_mode(ShareMode::Shared)
        .auto_enable(AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(irq, request) {
            Ok(handle) => {
                if !self.start_notify_service() {
                    let _ = ax_runtime::hal::irq::free_irq(handle);
                    return;
                }
                self.irq_handle.call_once(|| handle);
                if let Some(handle) = self.irq_handle.get().copied()
                    && let Err(err) = ax_runtime::hal::irq::enable_irq(handle)
                {
                    warn!("axivc: failed to enable notify IRQ {irq:?}: {err:?}");
                }
            }
            Err(err) => {
                warn!("axivc: failed to register notify IRQ {irq:?}: {err:?}");
            }
        }
    }

    fn start_notify_service(self: &Arc<Self>) -> bool {
        if self
            .notify_service_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return true;
        }
        let registry = Arc::downgrade(self);
        let notify = Arc::clone(&self.deferred_notify);
        match crate::task::kernel_thread_builder("axivc-notify-service".into()).spawn(move || {
            loop {
                notify.wait();
                let Some(registry) = registry.upgrade() else {
                    break;
                };
                #[cfg(feature = "axivc-ivshmem")]
                let ivshmem_active = registry.transport.uses_ivshmem()
                    || matches!(
                        &*registry.ivshmem_attachment.lock(),
                        IvshmemAttachmentState::Ready(_)
                    );
                #[cfg(feature = "axivc-ivshmem")]
                if ivshmem_active {
                    if !registry.ivshmem_event_logged.swap(true, Ordering::AcqRel) {
                        info!("axivc ivshmem MSI-X event");
                    }
                    if registry.advance_ivshmem_sessions() {
                        continue;
                    }
                }
                registry.wake_channel_pollers();
            }
        }) {
            Ok(_service) => true,
            Err(error) => {
                self.notify_service_started.store(false, Ordering::Release);
                warn!("axivc: failed to start notify service: {error}");
                false
            }
        }
    }

    #[cfg(all(test, not(axtest)))]
    fn handle_notify_irq(&self) -> IrqReturn {
        // The notify IRQ reports peer progress but does not carry a channel id.
        // Hard IRQ only publishes a coalesced event. The fixed service thread
        // performs PollSet fan-out in task context, where wakers may run.
        self.deferred_notify.notify_irq();
        IrqReturn::Wake
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn ivshmem_attachment(self: &Arc<Self>) -> VfsResult<Arc<IvshmemAttachment>> {
        let mut slot = self.ivshmem_attachment.lock();
        match &*slot {
            IvshmemAttachmentState::Ready(attachment) => return Ok(Arc::clone(attachment)),
            IvshmemAttachmentState::Failed => return Err(VfsError::NoSuchDevice),
            IvshmemAttachmentState::Unattempted => {}
        }

        let attachment = match IvshmemAttachment::probe() {
            Ok(attachment) => Arc::new(attachment),
            Err(error) => {
                *slot = IvshmemAttachmentState::Failed;
                return Err(error);
            }
        };
        if !self.start_notify_service() {
            *slot = IvshmemAttachmentState::Failed;
            return Err(VfsError::NoMemory);
        }
        let irq = match attachment.irq_binding().and_then(|binding| {
            ax_runtime::irq::resolve_binding_irq(binding).map_err(|error| {
                warn!("axivc: failed to resolve ivshmem MSI-X IRQ: {error:?}");
                VfsError::NoSuchDevice
            })
        }) {
            Ok(irq) => irq,
            Err(error) => {
                *slot = IvshmemAttachmentState::Failed;
                return Err(error);
            }
        };

        let irq_registers = attachment.irq_registers();
        let notify = Arc::clone(&self.deferred_notify);
        let request = IrqRequest::new(move |_| {
            match irq_registers.take_events() {
                Ok(0) | Err(_) => return IrqReturn::Unhandled,
                Ok(_) => {}
            }
            notify.notify_irq();
            IrqReturn::Wake
        })
        .share_mode(ShareMode::Shared)
        .auto_enable(AutoEnable::No);
        let handle = match ax_runtime::hal::irq::request_irq(irq, request) {
            Ok(handle) => handle,
            Err(error) => {
                warn!("axivc: failed to register ivshmem MSI-X IRQ {irq:?}: {error:?}");
                *slot = IvshmemAttachmentState::Failed;
                return Err(VfsError::NoSuchDevice);
            }
        };
        if let Err(error) = ax_runtime::hal::irq::enable_irq(handle) {
            warn!("axivc: failed to enable ivshmem MSI-X IRQ {irq:?}: {error:?}");
            let _ = ax_runtime::hal::irq::free_irq(handle);
            *slot = IvshmemAttachmentState::Failed;
            return Err(VfsError::NoSuchDevice);
        }

        // The lease stays masked until the hard handler and its deferred
        // service are active. Only then may the endpoint inject MSI-X.
        attachment.enable_irq();
        if let Err(error) = attachment.enable_interrupts() {
            attachment.disable_irq();
            let _ = ax_runtime::hal::irq::disable_irq(handle);
            let _ = ax_runtime::hal::irq::free_irq(handle);
            *slot = IvshmemAttachmentState::Failed;
            return Err(error);
        }

        self.ivshmem_irq_handle.call_once(|| handle);
        *slot = IvshmemAttachmentState::Ready(Arc::clone(&attachment));
        Ok(attachment)
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn advance_ivshmem_sessions(&self) -> bool {
        let mut inner = self.inner.lock();
        let mut terminal = false;
        let mut terminal_poll_set = None;
        let RegistryInner {
            publishers,
            subscribers,
        } = &mut *inner;
        for state in publishers
            .iter_mut()
            .chain(subscribers.iter_mut())
            .flatten()
        {
            if let ChannelBackend::Ivshmem(session) = &mut state.backend {
                let advance = session.advance();
                terminal |= advance.lost;
                if advance.lost {
                    terminal_poll_set.get_or_insert_with(|| state.poll_set.clone());
                }
            }
        }
        drop(inner);
        if let Some(poll_set) = terminal_poll_set {
            // SAFETY: the terminal state was published before the registry lock
            // was released, and this runs in the deferred task-context service.
            // `wake_all` is required for unconditional POLLERR/POLLHUP delivery.
            unsafe { poll_set.wake_all(IoEvents::ERR | IoEvents::HUP) };
        }
        terminal
    }

    fn wake_channel_pollers(&self) -> usize {
        self.publisher_poll_sets
            .iter()
            .chain(self.subscriber_poll_sets.iter())
            .map(|poll_set| wake_pollers(poll_set, IoEvents::IN | IoEvents::OUT))
            .sum()
    }

    fn poll_set(&self, role: ChannelRole, index: usize) -> Option<Arc<PollSet>> {
        match role {
            ChannelRole::Publisher => self.publisher_poll_sets.get(index),
            ChannelRole::Subscriber => self.subscriber_poll_sets.get(index),
        }
        .cloned()
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn provision_auto(self: &Arc<Self>, generation: u64) -> VfsResult<AutoChannel> {
        let _transition = self.session_transition.lock();
        let attachment = self.ivshmem_attachment()?;
        let role = if attachment.role_matches(true) {
            ChannelRole::Publisher
        } else if attachment.role_matches(false) {
            ChannelRole::Subscriber
        } else {
            return Err(VfsError::PermissionDenied);
        };
        let mut inner = self.inner.lock();
        let (index, poll_set) = match role {
            ChannelRole::Publisher => {
                if inner.publisher_has_ivshmem() || inner.publisher_exists(AUTO_CHANNEL_KEY) {
                    return Err(VfsError::ResourceBusy);
                }
                let index = inner.free_publisher_index().ok_or(VfsError::NoMemory)?;
                let poll_set = self.poll_set(role, index).ok_or(VfsError::NoMemory)?;
                (index, poll_set)
            }
            ChannelRole::Subscriber => {
                if inner.subscriber_has_ivshmem()
                    || inner.subscriber_exists(AUTO_PUBLISHER_ID, AUTO_CHANNEL_KEY)
                {
                    return Err(VfsError::ResourceBusy);
                }
                let index = inner.free_subscriber_index().ok_or(VfsError::NoMemory)?;
                let poll_set = self.poll_set(role, index).ok_or(VfsError::NoMemory)?;
                (index, poll_set)
            }
        };
        let peer_epoch = attachment.peer_epoch()?;
        let session = IvshmemChannelState::attach_with_epoch(
            Arc::clone(&attachment),
            role == ChannelRole::Publisher,
            peer_epoch,
        )?;
        let lease = Arc::new(AutoPayloadLease {
            attachment,
            generation,
            peer_epoch,
        });
        let state = ChannelState {
            publisher_id: AUTO_PUBLISHER_ID,
            key: AUTO_CHANNEL_KEY,
            fd_refs: 0,
            poll_set,
            closing: false,
            visible: true,
            backend: ChannelBackend::Ivshmem(session),
        };
        match role {
            ChannelRole::Publisher => inner.insert_publisher_at(index, state),
            ChannelRole::Subscriber => inner.insert_subscriber_at(index, state),
        }
        Ok(AutoChannel { role, index, lease })
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn release_auto(&self, channel: &AutoChannel) -> VfsResult<()> {
        let _transition = self.session_transition.lock();
        let mut inner = self.inner.lock();
        let state = match channel.role {
            ChannelRole::Publisher => inner.remove_publisher(AUTO_CHANNEL_KEY),
            ChannelRole::Subscriber => inner.remove_subscriber(AUTO_PUBLISHER_ID, AUTO_CHANNEL_KEY),
        }
        .ok_or(VfsError::NoSuchDevice)?;
        let poll_set = state.poll_set.clone();
        drop(inner);
        wake_pollers(&poll_set, IoEvents::ERR | IoEvents::HUP);
        let mut state = state;
        state.invalidate_ivshmem()
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn auto_channel_epoch_valid(&self, channel: &AutoChannel) -> bool {
        let (valid, poll_set) = {
            let mut inner = self.inner.lock();
            let Some(state) = inner.channel_mut(channel.role, channel.index) else {
                return false;
            };
            let poll_set = state.poll_set.clone();
            let valid = match &mut state.backend {
                ChannelBackend::Ivshmem(session) => session.epoch_valid(),
                ChannelBackend::Legacy(_) => false,
            };
            (valid, poll_set)
        };
        if !valid {
            // SAFETY: the terminal session state is published while the
            // registry lock is held; wake outside the lock.
            unsafe { poll_set.wake_all(IoEvents::ERR | IoEvents::HUP) };
        }
        valid
    }

    fn publish(self: &Arc<Self>, arg: &mut IvcPublishArg) -> VfsResult<()> {
        match self.transport {
            AxivcLinkSelection::Legacy => self.publish_legacy(arg),
            AxivcLinkSelection::Ivshmem(link) => self.publish_ivshmem(arg, link),
            AxivcLinkSelection::Invalid => Err(VfsError::InvalidInput),
        }
    }

    fn publish_legacy(&self, arg: &mut IvcPublishArg) -> VfsResult<()> {
        let shm_base_gpa = HyperCallOutputSlot::new(0)?;
        let shm_size = HyperCallOutputSlot::new(arg.channel_size as usize)?;

        ivc::publish_channel(
            arg.channel_key as usize,
            shm_base_gpa.guest_phys_addr(),
            shm_size.guest_phys_addr(),
        )
        .map_err(hvc_error)?;

        let shm_base_gpa = shm_base_gpa.read();
        let shm_size = shm_size.read();
        if shm_size < core::mem::size_of::<IvcRegion>() {
            let _ = ivc::unpublish_channel(arg.channel_key as usize);
            return Err(VfsError::InvalidInput);
        }

        let region = match shared_page_mut(shm_base_gpa, shm_size) {
            Ok(region) => region,
            Err(err) => {
                let _ = ivc::unpublish_channel(arg.channel_key as usize);
                return Err(err);
            }
        };
        let publisher_id = region.publisher_id();
        region.initialize();
        let region: &'static IvcRegion = region;
        let (producer, consumer) = unsafe { region.publisher_endpoints() }.into_parts();

        let mut inner = self.inner.lock();
        if inner.publisher_exists(arg.channel_key) {
            let _ = ivc::unpublish_channel(arg.channel_key as usize);
            return Err(VfsError::InvalidInput);
        }
        let Some(index) = inner.free_publisher_index() else {
            let _ = ivc::unpublish_channel(arg.channel_key as usize);
            return Err(VfsError::NoMemory);
        };
        let Some(poll_set) = self.poll_set(ChannelRole::Publisher, index) else {
            let _ = ivc::unpublish_channel(arg.channel_key as usize);
            return Err(VfsError::NoMemory);
        };
        inner.insert_publisher_at(
            index,
            ChannelState {
                publisher_id,
                key: arg.channel_key,
                fd_refs: 0,
                poll_set,
                closing: false,
                visible: false,
                backend: ChannelBackend::Legacy(LegacyChannelState {
                    notify_target_vm_id: Some(ivc::IVC_NOTIFY_PEER),
                    shm_base_gpa,
                    shm_size,
                    mmap_anchor: Arc::new(()),
                    producer,
                    consumer,
                    sequence: AtomicU64::new(1),
                }),
            },
        );
        write_device_name(
            &mut arg.device_name,
            &channel_device_path(ChannelRole::Publisher, index),
        );
        info!(
            "axivc: published key={:#x} slot={} base={:#x} size={:#x}",
            arg.channel_key, index, shm_base_gpa, shm_size
        );
        Ok(())
    }

    fn unpublish(&self, arg: &IvcPublishArg) -> VfsResult<()> {
        match self.transport {
            AxivcLinkSelection::Legacy => self.unpublish_legacy(arg, false),
            AxivcLinkSelection::Ivshmem(_) => self.unpublish_ivshmem(arg, false),
            AxivcLinkSelection::Invalid => Err(VfsError::InvalidInput),
        }
    }

    fn rollback_publish(&self, arg: &IvcPublishArg) -> VfsResult<()> {
        match self.transport {
            AxivcLinkSelection::Legacy => self.unpublish_legacy(arg, true),
            AxivcLinkSelection::Ivshmem(_) => self.unpublish_ivshmem(arg, true),
            AxivcLinkSelection::Invalid => Err(VfsError::InvalidInput),
        }
    }

    fn unpublish_legacy(&self, arg: &IvcPublishArg, rollback: bool) -> VfsResult<()> {
        let mut inner = self.inner.lock();
        let Some(state) = inner.publisher_mut(arg.channel_key) else {
            return Err(VfsError::NoSuchDevice);
        };
        if !rollback && !state.visible {
            return Err(VfsError::WouldBlock);
        }
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        if state.has_mmap_refs() {
            return Err(VfsError::WouldBlock);
        }
        if state.fd_refs != 0 {
            return Err(VfsError::WouldBlock);
        }
        state.closing = true;
        let poll_set = state.poll_set.clone();
        drop(inner);
        wake_pollers(&poll_set, IoEvents::ERR | IoEvents::HUP);

        let result = ivc::unpublish_channel(arg.channel_key as usize).map_err(hvc_error);
        match result {
            Ok(()) => {
                self.inner.lock().remove_publisher(arg.channel_key);
                Ok(())
            }
            Err(err) => {
                if let Some(state) = self.inner.lock().publisher_mut(arg.channel_key) {
                    state.closing = false;
                }
                Err(err)
            }
        }
    }

    fn subscribe(self: &Arc<Self>, arg: &mut IvcSubscribeArg) -> VfsResult<()> {
        match self.transport {
            AxivcLinkSelection::Legacy => self.subscribe_legacy(arg),
            AxivcLinkSelection::Ivshmem(link) => self.subscribe_ivshmem(arg, link),
            AxivcLinkSelection::Invalid => Err(VfsError::InvalidInput),
        }
    }

    fn subscribe_legacy(&self, arg: &mut IvcSubscribeArg) -> VfsResult<()> {
        let shm_base_gpa = HyperCallOutputSlot::new(0)?;
        let shm_size = HyperCallOutputSlot::new(0)?;
        ivc::subscribe_channel(
            arg.target_publisher_id as usize,
            arg.channel_key as usize,
            shm_base_gpa.guest_phys_addr(),
            shm_size.guest_phys_addr(),
        )
        .map_err(hvc_error)?;

        let shm_base_gpa = shm_base_gpa.read();
        let shm_size = shm_size.read();
        if shm_size < core::mem::size_of::<IvcRegion>() {
            unsubscribe_hvc(arg);
            return Err(VfsError::InvalidInput);
        }

        let region = match shared_page_ref(shm_base_gpa, shm_size) {
            Ok(region) => region,
            Err(err) => {
                unsubscribe_hvc(arg);
                return Err(err);
            }
        };
        if let Err(err) = wait_for_region_ready(
            region,
            arg.target_publisher_id as usize,
            arg.channel_key as usize,
        ) {
            unsubscribe_hvc(arg);
            return Err(err);
        }
        let (producer, consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

        let mut inner = self.inner.lock();
        if inner.subscriber_exists(arg.target_publisher_id as usize, arg.channel_key) {
            unsubscribe_hvc(arg);
            return Err(VfsError::InvalidInput);
        }
        let Some(index) = inner.free_subscriber_index() else {
            unsubscribe_hvc(arg);
            return Err(VfsError::NoMemory);
        };
        let Some(poll_set) = self.poll_set(ChannelRole::Subscriber, index) else {
            unsubscribe_hvc(arg);
            return Err(VfsError::NoMemory);
        };
        inner.insert_subscriber_at(
            index,
            ChannelState {
                publisher_id: arg.target_publisher_id as usize,
                key: arg.channel_key,
                fd_refs: 0,
                poll_set,
                closing: false,
                visible: false,
                backend: ChannelBackend::Legacy(LegacyChannelState {
                    notify_target_vm_id: Some(arg.target_publisher_id as usize),
                    shm_base_gpa,
                    shm_size,
                    mmap_anchor: Arc::new(()),
                    producer,
                    consumer,
                    sequence: AtomicU64::new(1),
                }),
            },
        );
        write_device_name(
            &mut arg.device_name,
            &channel_device_path(ChannelRole::Subscriber, index),
        );
        info!(
            "axivc: subscribed publisher={} key={:#x} slot={} base={:#x} size={:#x}",
            arg.target_publisher_id, arg.channel_key, index, shm_base_gpa, shm_size
        );
        Ok(())
    }

    fn unsubscribe(&self, arg: &IvcSubscribeArg) -> VfsResult<()> {
        match self.transport {
            AxivcLinkSelection::Legacy => self.unsubscribe_legacy(arg, false),
            AxivcLinkSelection::Ivshmem(_) => self.unsubscribe_ivshmem(arg, false),
            AxivcLinkSelection::Invalid => Err(VfsError::InvalidInput),
        }
    }

    fn rollback_subscribe(&self, arg: &IvcSubscribeArg) -> VfsResult<()> {
        match self.transport {
            AxivcLinkSelection::Legacy => self.unsubscribe_legacy(arg, true),
            AxivcLinkSelection::Ivshmem(_) => self.unsubscribe_ivshmem(arg, true),
            AxivcLinkSelection::Invalid => Err(VfsError::InvalidInput),
        }
    }

    fn unsubscribe_legacy(&self, arg: &IvcSubscribeArg, rollback: bool) -> VfsResult<()> {
        let mut inner = self.inner.lock();
        let Some(state) = inner.subscriber_mut(arg.target_publisher_id as usize, arg.channel_key)
        else {
            return Err(VfsError::NoSuchDevice);
        };
        if !rollback && !state.visible {
            return Err(VfsError::WouldBlock);
        }
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        if state.has_mmap_refs() {
            return Err(VfsError::WouldBlock);
        }
        if state.fd_refs != 0 {
            return Err(VfsError::WouldBlock);
        }
        state.closing = true;
        let poll_set = state.poll_set.clone();
        drop(inner);
        wake_pollers(&poll_set, IoEvents::ERR | IoEvents::HUP);

        let result =
            ivc::unsubscribe_channel(arg.target_publisher_id as usize, arg.channel_key as usize)
                .map_err(hvc_error);
        match result {
            Ok(()) => {
                self.inner
                    .lock()
                    .remove_subscriber(arg.target_publisher_id as usize, arg.channel_key);
                Ok(())
            }
            Err(err) => {
                if let Some(state) = self
                    .inner
                    .lock()
                    .subscriber_mut(arg.target_publisher_id as usize, arg.channel_key)
                {
                    state.closing = false;
                }
                Err(err)
            }
        }
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn publish_ivshmem(
        self: &Arc<Self>,
        arg: &mut IvcPublishArg,
        link: IvshmemLink,
    ) -> VfsResult<()> {
        if arg.channel_key != link.key() || arg.channel_size != LEGACY_PUBLISH_SIZE {
            return Err(VfsError::InvalidInput);
        }
        let publisher_id =
            usize::try_from(link.publisher_vm_id()).map_err(|_| VfsError::InvalidInput)?;
        let _transition = self.session_transition.lock();
        let attachment = self.ivshmem_attachment()?;
        if !attachment.role_matches(true) {
            return Err(VfsError::PermissionDenied);
        }

        let (index, poll_set) = {
            let inner = self.inner.lock();
            if inner.publisher_has_ivshmem() || inner.publisher_exists(arg.channel_key) {
                return Err(VfsError::ResourceBusy);
            }
            let index = inner.free_publisher_index().ok_or(VfsError::NoMemory)?;
            let poll_set = self
                .poll_set(ChannelRole::Publisher, index)
                .ok_or(VfsError::NoMemory)?;
            (index, poll_set)
        };
        let session = IvshmemChannelState::attach(attachment, true)?;

        self.inner.lock().insert_publisher_at(
            index,
            ChannelState {
                publisher_id,
                key: arg.channel_key,
                fd_refs: 0,
                poll_set,
                closing: false,
                visible: false,
                backend: ChannelBackend::Ivshmem(session),
            },
        );
        write_device_name(
            &mut arg.device_name,
            &channel_device_path(ChannelRole::Publisher, index),
        );
        info!(
            "axivc: ivshmem published key={:#x} publisher={} slot={}",
            arg.channel_key, publisher_id, index
        );
        Ok(())
    }

    #[cfg(not(feature = "axivc-ivshmem"))]
    fn publish_ivshmem(
        self: &Arc<Self>,
        _arg: &mut IvcPublishArg,
        _link: IvshmemLink,
    ) -> VfsResult<()> {
        Err(VfsError::NoSuchDevice)
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn subscribe_ivshmem(
        self: &Arc<Self>,
        arg: &mut IvcSubscribeArg,
        link: IvshmemLink,
    ) -> VfsResult<()> {
        if arg.target_publisher_id != link.publisher_vm_id() || arg.channel_key != link.key() {
            return Err(VfsError::InvalidInput);
        }
        let publisher_id =
            usize::try_from(link.publisher_vm_id()).map_err(|_| VfsError::InvalidInput)?;
        let _transition = self.session_transition.lock();
        let attachment = self.ivshmem_attachment()?;
        if !attachment.role_matches(false) {
            return Err(VfsError::PermissionDenied);
        }

        let (index, poll_set) = {
            let inner = self.inner.lock();
            if inner.subscriber_has_ivshmem()
                || inner.subscriber_exists(publisher_id, arg.channel_key)
            {
                return Err(VfsError::ResourceBusy);
            }
            let index = inner.free_subscriber_index().ok_or(VfsError::NoMemory)?;
            let poll_set = self
                .poll_set(ChannelRole::Subscriber, index)
                .ok_or(VfsError::NoMemory)?;
            (index, poll_set)
        };
        let session = IvshmemChannelState::attach(attachment, false)?;
        self.inner.lock().insert_subscriber_at(
            index,
            ChannelState {
                publisher_id,
                key: arg.channel_key,
                fd_refs: 0,
                poll_set,
                closing: false,
                visible: false,
                backend: ChannelBackend::Ivshmem(session),
            },
        );
        write_device_name(
            &mut arg.device_name,
            &channel_device_path(ChannelRole::Subscriber, index),
        );
        info!(
            "axivc: ivshmem subscribed key={:#x} publisher={} slot={}",
            arg.channel_key, publisher_id, index
        );
        Ok(())
    }

    #[cfg(not(feature = "axivc-ivshmem"))]
    fn subscribe_ivshmem(
        self: &Arc<Self>,
        _arg: &mut IvcSubscribeArg,
        _link: IvshmemLink,
    ) -> VfsResult<()> {
        Err(VfsError::NoSuchDevice)
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn unpublish_ivshmem(&self, arg: &IvcPublishArg, rollback: bool) -> VfsResult<()> {
        let _transition = self.session_transition.lock();
        let mut inner = self.inner.lock();
        let Some(state) = inner.publisher_mut(arg.channel_key) else {
            return Err(VfsError::NoSuchDevice);
        };
        if !rollback && !state.visible {
            return Err(VfsError::WouldBlock);
        }
        if rollback && state.visible {
            return Err(VfsError::WouldBlock);
        }
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        if state.has_mmap_refs() || state.fd_refs != 0 {
            return Err(VfsError::WouldBlock);
        }
        state.closing = true;
        let poll_set = state.poll_set.clone();
        let mut state = inner
            .remove_publisher(arg.channel_key)
            .ok_or(VfsError::NoSuchDevice)?;
        drop(inner);
        wake_pollers(&poll_set, IoEvents::ERR | IoEvents::HUP);
        if let Err(error) = state.invalidate_ivshmem() {
            warn!("axivc: ivshmem publisher teardown was incomplete: {error:?}");
        }
        Ok(())
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn unsubscribe_ivshmem(&self, arg: &IvcSubscribeArg, rollback: bool) -> VfsResult<()> {
        let publisher_id =
            usize::try_from(arg.target_publisher_id).map_err(|_| VfsError::InvalidInput)?;
        let _transition = self.session_transition.lock();
        let mut inner = self.inner.lock();
        let Some(state) = inner.subscriber_mut(publisher_id, arg.channel_key) else {
            return Err(VfsError::NoSuchDevice);
        };
        if !rollback && !state.visible {
            return Err(VfsError::WouldBlock);
        }
        if rollback && state.visible {
            return Err(VfsError::WouldBlock);
        }
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        if state.has_mmap_refs() || state.fd_refs != 0 {
            return Err(VfsError::WouldBlock);
        }
        state.closing = true;
        let poll_set = state.poll_set.clone();
        let mut state = inner
            .remove_subscriber(publisher_id, arg.channel_key)
            .ok_or(VfsError::NoSuchDevice)?;
        drop(inner);
        wake_pollers(&poll_set, IoEvents::ERR | IoEvents::HUP);
        if let Err(error) = state.invalidate_ivshmem() {
            warn!("axivc: ivshmem subscriber teardown was incomplete: {error:?}");
        }
        Ok(())
    }

    #[cfg(not(feature = "axivc-ivshmem"))]
    fn unpublish_ivshmem(&self, _arg: &IvcPublishArg, _rollback: bool) -> VfsResult<()> {
        Err(VfsError::NoSuchDevice)
    }

    #[cfg(not(feature = "axivc-ivshmem"))]
    fn unsubscribe_ivshmem(&self, _arg: &IvcSubscribeArg, _rollback: bool) -> VfsResult<()> {
        Err(VfsError::NoSuchDevice)
    }

    fn commit_publisher(&self, key: u64) -> VfsResult<()> {
        let mut inner = self.inner.lock();
        let state = inner.publisher_mut(key).ok_or(VfsError::NoSuchDevice)?;
        state.visible = true;
        Ok(())
    }

    fn commit_subscriber(&self, publisher_id: usize, key: u64) -> VfsResult<()> {
        let mut inner = self.inner.lock();
        let state = inner
            .subscriber_mut(publisher_id, key)
            .ok_or(VfsError::NoSuchDevice)?;
        state.visible = true;
        Ok(())
    }
}

impl Drop for AxivcRegistry {
    fn drop(&mut self) {
        #[cfg(feature = "axivc-ivshmem")]
        if let IvshmemAttachmentState::Ready(attachment) = &*self.ivshmem_attachment.lock() {
            if let Err(error) = attachment.disable_interrupts() {
                warn!("axivc: failed to disable ivshmem interrupts during teardown: {error:?}");
            }
            attachment.disable_irq();
        }
        if let Some(handle) = self.irq_handle.get().copied() {
            let _ = ax_runtime::hal::irq::disable_irq(handle);
            let _ = ax_runtime::hal::irq::free_irq(handle);
        }
        #[cfg(feature = "axivc-ivshmem")]
        if let Some(handle) = self.ivshmem_irq_handle.get().copied() {
            let _ = ax_runtime::hal::irq::disable_irq(handle);
            let _ = ax_runtime::hal::irq::free_irq(handle);
        }
        // The service owns only a Weak registry reference. Wake it once so it
        // can observe that the final registry owner is gone and exit.
        self.deferred_notify.notify();
    }
}

fn new_poll_sets(channel_count: usize) -> Vec<Arc<PollSet>> {
    let mut poll_sets = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        poll_sets.push(Arc::new(PollSet::new()));
    }
    poll_sets
}

fn probe_notify_irq() -> Option<IrqId> {
    rdrive::with_fdt(|fdt| {
        fdt.find_compatible(AXIVC_COMPATIBLES)
            .into_iter()
            .find_map(resolve_notify_irq_from_node)
    })
    .flatten()
}

fn resolve_notify_irq_from_node(node: rdrive::probe::fdt::NodeType<'_>) -> Option<IrqId> {
    if matches!(
        node.as_node().status(),
        Some(rdrive::probe::fdt::Status::Disabled)
    ) {
        return None;
    }
    let interrupts = node.interrupts();
    match decode_fdt_irq(&interrupts) {
        Ok(irq) => irq,
        Err(err) => {
            warn!(
                "axivc: failed to resolve notify IRQ for {}: {err:?}",
                node.name()
            );
            None
        }
    }
}

fn decode_fdt_irq(
    interrupts: &[rdrive::probe::fdt::InterruptRef],
) -> Result<Option<IrqId>, IrqError> {
    let Some(interrupt) = interrupts.first() else {
        return Ok(None);
    };
    let controller = rdrive::fdt_phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or(IrqError::Unsupported)?;
    ax_runtime::irq::resolve_binding_irq(ax_driver::BindingIrq::fdt_interrupt_with_controller(
        controller,
        interrupt.specifier.clone(),
    ))
    .map(Some)
}

struct RegistryInner {
    publishers: Vec<Option<ChannelState>>,
    subscribers: Vec<Option<ChannelState>>,
}

impl RegistryInner {
    fn new(channel_count: usize) -> Self {
        let mut publishers = Vec::with_capacity(channel_count);
        let mut subscribers = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            publishers.push(None);
            subscribers.push(None);
        }
        Self {
            publishers,
            subscribers,
        }
    }

    fn publisher_exists(&self, key: u64) -> bool {
        self.publishers
            .iter()
            .flatten()
            .any(|state| state.key == key)
    }

    fn subscriber_exists(&self, publisher_id: usize, key: u64) -> bool {
        self.subscribers
            .iter()
            .flatten()
            .any(|state| state.publisher_id == publisher_id && state.key == key)
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn publisher_has_ivshmem(&self) -> bool {
        self.publishers
            .iter()
            .flatten()
            .any(|state| matches!(&state.backend, ChannelBackend::Ivshmem(_)))
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn subscriber_has_ivshmem(&self) -> bool {
        self.subscribers
            .iter()
            .flatten()
            .any(|state| matches!(&state.backend, ChannelBackend::Ivshmem(_)))
    }

    fn free_publisher_index(&self) -> Option<usize> {
        self.publishers.iter().position(Option::is_none)
    }

    fn free_subscriber_index(&self) -> Option<usize> {
        self.subscribers.iter().position(Option::is_none)
    }

    fn insert_publisher_at(&mut self, index: usize, state: ChannelState) {
        debug_assert!(
            self.publishers
                .get(index)
                .is_some_and(|entry| entry.is_none())
        );
        self.publishers[index] = Some(state);
    }

    fn insert_subscriber_at(&mut self, index: usize, state: ChannelState) {
        debug_assert!(
            self.subscribers
                .get(index)
                .is_some_and(|entry| entry.is_none())
        );
        self.subscribers[index] = Some(state);
    }

    fn remove_publisher(&mut self, key: u64) -> Option<ChannelState> {
        let index = self
            .publishers
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|state| state.key == key))?;
        self.publishers[index].take()
    }

    fn remove_subscriber(&mut self, publisher_id: usize, key: u64) -> Option<ChannelState> {
        let index = self.subscribers.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|state| state.publisher_id == publisher_id && state.key == key)
        })?;
        self.subscribers[index].take()
    }

    fn publisher_mut(&mut self, key: u64) -> Option<&mut ChannelState> {
        self.publishers
            .iter_mut()
            .flatten()
            .find(|state| state.key == key)
    }

    fn subscriber_mut(&mut self, publisher_id: usize, key: u64) -> Option<&mut ChannelState> {
        self.subscribers
            .iter_mut()
            .flatten()
            .find(|state| state.publisher_id == publisher_id && state.key == key)
    }

    fn channel_mut(&mut self, role: ChannelRole, index: usize) -> Option<&mut ChannelState> {
        match role {
            ChannelRole::Publisher => self.publishers.get_mut(index)?.as_mut(),
            ChannelRole::Subscriber => self.subscribers.get_mut(index)?.as_mut(),
        }
    }

    fn channel(&self, role: ChannelRole, index: usize) -> Option<&ChannelState> {
        match role {
            ChannelRole::Publisher => self.publishers.get(index)?.as_ref(),
            ChannelRole::Subscriber => self.subscribers.get(index)?.as_ref(),
        }
    }

    fn open_channel(&mut self, role: ChannelRole, index: usize) -> VfsResult<()> {
        let state = self
            .channel_mut(role, index)
            .ok_or(VfsError::NoSuchDevice)?;
        if state.closing || !state.visible {
            return Err(VfsError::WouldBlock);
        }
        state.fd_refs = state.fd_refs.checked_add(1).ok_or(VfsError::NoMemory)?;
        Ok(())
    }

    fn close_channel(&mut self, role: ChannelRole, index: usize) -> Option<Arc<PollSet>> {
        let state = self.channel_mut(role, index)?;
        state.fd_refs = state.fd_refs.saturating_sub(1);
        (state.closing && state.fd_refs == 0).then(|| state.poll_set.clone())
    }

    fn channel_poll_set(&self, role: ChannelRole, index: usize) -> Option<Arc<PollSet>> {
        Some(self.channel(role, index)?.poll_set.clone())
    }
}

struct ChannelState {
    publisher_id: usize,
    key: u64,
    fd_refs: usize,
    poll_set: Arc<PollSet>,
    closing: bool,
    visible: bool,
    backend: ChannelBackend,
}

enum ChannelBackend {
    Legacy(LegacyChannelState),
    #[cfg(feature = "axivc-ivshmem")]
    Ivshmem(IvshmemChannelState),
}

struct LegacyChannelState {
    notify_target_vm_id: Option<usize>,
    shm_base_gpa: usize,
    shm_size: usize,
    mmap_anchor: Arc<()>,
    producer: IvcProducer<'static>,
    consumer: IvcConsumer<'static>,
    sequence: AtomicU64,
}

impl ChannelState {
    fn has_mmap_refs(&self) -> bool {
        match &self.backend {
            ChannelBackend::Legacy(legacy) => Arc::strong_count(&legacy.mmap_anchor) > 1,
            #[cfg(feature = "axivc-ivshmem")]
            ChannelBackend::Ivshmem(_) => false,
        }
    }

    #[cfg(feature = "axivc-ivshmem")]
    fn invalidate_ivshmem(&mut self) -> VfsResult<()> {
        if let ChannelBackend::Ivshmem(session) = &mut self.backend {
            session.invalidate()
        } else {
            Ok(())
        }
    }
}

pub(super) fn register_devices(root: &mut DirMapping, fs: Arc<SimpleFs>) {
    let transport = AxivcLinkSelection::from_bootargs();
    match transport {
        AxivcLinkSelection::Legacy => {}
        AxivcLinkSelection::Ivshmem(link) => info!(
            "axivc: transport=ivshmem key={} publisher_vm_id={}",
            link.key(),
            link.publisher_vm_id()
        ),
        AxivcLinkSelection::Invalid => {
            warn!("axivc: malformed or duplicate axivc.link token; HVC fallback disabled");
        }
    }
    let registry = Arc::new(AxivcRegistry::new(MAX_CHANNELS, transport));
    if transport.is_legacy() {
        registry.register_notify_irq();
    }
    root.add(
        "axivc",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(10, AXIVC_MANAGER_MINOR),
            Arc::new(AxivcManager {
                registry: registry.clone(),
            }),
        ),
    );
    #[cfg(feature = "axivc-ivshmem")]
    root.add(
        "axivc0",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(10, AXIVC_AUTO_MINOR),
            Arc::new(AxivcAutoEndpoint {
                registry: registry.clone(),
                state: Mutex::new(AutoEndpointState {
                    channel: None,
                    retired: Vec::new(),
                    next_generation: 0,
                }),
            }),
        ),
    );

    for index in 0..MAX_CHANNELS {
        root.add(
            format!("axivc_publisher_{index}"),
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(10, AXIVC_PUBLISHER_MINOR_BASE + index as u32),
                Arc::new(AxivcChannel {
                    registry: registry.clone(),
                    role: ChannelRole::Publisher,
                    index,
                }),
            ),
        );
        root.add(
            format!("axivc_subscriber_{index}"),
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(10, AXIVC_SUBSCRIBER_MINOR_BASE + index as u32),
                Arc::new(AxivcChannel {
                    registry: registry.clone(),
                    role: ChannelRole::Subscriber,
                    index,
                }),
            ),
        );
    }
}

struct AxivcManager {
    registry: Arc<AxivcRegistry>,
}

impl DeviceOps for AxivcManager {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn ioctl(&self, current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            IVC_PUBLISH_CHANNEL => {
                let user_arg = arg as *mut IvcPublishArg;
                let mut publish_arg = user_arg.vm_read(current).map_err(vm_error)?;
                self.registry.publish(&mut publish_arg)?;
                if let Err(err) = user_arg.vm_write(current, publish_arg) {
                    warn!(
                        "axivc: publish ioctl writeback failed key={:#x}",
                        publish_arg.channel_key
                    );
                    let _ = self.registry.rollback_publish(&publish_arg);
                    return Err(vm_error(err));
                }
                if let Err(error) = self.registry.commit_publisher(publish_arg.channel_key) {
                    let _ = self.registry.rollback_publish(&publish_arg);
                    return Err(error);
                }
                info!(
                    "axivc: publish ioctl complete key={:#x} device={}",
                    publish_arg.channel_key,
                    device_name_for_log(&publish_arg.device_name)
                );
                Ok(0)
            }
            IVC_UNPUBLISH_CHANNEL => {
                let user_arg = (arg as *const IvcPublishArg)
                    .vm_read(current)
                    .map_err(vm_error)?;
                self.registry.unpublish(&user_arg)?;
                Ok(0)
            }
            IVC_SUBSCRIBE_CHANNEL => {
                let user_arg = arg as *mut IvcSubscribeArg;
                let mut subscribe_arg = user_arg.vm_read(current).map_err(vm_error)?;
                self.registry.subscribe(&mut subscribe_arg)?;
                if let Err(err) = user_arg.vm_write(current, subscribe_arg) {
                    warn!(
                        "axivc: subscribe ioctl writeback failed publisher={} key={:#x}",
                        subscribe_arg.target_publisher_id, subscribe_arg.channel_key
                    );
                    let _ = self.registry.rollback_subscribe(&subscribe_arg);
                    return Err(vm_error(err));
                }
                let publisher_id = match usize::try_from(subscribe_arg.target_publisher_id) {
                    Ok(publisher_id) => publisher_id,
                    Err(_) => {
                        let _ = self.registry.rollback_subscribe(&subscribe_arg);
                        return Err(VfsError::InvalidInput);
                    }
                };
                if let Err(error) = self
                    .registry
                    .commit_subscriber(publisher_id, subscribe_arg.channel_key)
                {
                    let _ = self.registry.rollback_subscribe(&subscribe_arg);
                    return Err(error);
                }
                info!(
                    "axivc: subscribe ioctl complete publisher={} key={:#x} device={}",
                    subscribe_arg.target_publisher_id,
                    subscribe_arg.channel_key,
                    device_name_for_log(&subscribe_arg.device_name)
                );
                Ok(0)
            }
            IVC_UNSUBSCRIBE_CHANNEL => {
                let user_arg = (arg as *const IvcSubscribeArg)
                    .vm_read(current)
                    .map_err(vm_error)?;
                self.registry.unsubscribe(&user_arg)?;
                Ok(0)
            }
            _ => Err(VfsError::NotATty),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(feature = "axivc-ivshmem")]
impl AxivcAutoEndpoint {
    fn current_channel(&self) -> VfsResult<AutoChannel> {
        self.state
            .lock()
            .channel
            .clone()
            .ok_or(VfsError::NoSuchDevice)
    }

    fn channel_ops(&self, channel: &AutoChannel) -> AxivcChannel {
        AxivcChannel {
            registry: Arc::clone(&self.registry),
            role: channel.role,
            index: channel.index,
        }
    }

    fn payload_info(&self, channel: &AutoChannel) -> IvcPayloadInfo {
        IvcPayloadInfo {
            version: PAYLOAD_INFO_VERSION,
            generation: channel.lease.generation,
            local_offset: 0,
            peer_offset: PAYLOAD_PEER_MMAP_OFFSET,
            payload_size: channel.lease.attachment.payload_capacity() as u64,
        }
    }
}

#[cfg(feature = "axivc-ivshmem")]
impl DeviceOps for AxivcAutoEndpoint {
    fn len(&self) -> VfsResult<u64> {
        let channel = self.current_channel()?;
        if !self.registry.auto_channel_epoch_valid(&channel) {
            return Err(VfsError::Io);
        }
        Ok(channel.lease.attachment.payload_capacity() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let channel = self.current_channel()?;
        if !self.registry.auto_channel_epoch_valid(&channel) {
            return Err(VfsError::Io);
        }
        let result = self.channel_ops(&channel).read_at(buf, offset);
        if matches!(result, Err(VfsError::Io)) {
            self.registry.auto_channel_epoch_valid(&channel);
        }
        result
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let channel = self.current_channel()?;
        if !self.registry.auto_channel_epoch_valid(&channel) {
            return Err(VfsError::Io);
        }
        let result = self.channel_ops(&channel).write_at(buf, offset);
        if matches!(result, Err(VfsError::Io)) {
            self.registry.auto_channel_epoch_valid(&channel);
        }
        result
    }

    fn ioctl(&self, current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        if cmd != IVC_PAYLOAD_INFO {
            return Err(VfsError::NotATty);
        }
        let channel = self.current_channel()?;
        if !self.registry.auto_channel_epoch_valid(&channel) {
            return Err(VfsError::Io);
        }
        let info = self.payload_info(&channel);
        (arg as *mut IvcPayloadInfo)
            .vm_write(current, info)
            .map_err(vm_error)?;
        Ok(0)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM | NodeFlags::PACKET
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        let Ok(channel) = self.current_channel() else {
            return DeviceMmap::None;
        };
        if !self.registry.auto_channel_epoch_valid(&channel) {
            return DeviceMmap::None;
        }
        let Ok(length) = usize::try_from(length) else {
            return DeviceMmap::None;
        };
        if length == 0 || length > channel.lease.attachment.payload_capacity() {
            return DeviceMmap::None;
        }
        let peer = match offset {
            0 => false,
            PAYLOAD_PEER_MMAP_OFFSET => true,
            _ => return DeviceMmap::None,
        };
        match channel.lease.map(peer) {
            Ok(mapping) => mapping,
            Err(_) => {
                self.registry.auto_channel_epoch_valid(&channel);
                DeviceMmap::None
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        let mut state = self.state.lock();
        state.retired.retain(|lease| lease.strong_count() != 0);
        if state.channel.is_some() || !state.retired.is_empty() {
            return Err(VfsError::ResourceBusy);
        }
        let generation = state
            .next_generation
            .checked_add(1)
            .ok_or(VfsError::NoMemory)?;
        let channel = self.registry.provision_auto(generation)?;
        state.next_generation = generation;
        state.channel = Some(channel);
        Ok(())
    }

    fn close(&self, _exclusive: bool) {
        let mut state = self.state.lock();
        let Some(channel) = state.channel.take() else {
            return;
        };
        state.retired.retain(|lease| lease.strong_count() != 0);
        state.retired.push(Arc::downgrade(&channel.lease));
        drop(state);
        if let Err(error) = self.registry.release_auto(&channel) {
            warn!("axivc0: failed to release automatic ivshmem channel: {error:?}");
        }
    }
}

#[cfg(feature = "axivc-ivshmem")]
impl AxivcAutoEndpoint {
    fn ready_events(&self) -> IoEvents {
        let Ok(channel) = self.current_channel() else {
            return IoEvents::ERR | IoEvents::HUP;
        };
        let events = self.channel_ops(&channel).ready_events();
        if events.intersects(IoEvents::ERR | IoEvents::HUP) {
            self.registry.auto_channel_epoch_valid(&channel);
        }
        events
    }

    fn channel_poll_set(&self) -> Option<Arc<PollSet>> {
        let channel = self.current_channel().ok()?;
        self.channel_ops(&channel).poll_set()
    }
}

#[cfg(feature = "axivc-ivshmem")]
impl Pollable for AxivcAutoEndpoint {
    fn poll(&self) -> IoEvents {
        self.ready_events()
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        if self
            .ready_events()
            .intersects(events | IoEvents::ALWAYS_POLL)
        {
            sink.waker().wake_by_ref();
            return;
        }
        if let Some(poll_set) = self.channel_poll_set() {
            unsafe { sink.register_shared(poll_set.as_ref(), events) };
            if self
                .ready_events()
                .intersects(events | IoEvents::ALWAYS_POLL)
            {
                sink.waker().wake_by_ref();
            }
        } else {
            sink.waker().wake_by_ref();
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ChannelRole {
    Publisher,
    Subscriber,
}

struct AxivcChannel {
    registry: Arc<AxivcRegistry>,
    role: ChannelRole,
    index: usize,
}

impl DeviceOps for AxivcChannel {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut inner = self.registry.inner.lock();
        let state = inner
            .channel_mut(self.role, self.index)
            .ok_or(VfsError::NoSuchDevice)?;
        if state.closing || !state.visible {
            return Err(VfsError::WouldBlock);
        }
        match &mut state.backend {
            ChannelBackend::Legacy(legacy) => {
                let message = legacy.consumer.try_recv(buf).map_err(ring_error)?;
                let Some(message) = message else {
                    return Err(VfsError::WouldBlock);
                };
                let len = message.len();
                let key = state.key;
                let poll_set = state.poll_set.clone();
                notify_legacy_peer(
                    state.publisher_id,
                    key,
                    legacy.notify_target_vm_id,
                    self.role_name(),
                );
                info!(
                    "axivc: read role={} key={key:#x} seq={} kind={:?} len={len}",
                    self.role_name(),
                    message.sequence(),
                    message.kind()
                );
                drop(inner);
                wake_pollers(&poll_set, IoEvents::OUT);
                Ok(len)
            }
            #[cfg(feature = "axivc-ivshmem")]
            ChannelBackend::Ivshmem(session) => {
                let poll_set = state.poll_set.clone();
                let (len, events) = session.read(buf)?;
                drop(inner);
                if !events.is_empty() {
                    wake_pollers(&poll_set, events);
                }
                Ok(len)
            }
        }
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if buf.len() > APP_PAYLOAD_LIMIT {
            return Err(VfsError::InvalidInput);
        }
        let mut inner = self.registry.inner.lock();
        let state = inner
            .channel_mut(self.role, self.index)
            .ok_or(VfsError::NoSuchDevice)?;
        if state.closing || !state.visible {
            return Err(VfsError::WouldBlock);
        }
        match &mut state.backend {
            ChannelBackend::Legacy(legacy) => {
                let sequence = legacy.sequence.fetch_add(1, Ordering::Relaxed);
                let kind = match self.role {
                    ChannelRole::Publisher => IvcMessageKind::Request,
                    ChannelRole::Subscriber => IvcMessageKind::Ack,
                };
                legacy
                    .producer
                    .send(kind, sequence, buf)
                    .map_err(ring_error)?;
                let key = state.key;
                let poll_set = state.poll_set.clone();
                notify_legacy_peer(
                    state.publisher_id,
                    key,
                    legacy.notify_target_vm_id,
                    self.role_name(),
                );
                info!(
                    "axivc: write role={} key={key:#x} seq={sequence} len={}",
                    self.role_name(),
                    buf.len()
                );
                drop(inner);
                wake_pollers(&poll_set, IoEvents::IN);
                Ok(buf.len())
            }
            #[cfg(feature = "axivc-ivshmem")]
            ChannelBackend::Ivshmem(session) => {
                let poll_set = state.poll_set.clone();
                let (len, events) = session.write(buf)?;
                drop(inner);
                if !events.is_empty() {
                    wake_pollers(&poll_set, events);
                }
                Ok(len)
            }
        }
    }

    fn ioctl(&self, current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        if !self.registry.transport.is_legacy() {
            return Err(VfsError::NotATty);
        }
        let op = match cmd {
            IVC_CACHE_FLUSH => DCacheOp::Clean,
            IVC_CACHE_INVALIDATE => DCacheOp::Invalidate,
            _ => return Err(VfsError::NotATty),
        };
        let cache_arg = (arg as *const IvcCacheOpArg)
            .vm_read(current)
            .map_err(vm_error)?;
        let (shm_base_gpa, shm_size) = {
            let inner = self.registry.inner.lock();
            let state = inner
                .channel(self.role, self.index)
                .ok_or(VfsError::NoSuchDevice)?;
            if state.closing {
                return Err(VfsError::WouldBlock);
            }
            match &state.backend {
                ChannelBackend::Legacy(legacy) => (legacy.shm_base_gpa, legacy.shm_size),
                #[cfg(feature = "axivc-ivshmem")]
                ChannelBackend::Ivshmem(_) => return Err(VfsError::NotATty),
            }
        };
        if cache_arg.size == 0 {
            return Ok(0);
        }
        cache_op_user_range(
            current,
            op,
            shm_base_gpa,
            shm_size,
            cache_arg.addr,
            cache_arg.size,
        )?;
        Ok(0)
    }

    fn flags(&self) -> NodeFlags {
        let flags = NodeFlags::NON_CACHEABLE | NodeFlags::STREAM;
        if self.registry.transport.uses_ivshmem() {
            flags | NodeFlags::PACKET
        } else {
            flags
        }
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        if !self.registry.transport.is_legacy() {
            return DeviceMmap::None;
        }
        let Ok(offset) = usize::try_from(offset) else {
            return DeviceMmap::None;
        };
        let Ok(length) = usize::try_from(length) else {
            return DeviceMmap::None;
        };
        if length == 0 {
            return DeviceMmap::None;
        }

        let inner = self.registry.inner.lock();
        let Some(state) = inner.channel(self.role, self.index) else {
            return DeviceMmap::None;
        };
        if state.closing || !state.visible || offset != 0 {
            return DeviceMmap::None;
        }
        match &state.backend {
            ChannelBackend::Legacy(legacy) => {
                let retainer: Arc<dyn Any + Send + Sync> = legacy.mmap_anchor.clone();
                let retainer = Some(retainer);
                let range = PhysAddrRange::from_start_size(
                    PhysAddr::from_usize(legacy.shm_base_gpa),
                    legacy.shm_size,
                );
                DeviceMmap::PhysicalCached(range, retainer)
            }
            #[cfg(feature = "axivc-ivshmem")]
            ChannelBackend::Ivshmem(_) => DeviceMmap::None,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        self.registry
            .inner
            .lock()
            .open_channel(self.role, self.index)
    }

    fn close(&self, _exclusive: bool) {
        let poll_set = self
            .registry
            .inner
            .lock()
            .close_channel(self.role, self.index);
        if let Some(poll_set) = poll_set {
            wake_pollers(&poll_set, IoEvents::ERR | IoEvents::HUP);
        }
    }
}

impl AxivcChannel {
    fn role_name(&self) -> &'static str {
        match self.role {
            ChannelRole::Publisher => "publisher",
            ChannelRole::Subscriber => "subscriber",
        }
    }

    fn ready_events(&self) -> IoEvents {
        let mut inner = self.registry.inner.lock();
        let Some(state) = inner.channel_mut(self.role, self.index) else {
            return IoEvents::ERR | IoEvents::HUP;
        };
        if state.closing || !state.visible {
            return IoEvents::ERR | IoEvents::HUP;
        }

        match &mut state.backend {
            ChannelBackend::Legacy(legacy) => {
                let mut ready = IoEvents::empty();
                if legacy.consumer.can_recv() {
                    ready |= IoEvents::IN;
                }
                if legacy.producer.can_send() {
                    ready |= IoEvents::OUT;
                }
                ready
            }
            #[cfg(feature = "axivc-ivshmem")]
            ChannelBackend::Ivshmem(session) => session.ready_events(),
        }
    }

    fn poll_set(&self) -> Option<Arc<PollSet>> {
        self.registry
            .inner
            .lock()
            .channel_poll_set(self.role, self.index)
    }
}

impl Pollable for AxivcChannel {
    fn poll(&self) -> IoEvents {
        self.ready_events()
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        if self
            .ready_events()
            .intersects(events | IoEvents::ALWAYS_POLL)
        {
            sink.waker().wake_by_ref();
            return;
        }
        if let Some(poll_set) = self.poll_set() {
            unsafe { sink.register_shared(poll_set.as_ref(), events) };
            if self
                .ready_events()
                .intersects(events | IoEvents::ALWAYS_POLL)
            {
                sink.waker().wake_by_ref();
            }
        } else {
            sink.waker().wake_by_ref();
        }
    }
}

struct HyperCallOutputSlot {
    // HVC writes through a guest physical pointer. Task stacks may use a
    // guarded vmap alias, which is not covered by direct-map virt_to_phys.
    // Keep this single-word bounce buffer in the physical heap until return.
    value: Box<UnsafeCell<usize>>,
}

impl HyperCallOutputSlot {
    fn new(value: usize) -> VfsResult<Self> {
        Ok(Self {
            value: Box::try_new(UnsafeCell::new(value)).map_err(|_| VfsError::NoMemory)?,
        })
    }

    fn guest_phys_addr(&self) -> IvcGuestPhysAddr {
        let vaddr = VirtAddr::from_usize(self.value.get().addr());
        IvcGuestPhysAddr::new(virt_to_phys(vaddr).as_usize())
    }

    fn read(&self) -> usize {
        // SAFETY: the aligned initialized word stays owned by this Box across
        // the synchronous HVC; the host has finished writing before we read.
        unsafe { core::ptr::read_volatile(self.value.get()) }
    }
}

fn shared_page_mut(shm_base_gpa: usize, shm_size: usize) -> VfsResult<&'static mut IvcRegion> {
    check_shared_page_range(shm_base_gpa, shm_size)?;
    // AXIVC shared memory is CPU-owned RAM. Keep the Starry kernel view on the
    // same Normal WB contract as userspace mmap, Linux and Zephyr; mixing WB
    // and uncached aliases for the same PA can corrupt ring/header visibility
    // on ARM64.
    let vaddr = ax_mm::iomap_cached(PhysAddr::from_usize(shm_base_gpa), shm_size)
        .map_err(|_| VfsError::NoMemory)?;
    Ok(unsafe { &mut *(vaddr.as_mut_ptr() as *mut IvcRegion) })
}

fn shared_page_ref(shm_base_gpa: usize, shm_size: usize) -> VfsResult<&'static IvcRegion> {
    check_shared_page_range(shm_base_gpa, shm_size)?;
    let vaddr = ax_mm::iomap_cached(PhysAddr::from_usize(shm_base_gpa), shm_size)
        .map_err(|_| VfsError::NoMemory)?;
    Ok(unsafe { &*(vaddr.as_ptr() as *const IvcRegion) })
}

fn check_shared_page_range(shm_base_gpa: usize, shm_size: usize) -> VfsResult<()> {
    if shm_size == 0 || shm_base_gpa.checked_add(shm_size).is_none() {
        return Err(VfsError::InvalidInput);
    }
    Ok(())
}

fn wait_for_region_ready(region: &IvcRegion, publisher_id: usize, key: usize) -> VfsResult<()> {
    for _ in 0..REGION_READY_RETRIES {
        if region.channel_header_matches(publisher_id, key) && region.protocol_header_matches() {
            return Ok(());
        }
        crate::task::sleep(Duration::from_millis(REGION_READY_DELAY_MS));
    }
    Err(VfsError::WouldBlock)
}

fn write_device_name(buf: &mut [u8; 64], name: &str) {
    buf.fill(0);
    let bytes = name.as_bytes();
    let len = bytes.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
}

fn channel_device_path(role: ChannelRole, index: usize) -> String {
    match role {
        ChannelRole::Publisher => format!("/dev/axivc_publisher_{index}"),
        ChannelRole::Subscriber => format!("/dev/axivc_subscriber_{index}"),
    }
}

fn device_name_for_log(buf: &[u8; 64]) -> &str {
    let end = buf.iter().position(|&byte| byte == 0).unwrap_or(buf.len());
    core::str::from_utf8(&buf[..end]).unwrap_or("<nonutf8>")
}

fn unsubscribe_hvc(arg: &IvcSubscribeArg) {
    let _ = ivc::unsubscribe_channel(arg.target_publisher_id as usize, arg.channel_key as usize);
}

fn notify_legacy_peer(publisher_id: usize, key: u64, target_vm_id: Option<usize>, role_name: &str) {
    let Some(target_vm_id) = target_vm_id else {
        return;
    };
    if let Err(err) = ivc::notify_channel(publisher_id, key as usize, target_vm_id) {
        warn!(
            "axivc: notify failed role={} publisher={} key={:#x} target={} err={err}",
            role_name, publisher_id, key, target_vm_id
        );
    }
}

fn wake_pollers(poll_set: &PollSet, events: IoEvents) -> usize {
    unsafe { poll_set.wake(events) }
}

fn cache_op_user_range(
    current: &crate::task::UserTaskRef,
    op: DCacheOp,
    shm_base_gpa: usize,
    shm_size: usize,
    addr: u64,
    size: u64,
) -> VfsResult<()> {
    let addr = usize::try_from(addr).map_err(|_| VfsError::InvalidInput)?;
    let size = usize::try_from(size).map_err(|_| VfsError::InvalidInput)?;
    if addr == 0 || size == 0 {
        return Err(VfsError::InvalidInput);
    }
    let end = addr.checked_add(size).ok_or(VfsError::InvalidInput)?;
    let shm_end = shm_base_gpa
        .checked_add(shm_size)
        .ok_or(VfsError::InvalidInput)?;

    let aspace_pin = current.as_thread().proc_data.pin_aspace()?;
    let aspace = aspace_pin.lock();
    let mut cursor = addr;
    while cursor < end {
        let vaddr = VirtAddr::from_usize(cursor);
        let paddr = aspace.translate(vaddr).map_err(|_| VfsError::BadAddress)?;
        let page_size = aspace.resident_span(vaddr).ok_or(VfsError::BadAddress)?;
        if page_size == 0 {
            return Err(VfsError::InvalidInput);
        }
        let page_offset = cursor % page_size;
        let span = (end - cursor).min(page_size - page_offset);
        let paddr_start = paddr.as_usize();
        let paddr_end = paddr_start
            .checked_add(span)
            .ok_or(VfsError::InvalidInput)?;
        if paddr_start < shm_base_gpa || paddr_end > shm_end {
            return Err(VfsError::PermissionDenied);
        }
        cursor += span;
    }

    dcache_range(op, VirtAddr::from_usize(addr), size);
    Ok(())
}

fn hvc_error(_err: ivc::IvcHyperCallError) -> VfsError {
    VfsError::Io
}

fn vm_error(_err: starry_vm::VmError) -> VfsError {
    VfsError::BadAddress
}

fn ring_error(err: legacy_ivc::IvcRingError) -> VfsError {
    match err {
        legacy_ivc::IvcRingError::Full => VfsError::WouldBlock,
        legacy_ivc::IvcRingError::PayloadTooLarge { .. } => VfsError::InvalidInput,
        legacy_ivc::IvcRingError::BufferTooSmall { .. } => VfsError::InvalidInput,
        legacy_ivc::IvcRingError::UnknownMessageKind(_) => VfsError::InvalidData,
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use std::{
        sync::{
            Arc as StdArc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Wake, Waker},
    };

    use axpoll::{PollRegistrar, SharedObserver};

    use super::*;

    struct Counter(AtomicUsize);

    impl Counter {
        fn new() -> StdArc<Self> {
            StdArc::new(Self(AtomicUsize::new(0)))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl Wake for Counter {
        fn wake(self: StdArc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &StdArc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn register_waiter(
        poll_set: &PollSet,
        interests: IoEvents,
    ) -> (StdArc<Counter>, PollRegistrar<SharedObserver>) {
        let counter = Counter::new();
        let waker = Waker::from(counter.clone());
        let mut registrar = PollRegistrar::new(&waker);
        unsafe { registrar.register(poll_set, interests) };
        (counter, registrar)
    }

    #[test]
    fn deferred_notify_service_wakes_registered_channel_pollers() {
        let registry = AxivcRegistry::new_with_notify_irq(2, AxivcLinkSelection::Legacy, None);
        let publisher_poll_set = registry.poll_set(ChannelRole::Publisher, 0).unwrap();
        let subscriber_poll_set = registry.poll_set(ChannelRole::Subscriber, 1).unwrap();

        let (writer, _writer_registration) = register_waiter(&publisher_poll_set, IoEvents::OUT);
        let (reader, _reader_registration) = register_waiter(&subscriber_poll_set, IoEvents::IN);

        assert_eq!(registry.handle_notify_irq(), IrqReturn::Wake);
        assert_eq!(registry.wake_channel_pollers(), 2);
        assert_eq!(writer.count(), 1);
        assert_eq!(reader.count(), 1);
    }

    #[test]
    fn notify_irq_bridge_always_requests_deferred_service() {
        let registry = AxivcRegistry::new_with_notify_irq(2, AxivcLinkSelection::Legacy, None);

        assert_eq!(registry.handle_notify_irq(), IrqReturn::Wake);
    }
}
