use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ax_hal::mem::{PhysAddr, VirtAddr, virt_to_phys};
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsError, VfsResult};
use axhvc::ivc::{self, IvcGuestPhysAddr};
use axivc::{IVC_SLOT_PAYLOAD_SIZE, IvcConsumer, IvcMessageKind, IvcProducer, IvcRegion};
use bytemuck::AnyBitPattern;
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    pseudofs::{Device, DeviceOps, DirMapping, SimpleFs},
    sync::Mutex,
};

const MAX_CHANNELS: usize = 16;
const AXIVC_MANAGER_MINOR: u32 = 240;
const AXIVC_PUBLISHER_MINOR_BASE: u32 = 241;
const AXIVC_SUBSCRIBER_MINOR_BASE: u32 = 257;
const REGION_READY_RETRIES: usize = 100;
const REGION_READY_DELAY_MS: u64 = 10;

const IVC_PUBLISH_CHANNEL: u32 = 0x4050_0000;
const IVC_UNPUBLISH_CHANNEL: u32 = 0x4050_0001;
const IVC_SUBSCRIBE_CHANNEL: u32 = 0x4050_0002;
const IVC_UNSUBSCRIBE_CHANNEL: u32 = 0x4050_0003;

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

struct AxivcRegistry {
    inner: Mutex<RegistryInner>,
}

impl AxivcRegistry {
    fn new(channel_count: usize) -> Self {
        Self {
            inner: Mutex::new(RegistryInner::new(channel_count)),
        }
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
        region.initialize();
        region.publish_header_volatile();
        let region: &'static IvcRegion = region;
        let (producer, consumer) = unsafe { region.publisher_endpoints() }.into_parts();

        let mut inner = self.inner.lock();
        if inner.publisher_exists(arg.channel_key) {
            let _ = ivc::unpublish_channel(arg.channel_key as usize);
            return Err(VfsError::InvalidInput);
        }
        let Some(index) = inner.insert_publisher(ChannelState {
            publisher_id: 0,
            notify_target_vm_id: None,
            key: arg.channel_key,
            producer,
            consumer,
            sequence: AtomicU64::new(1),
        }) else {
            let _ = ivc::unpublish_channel(arg.channel_key as usize);
            return Err(VfsError::NoMemory);
        };
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
        self.inner.lock().remove_publisher(arg.channel_key);
        ivc::unpublish_channel(arg.channel_key as usize).map_err(hvc_error)
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
        let Some(index) = inner.insert_subscriber(ChannelState {
            publisher_id: arg.target_publisher_id as usize,
            notify_target_vm_id: Some(arg.target_publisher_id as usize),
            key: arg.channel_key,
            producer,
            consumer,
            sequence: AtomicU64::new(1),
        }) else {
            unsubscribe_hvc(arg);
            return Err(VfsError::NoMemory);
        };
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
        self.inner
            .lock()
            .remove_subscriber(arg.target_publisher_id as usize, arg.channel_key);
        ivc::unsubscribe_channel(arg.target_publisher_id as usize, arg.channel_key as usize)
            .map_err(hvc_error)
    }
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

    fn insert_publisher(&mut self, state: ChannelState) -> Option<usize> {
        insert_channel(&mut self.publishers, state)
    }

    fn insert_subscriber(&mut self, state: ChannelState) -> Option<usize> {
        insert_channel(&mut self.subscribers, state)
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

    fn channel_mut(&mut self, role: ChannelRole, index: usize) -> Option<&mut ChannelState> {
        match role {
            ChannelRole::Publisher => self.publishers.get_mut(index)?.as_mut(),
            ChannelRole::Subscriber => self.subscribers.get_mut(index)?.as_mut(),
        }
    }
}

fn insert_channel(channels: &mut [Option<ChannelState>], state: ChannelState) -> Option<usize> {
    let index = channels.iter().position(Option::is_none)?;
    channels[index] = Some(state);
    Some(index)
}

struct ChannelState {
    publisher_id: usize,
    notify_target_vm_id: Option<usize>,
    key: u64,
    producer: IvcProducer<'static>,
    consumer: IvcConsumer<'static>,
    sequence: AtomicU64,
}

pub(super) fn register_devices(root: &mut DirMapping, fs: Arc<SimpleFs>) {
    let registry = Arc::new(AxivcRegistry::new(MAX_CHANNELS));
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
        let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
        let message = state.consumer.try_recv(&mut payload).map_err(ring_error)?;
        let Some(message) = message else {
            return Err(VfsError::WouldBlock);
        };

        let len = message.len().min(buf.len());
        let record_len = IVC_SLOT_PAYLOAD_SIZE.min(buf.len());
        buf[..len].copy_from_slice(&payload[..len]);
        buf[len..record_len].fill(0);
        if matches!(self.role, ChannelRole::Subscriber) {
            notify_peer(state, self.role_name());
        }
        info!(
            "axivc: read role={} key={:#x} seq={} kind={:?} len={}",
            self.role_name(),
            state.key,
            message.sequence(),
            message.kind(),
            len
        );
        Ok(record_len)
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
        let sequence = state.sequence.fetch_add(1, Ordering::Relaxed);
        let kind = match self.role {
            ChannelRole::Publisher => IvcMessageKind::Request,
            ChannelRole::Subscriber => IvcMessageKind::Ack,
        };

        state
            .producer
            .send(kind, sequence, buf)
            .map_err(ring_error)?;
        if matches!(self.role, ChannelRole::Subscriber) {
            notify_peer(state, self.role_name());
        }
        info!(
            "axivc: write role={} key={:#x} seq={} len={}",
            self.role_name(),
            state.key,
            sequence,
            buf.len()
        );
        Ok(buf.len())
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::BLOCKING | NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl AxivcChannel {
    fn role_name(&self) -> &'static str {
        match self.role {
            ChannelRole::Publisher => "publisher",
            ChannelRole::Subscriber => "subscriber",
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
    let vaddr = ax_mm::iomap_uncached(PhysAddr::from_usize(shm_base_gpa), shm_size)
        .map_err(|_| VfsError::NoMemory)?;
    Ok(unsafe { &mut *(vaddr.as_mut_ptr() as *mut IvcRegion) })
}

fn shared_page_ref(shm_base_gpa: usize, shm_size: usize) -> VfsResult<&'static IvcRegion> {
    let vaddr = ax_mm::iomap_uncached(PhysAddr::from_usize(shm_base_gpa), shm_size)
        .map_err(|_| VfsError::NoMemory)?;
    Ok(unsafe { &*(vaddr.as_ptr() as *const IvcRegion) })
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
