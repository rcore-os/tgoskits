use alloc::{
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

use ax_hal::mem::{DCacheOp, PhysAddr, VirtAddr, dcache_range, virt_to_phys};
use ax_lazyinit::OnceLock;
use ax_memory_addr::PhysAddrRange;
use ax_runtime::hal::irq::{
    AutoEnable, IrqError, IrqHandle, IrqId, IrqRequest, IrqReturn, ShareMode,
};
use ax_task::current;
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsError, VfsResult};
use axhvc::ivc::{self, IvcGuestPhysAddr};
use axivc::{IVC_SLOT_PAYLOAD_SIZE, IvcConsumer, IvcMessageKind, IvcProducer, IvcRegion};
use axpoll::{IoEvents, PollSet, Pollable};
use bytemuck::AnyBitPattern;
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    pseudofs::{Device, DeviceMmap, DeviceOps, DirMapping, SimpleFs},
    sync::Mutex,
    task::AsThread,
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

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct IvcPublishArg {
    channel_key: u64,
    channel_size: u64,
    device_name: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct IvcSubscribeArg {
    target_publisher_id: u64,
    channel_key: u64,
    device_name: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct IvcCacheOpArg {
    addr: u64,
    size: u64,
}

struct AxivcRegistry {
    inner: Mutex<RegistryInner>,
    publisher_poll_sets: Vec<Arc<PollSet>>,
    subscriber_poll_sets: Vec<Arc<PollSet>>,
    notify_irq: Option<IrqId>,
    irq_handle: OnceLock<IrqHandle>,
}

impl AxivcRegistry {
    fn new(channel_count: usize) -> Self {
        Self::new_with_notify_irq(channel_count, probe_notify_irq())
    }

    fn new_with_notify_irq(channel_count: usize, notify_irq: Option<IrqId>) -> Self {
        Self {
            inner: Mutex::new(RegistryInner::new(channel_count)),
            publisher_poll_sets: new_poll_sets(channel_count),
            subscriber_poll_sets: new_poll_sets(channel_count),
            notify_irq,
            irq_handle: OnceLock::new(),
        }
    }

    fn register_notify_irq(self: &Arc<Self>) {
        let Some(irq) = self.notify_irq else {
            debug!(
                "axivc: axvisor,ivc-channel notify IRQ not found; blocking poll waits need peer-side polling"
            );
            return;
        };

        let registry = Arc::downgrade(self);
        let request = IrqRequest::new(move |_| notify_irq_handler(&registry))
            .share_mode(ShareMode::Shared)
            .auto_enable(AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(irq, request) {
            Ok(handle) => {
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

    fn handle_notify_irq(&self) -> IrqReturn {
        // The notify IRQ reports peer progress but does not carry a channel id.
        // Wake every slot poll set and let `poll()` re-check exact ring state.
        let woke = self
            .publisher_poll_sets
            .iter()
            .chain(self.subscriber_poll_sets.iter())
            .map(|poll_set| poll_set.wake_from_irq(IoEvents::IN | IoEvents::OUT))
            .sum::<usize>();
        if woke == 0 {
            IrqReturn::Handled
        } else {
            IrqReturn::Wake
        }
    }

    fn poll_set(&self, role: ChannelRole, index: usize) -> Option<Arc<PollSet>> {
        match role {
            ChannelRole::Publisher => self.publisher_poll_sets.get(index),
            ChannelRole::Subscriber => self.subscriber_poll_sets.get(index),
        }
        .cloned()
    }

    fn publish(&self, arg: &mut IvcPublishArg) -> VfsResult<()> {
        let shm_base_gpa = HyperCallOutputSlot::new(0);
        let shm_size = HyperCallOutputSlot::new(arg.channel_size as usize);

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
        inner.insert_publisher_at(index, ChannelState {
            publisher_id,
            notify_target_vm_id: Some(ivc::IVC_NOTIFY_PEER),
            key: arg.channel_key,
            shm_base_gpa,
            shm_size,
            mmap_anchor: Arc::new(()),
            fd_refs: 0,
            poll_set,
            producer,
            consumer,
            sequence: AtomicU64::new(1),
            closing: false,
        });
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
        let mut inner = self.inner.lock();
        let Some(state) = inner.publisher_mut(arg.channel_key) else {
            return Err(VfsError::NoSuchDevice);
        };
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        if Arc::strong_count(&state.mmap_anchor) > 1 {
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

    fn subscribe(&self, arg: &mut IvcSubscribeArg) -> VfsResult<()> {
        let shm_base_gpa = HyperCallOutputSlot::new(0);
        let shm_size = HyperCallOutputSlot::new(0);
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
        inner.insert_subscriber_at(index, ChannelState {
            publisher_id: arg.target_publisher_id as usize,
            notify_target_vm_id: Some(arg.target_publisher_id as usize),
            key: arg.channel_key,
            shm_base_gpa,
            shm_size,
            mmap_anchor: Arc::new(()),
            fd_refs: 0,
            poll_set,
            producer,
            consumer,
            sequence: AtomicU64::new(1),
            closing: false,
        });
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
        let mut inner = self.inner.lock();
        let Some(state) = inner.subscriber_mut(arg.target_publisher_id as usize, arg.channel_key)
        else {
            return Err(VfsError::NoSuchDevice);
        };
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        if Arc::strong_count(&state.mmap_anchor) > 1 {
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
}

impl Drop for AxivcRegistry {
    fn drop(&mut self) {
        if let Some(handle) = self.irq_handle.get().copied() {
            let _ = ax_runtime::hal::irq::disable_irq(handle);
            let _ = ax_runtime::hal::irq::free_irq(handle);
        }
    }
}

fn new_poll_sets(channel_count: usize) -> Vec<Arc<PollSet>> {
    let mut poll_sets = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        poll_sets.push(Arc::new(PollSet::new()));
    }
    poll_sets
}

fn notify_irq_handler(registry: &Weak<AxivcRegistry>) -> IrqReturn {
    let Some(registry) = registry.upgrade() else {
        return IrqReturn::Unhandled;
    };
    registry.handle_notify_irq()
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

    fn free_publisher_index(&self) -> Option<usize> {
        self.publishers.iter().position(Option::is_none)
    }

    fn free_subscriber_index(&self) -> Option<usize> {
        self.subscribers.iter().position(Option::is_none)
    }

    fn insert_publisher_at(&mut self, index: usize, state: ChannelState) {
        debug_assert!(self
            .publishers
            .get(index)
            .is_some_and(|entry| entry.is_none()));
        self.publishers[index] = Some(state);
    }

    fn insert_subscriber_at(&mut self, index: usize, state: ChannelState) {
        debug_assert!(self
            .subscribers
            .get(index)
            .is_some_and(|entry| entry.is_none()));
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
        let state = self.channel_mut(role, index).ok_or(VfsError::NoSuchDevice)?;
        if state.closing {
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
    notify_target_vm_id: Option<usize>,
    key: u64,
    shm_base_gpa: usize,
    shm_size: usize,
    mmap_anchor: Arc<()>,
    fd_refs: usize,
    poll_set: Arc<PollSet>,
    producer: IvcProducer<'static>,
    consumer: IvcConsumer<'static>,
    sequence: AtomicU64,
    closing: bool,
}

pub(super) fn register_devices(root: &mut DirMapping, fs: Arc<SimpleFs>) {
    let registry = Arc::new(AxivcRegistry::new(MAX_CHANNELS));
    registry.register_notify_irq();
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

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            IVC_PUBLISH_CHANNEL => {
                let user_arg = arg as *mut IvcPublishArg;
                let mut publish_arg = user_arg.vm_read().map_err(vm_error)?;
                self.registry.publish(&mut publish_arg)?;
                if let Err(err) = user_arg.vm_write(publish_arg) {
                    warn!(
                        "axivc: publish ioctl writeback failed key={:#x}",
                        publish_arg.channel_key
                    );
                    let _ = self.registry.unpublish(&publish_arg);
                    return Err(vm_error(err));
                }
                info!(
                    "axivc: publish ioctl complete key={:#x} device={}",
                    publish_arg.channel_key,
                    device_name_for_log(&publish_arg.device_name)
                );
                Ok(0)
            }
            IVC_UNPUBLISH_CHANNEL => {
                let user_arg = (arg as *const IvcPublishArg).vm_read().map_err(vm_error)?;
                self.registry.unpublish(&user_arg)?;
                Ok(0)
            }
            IVC_SUBSCRIBE_CHANNEL => {
                let user_arg = arg as *mut IvcSubscribeArg;
                let mut subscribe_arg = user_arg.vm_read().map_err(vm_error)?;
                self.registry.subscribe(&mut subscribe_arg)?;
                if let Err(err) = user_arg.vm_write(subscribe_arg) {
                    warn!(
                        "axivc: subscribe ioctl writeback failed publisher={} key={:#x}",
                        subscribe_arg.target_publisher_id, subscribe_arg.channel_key
                    );
                    let _ = self.registry.unsubscribe(&subscribe_arg);
                    return Err(vm_error(err));
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
                    .vm_read()
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

#[derive(Clone, Copy)]
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
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        let message = state.consumer.try_recv(buf).map_err(ring_error)?;
        let Some(message) = message else {
            return Err(VfsError::WouldBlock);
        };

        let len = message.len();
        let poll_set = state.poll_set.clone();
        notify_peer(state, self.role_name());
        info!(
            "axivc: read role={} key={:#x} seq={} kind={:?} len={}",
            self.role_name(),
            state.key,
            message.sequence(),
            message.kind(),
            len
        );
        drop(inner);
        wake_pollers(&poll_set, IoEvents::OUT);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if buf.len() > IVC_SLOT_PAYLOAD_SIZE {
            return Err(VfsError::InvalidInput);
        }
        let mut inner = self.registry.inner.lock();
        let state = inner
            .channel_mut(self.role, self.index)
            .ok_or(VfsError::NoSuchDevice)?;
        if state.closing {
            return Err(VfsError::WouldBlock);
        }
        let sequence = state.sequence.fetch_add(1, Ordering::Relaxed);
        let kind = match self.role {
            ChannelRole::Publisher => IvcMessageKind::Request,
            ChannelRole::Subscriber => IvcMessageKind::Ack,
        };

        state
            .producer
            .send(kind, sequence, buf)
            .map_err(ring_error)?;
        let poll_set = state.poll_set.clone();
        notify_peer(state, self.role_name());
        info!(
            "axivc: write role={} key={:#x} seq={} len={}",
            self.role_name(),
            state.key,
            sequence,
            buf.len()
        );
        drop(inner);
        wake_pollers(&poll_set, IoEvents::IN);
        Ok(buf.len())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        let op = match cmd {
            IVC_CACHE_FLUSH => DCacheOp::Clean,
            IVC_CACHE_INVALIDATE => DCacheOp::Invalidate,
            _ => return Err(VfsError::NotATty),
        };
        let cache_arg = (arg as *const IvcCacheOpArg).vm_read().map_err(vm_error)?;
        let (shm_base_gpa, shm_size) = {
            let inner = self.registry.inner.lock();
            let state = inner
                .channel(self.role, self.index)
                .ok_or(VfsError::NoSuchDevice)?;
            if state.closing {
                return Err(VfsError::WouldBlock);
            }
            (state.shm_base_gpa, state.shm_size)
        };
        if cache_arg.size == 0 {
            return Ok(0);
        }
        cache_op_user_range(op, shm_base_gpa, shm_size, cache_arg.addr, cache_arg.size)?;
        Ok(0)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
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
        if state.closing || offset != 0 {
            return DeviceMmap::None;
        }
        let retainer: Arc<dyn Any + Send + Sync> = state.mmap_anchor.clone();
        let retainer = Some(retainer);
        let range = PhysAddrRange::from_start_size(
            PhysAddr::from_usize(state.shm_base_gpa),
            state.shm_size,
        );
        DeviceMmap::PhysicalCached(range, retainer)
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
        let inner = self.registry.inner.lock();
        let Some(state) = inner.channel(self.role, self.index) else {
            return IoEvents::ERR | IoEvents::HUP;
        };
        if state.closing {
            return IoEvents::ERR | IoEvents::HUP;
        }

        let mut ready = IoEvents::empty();
        if state.consumer.can_recv() {
            ready |= IoEvents::IN;
        }
        if state.producer.can_send() {
            ready |= IoEvents::OUT;
        }
        ready
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

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if self.ready_events().intersects(events | IoEvents::ALWAYS_POLL) {
            context.waker().wake_by_ref();
            return;
        }
        if let Some(poll_set) = self.poll_set() {
            unsafe { poll_set.register(context.waker(), events) };
            if self.ready_events().intersects(events | IoEvents::ALWAYS_POLL) {
                context.waker().wake_by_ref();
            }
        } else {
            context.waker().wake_by_ref();
        }
    }
}

struct HyperCallOutputSlot {
    value: UnsafeCell<usize>,
}

impl HyperCallOutputSlot {
    const fn new(value: usize) -> Self {
        Self {
            value: UnsafeCell::new(value),
        }
    }

    fn guest_phys_addr(&self) -> IvcGuestPhysAddr {
        let vaddr = VirtAddr::from_usize(self.value.get().addr());
        IvcGuestPhysAddr::new(virt_to_phys(vaddr).as_usize())
    }

    fn read(&self) -> usize {
        unsafe { core::ptr::read_volatile(self.value.get()) }
    }
}

fn shared_page_mut(shm_base_gpa: usize, shm_size: usize) -> VfsResult<&'static mut IvcRegion> {
    check_shared_page_range(shm_base_gpa, shm_size)?;
    // AXIVC shared memory is CPU-owned RAM. Keep the Starry kernel view on the
    // same Normal WB contract as userspace mmap, Linux and Zephyr; mixing WB
    // and uncached aliases for the same PA can corrupt ring/header visibility
    // on ARM64.
    let vaddr =
        ax_mm::iomap_cached(PhysAddr::from_usize(shm_base_gpa), shm_size).map_err(|_| VfsError::NoMemory)?;
    Ok(unsafe { &mut *(vaddr.as_mut_ptr() as *mut IvcRegion) })
}

fn shared_page_ref(shm_base_gpa: usize, shm_size: usize) -> VfsResult<&'static IvcRegion> {
    check_shared_page_range(shm_base_gpa, shm_size)?;
    let vaddr =
        ax_mm::iomap_cached(PhysAddr::from_usize(shm_base_gpa), shm_size).map_err(|_| VfsError::NoMemory)?;
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
        ax_task::sleep(Duration::from_millis(REGION_READY_DELAY_MS));
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

fn notify_peer(state: &ChannelState, role_name: &str) {
    let Some(target_vm_id) = state.notify_target_vm_id else {
        return;
    };
    if let Err(err) = ivc::notify_channel(state.publisher_id, state.key as usize, target_vm_id) {
        warn!(
            "axivc: notify failed role={} publisher={} key={:#x} target={} err={err}",
            role_name, state.publisher_id, state.key, target_vm_id
        );
    }
}

fn wake_pollers(poll_set: &PollSet, events: IoEvents) {
    unsafe {
        let _ = poll_set.wake(events);
    }
}

fn cache_op_user_range(
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

    let aspace = current().as_thread().proc_data.aspace();
    let aspace = aspace.lock();
    let mut cursor = addr;
    while cursor < end {
        let (paddr, _flags, page_size) = aspace
            .page_table()
            .query(VirtAddr::from_usize(cursor))
            .map_err(|_| VfsError::BadAddress)?;
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

fn ring_error(err: axivc::IvcRingError) -> VfsError {
    match err {
        axivc::IvcRingError::Full => VfsError::WouldBlock,
        axivc::IvcRingError::PayloadTooLarge { .. } => VfsError::InvalidInput,
        axivc::IvcRingError::BufferTooSmall { .. } => VfsError::InvalidInput,
        axivc::IvcRingError::UnknownMessageKind(_) => VfsError::InvalidData,
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use super::*;
    use std::{
        sync::{
            Arc as StdArc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Wake, Waker},
    };

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

    fn register_waiter(poll_set: &PollSet, interests: IoEvents) -> StdArc<Counter> {
        let counter = Counter::new();
        let waker = Waker::from(counter.clone());
        unsafe { poll_set.register(&waker, interests) };
        counter
    }

    #[test]
    fn notify_irq_bridge_wakes_registered_channel_pollers() {
        let registry = AxivcRegistry::new_with_notify_irq(2, None);
        let publisher_poll_set = registry.poll_set(ChannelRole::Publisher, 0).unwrap();
        let subscriber_poll_set = registry.poll_set(ChannelRole::Subscriber, 1).unwrap();

        let writer = register_waiter(&publisher_poll_set, IoEvents::OUT);
        let reader = register_waiter(&subscriber_poll_set, IoEvents::IN);

        assert_eq!(registry.handle_notify_irq(), IrqReturn::Wake);
        assert_eq!(writer.count(), 1);
        assert_eq!(reader.count(), 1);
    }

    #[test]
    fn notify_irq_bridge_reports_handled_without_waiters() {
        let registry = AxivcRegistry::new_with_notify_irq(2, None);

        assert_eq!(registry.handle_notify_irq(), IrqReturn::Handled);
    }
}
