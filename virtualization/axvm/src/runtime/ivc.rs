// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Inter-VM communication (IVC) module.
use std::{
    collections::{BTreeMap, btree_map::Entry},
    format,
    sync::{Arc, Mutex, MutexGuard},
    vec,
    vec::Vec,
};

use ax_memory_addr::{PAGE_SIZE_4K, align_up_4k, is_aligned_4k};
use axdevice::{
    DeviceManagerError, DeviceManagerResult, DeviceRuntime, ServiceCardinality, ServiceKey,
};
use axdevice_base::IrqLine;

use crate::{
    AxVmError, AxVmResult, GuestPhysAddr, HostPhysAddr, MappingFlags, ax_err_type,
    host::PagingHandler, sync::MutexExt,
};

/// A global btree map to store IVC channels,
/// indexed by (publisher_vm_id, channel_key).
type HostIVCChannel = IVCChannel<crate::HostPagingHandler>;

static IVC_CHANNELS: Mutex<BTreeMap<(usize, usize), HostIVCChannel>> = Mutex::new(BTreeMap::new());

/// Maximum size of one IVC channel's shared region.
///
/// Requests larger than this are truncated; the hypercall ABI always writes
/// the actual granted size back to the guest, so guests must check it.
pub const MAX_IVC_CHANNEL_SIZE: usize = 0x100_0000;
const IVC_NOTIFY_PEER: usize = usize::MAX;

/// Stage-2 attributes for AXIVC shared pages.
///
/// The shared region is CPU-owned Normal Write-Back memory. Guests that map the
/// same physical pages through StarryOS, Linux or Zephyr must use compatible WB
/// attributes; DEVICE/UNCACHED aliases are not part of the AXIVC contract.
pub(crate) fn shared_memory_mapping_flags() -> MappingFlags {
    MappingFlags::READ | MappingFlags::WRITE
}

/// Allocates guest-physical bindings inside one graph-owned IVC MMIO aperture.
pub(crate) trait IvcApertureAllocator: Send + Sync {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr>;

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult;
}

/// Type key for a VM's IVC aperture allocator service.
pub(crate) struct IvcApertureAllocatorKey;

impl ServiceKey for IvcApertureAllocatorKey {
    type Service = dyn IvcApertureAllocator;

    const NAME: &'static str = "ivc-aperture-allocator";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-local endpoint used by IVC peer notification.
pub(crate) trait IvcNotifyEndpoint: Send + Sync {
    fn notify(&self) -> DeviceManagerResult;

    fn input(&self) -> usize;
}

/// IVC notify endpoint backed by one graph-owned wired IRQ line.
pub(crate) struct WiredIvcNotifyEndpoint {
    line: IrqLine,
}

impl WiredIvcNotifyEndpoint {
    /// Wraps a planned wired IRQ line as an IVC notify endpoint.
    pub(crate) const fn new(line: IrqLine) -> Self {
        Self { line }
    }
}

impl IvcNotifyEndpoint for WiredIvcNotifyEndpoint {
    fn notify(&self) -> DeviceManagerResult {
        self.line.pulse().map_err(DeviceManagerError::from)
    }

    fn input(&self) -> usize {
        self.line.input().value()
    }
}

/// Type key for the optional VM-local IRQ endpoint used by IVC peer notification.
pub(crate) struct IvcNotifyEndpointKey;

impl ServiceKey for IvcNotifyEndpointKey {
    type Service = dyn IvcNotifyEndpoint;

    const NAME: &'static str = "ivc-notify-endpoint";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Default allocator for an IVC MMIO aperture claimed by an IVC device model.
pub(crate) struct IvcAperturePool {
    ranges: Mutex<RangeAllocator>,
}

impl IvcAperturePool {
    fn ranges(&self) -> MutexGuard<'_, RangeAllocator> {
        self.ranges.lock_unpoisoned()
    }

    /// Creates an allocator over one non-empty, page-aligned range.
    pub(crate) fn new(base: usize, length: usize) -> DeviceManagerResult<Self> {
        let end = base
            .checked_add(length)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "create IVC aperture allocator",
                detail: "IVC aperture overflows the address space".into(),
            })?;
        if length == 0 || !is_aligned_4k(base) || !is_aligned_4k(length) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "create IVC aperture allocator",
                detail: format!(
                    "base {base:#x} and length {length:#x} must be non-zero and 4 KiB aligned"
                ),
            });
        }
        Ok(Self {
            ranges: Mutex::new(RangeAllocator::new(base..end)),
        })
    }

    /// Converts this pool into the typed runtime service capability.
    pub(crate) fn into_service(self) -> Arc<dyn IvcApertureAllocator> {
        Arc::new(self)
    }
}

impl IvcApertureAllocator for IvcAperturePool {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
        validate_aperture_size(size, "allocate IVC aperture range")?;
        self.ranges()
            .allocate(size)
            .map(|range| GuestPhysAddr::from_usize(range.start))
            .ok_or(DeviceManagerError::OutOfMemory {
                operation: "allocate IVC aperture range",
            })
    }

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        validate_aperture_size(size, "release IVC aperture range")?;
        let end =
            addr.as_usize()
                .checked_add(size)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "release IVC aperture range",
                    detail: "IVC aperture range end overflows the address space".into(),
                })?;
        if self.ranges().release(addr.as_usize()..end) {
            Ok(())
        } else {
            Err(DeviceManagerError::InvalidInput {
                operation: "release IVC aperture range",
                detail: format!(
                    "range {:#x}..{end:#x} is outside the pool or is not allocated",
                    addr.as_usize()
                ),
            })
        }
    }
}

pub(crate) fn alloc_guest_binding(
    devices: &DeviceRuntime,
    size: usize,
) -> DeviceManagerResult<GuestPhysAddr> {
    devices.service::<IvcApertureAllocatorKey>()?.allocate(size)
}

pub(crate) fn release_guest_binding(
    devices: &DeviceRuntime,
    addr: GuestPhysAddr,
    size: usize,
) -> DeviceManagerResult {
    devices
        .service::<IvcApertureAllocatorKey>()?
        .release(addr, size)
}

pub(crate) fn notify_peer(devices: &DeviceRuntime) -> DeviceManagerResult<Option<usize>> {
    match devices.service::<IvcNotifyEndpointKey>() {
        Ok(endpoint) => {
            endpoint.notify()?;
            Ok(Some(endpoint.input()))
        }
        Err(DeviceManagerError::ResourceNotFound { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn validate_aperture_size(size: usize, operation: &'static str) -> DeviceManagerResult {
    if size == 0 || !is_aligned_4k(size) {
        Err(DeviceManagerError::InvalidInput {
            operation,
            detail: format!("size {size:#x} must be non-zero and 4 KiB aligned"),
        })
    } else {
        Ok(())
    }
}

struct RangeAllocator {
    initial: core::ops::Range<usize>,
    free: Vec<core::ops::Range<usize>>,
}

impl RangeAllocator {
    fn new(range: core::ops::Range<usize>) -> Self {
        Self {
            initial: range.clone(),
            free: vec![range],
        }
    }

    fn allocate(&mut self, size: usize) -> Option<core::ops::Range<usize>> {
        let index = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, range)| range.end - range.start >= size)
            .min_by_key(|(_, range)| range.end - range.start)
            .map(|(index, _)| index)?;
        let start = self.free[index].start;
        let end = start + size;
        if self.free[index].end == end {
            self.free.remove(index);
        } else {
            self.free[index].start = end;
        }
        Some(start..end)
    }

    fn release(&mut self, range: core::ops::Range<usize>) -> bool {
        if range.start >= range.end
            || range.start < self.initial.start
            || range.end > self.initial.end
        {
            return false;
        }
        let index = self
            .free
            .iter()
            .position(|free| free.start > range.start)
            .unwrap_or(self.free.len());
        if index > 0 && self.free[index - 1].end > range.start
            || index < self.free.len() && range.end > self.free[index].start
        {
            return false;
        }
        if index > 0 && self.free[index - 1].end == range.start {
            self.free[index - 1].end = range.end;
            if index < self.free.len() && self.free[index - 1].end == self.free[index].start {
                let next = self.free.remove(index);
                self.free[index - 1].end = next.end;
            }
        } else if index < self.free.len() && range.end == self.free[index].start {
            self.free[index].start = range.start;
        } else {
            self.free.insert(index, range);
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcNotifyRoute {
    pub source_vm_id: usize,
    pub target_vm_id: usize,
    pub publisher_vm_id: usize,
    pub key: usize,
}

/// One guest-visible IVC mapping owned by a VM runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IvcGuestBinding {
    pub gpa: GuestPhysAddr,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IvcTeardownAction {
    Publisher {
        publisher_vm_id: usize,
        key: usize,
    },
    Subscriber {
        publisher_vm_id: usize,
        key: usize,
        subscriber_vm_id: usize,
    },
}

/// A prepared IVC teardown step whose backing frames are still kept alive.
///
/// Creating this transaction only blocks new IVC operations on the endpoint and
/// returns the guest binding that must be unmapped and released by the owning VM.
/// The channel entry, and therefore the shared HPA backing, is removed only when
/// [`Self::commit`] is called after stage-2 unmap and aperture release succeed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IvcTeardown {
    binding: IvcGuestBinding,
    action: IvcTeardownAction,
}

impl IvcTeardown {
    fn publisher(publisher_vm_id: usize, key: usize, binding: IvcGuestBinding) -> Self {
        Self {
            binding,
            action: IvcTeardownAction::Publisher {
                publisher_vm_id,
                key,
            },
        }
    }

    fn subscriber(
        publisher_vm_id: usize,
        key: usize,
        subscriber_vm_id: usize,
        binding: IvcGuestBinding,
    ) -> Self {
        Self {
            binding,
            action: IvcTeardownAction::Subscriber {
                publisher_vm_id,
                key,
                subscriber_vm_id,
            },
        }
    }

    pub(crate) const fn binding(&self) -> IvcGuestBinding {
        self.binding
    }

    pub(crate) fn commit(self) {
        let mut channels = IVC_CHANNELS.lock_unpoisoned();
        commit_teardown_action(&mut channels, self.action);
    }
}

pub fn insert_channel(publisher_vm_id: usize, channel: HostIVCChannel) -> AxVmResult<()> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
    let channel_key = (publisher_vm_id, channel.key);
    match channels.entry(channel_key) {
        Entry::Vacant(entry) => {
            entry.insert(channel);
            Ok(())
        }
        Entry::Occupied(_) => Err(ax_err_type!(AlreadyExists, "IVC channel already exists")),
    }
}

/// Removes every global IVC binding owned by one VM.
///
/// The returned bindings are the caller VM's guest GPA windows that must be
/// unmapped and released from its graph-owned IVC aperture allocator. A
/// subscriber teardown detaches only that subscriber. A publisher teardown
/// removes the publisher's GPA binding and either drops the whole channel when
/// no subscriber remains, or keeps the backing frame alive until the last
/// subscriber tears down.
pub(crate) fn teardown_vm(vm_id: usize) -> Vec<IvcTeardown> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
    teardown_vm_from_channels(&mut channels, vm_id)
}

pub fn ensure_channel_absent(publisher_vm_id: usize, key: usize) -> AxVmResult<()> {
    if IVC_CHANNELS
        .lock_unpoisoned()
        .contains_key(&(publisher_vm_id, key))
    {
        Err(ax_err_type!(
            AlreadyExists,
            format!(
                "IVC channel for publisher VM {} with key {} already exists",
                publisher_vm_id, key
            )
        ))
    } else {
        Ok(())
    }
}

/// Try to remove a channel according to the publisher VM ID and key.
/// If the channel still has subscribers, it will just mark it as unpublished
/// (by setting its base GPA to None).
/// If the channel is successfully unpublished, it will return the base GPA and size of the channel.
/// If the channel does not exist, it will return an error.
pub fn unpublish_channel(publisher_vm_id: usize, key: usize) -> AxVmResult<IvcTeardown> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
    let channel_key = (publisher_vm_id, key);
    let channel = channels.get_mut(&channel_key).ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM {} with key {} not found",
                publisher_vm_id, key
            )
        )
    })?;
    channel.begin_publisher_teardown(channel_key)
}

pub fn prepare_subscribe_channel(
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
) -> AxVmResult<usize> {
    let channels = IVC_CHANNELS.lock_unpoisoned();
    prepare_subscribe_channel_from_channels(&channels, publisher_vm_id, key, subscriber_vm_id)
}

fn prepare_subscribe_channel_from_channels<H: PagingHandler>(
    channels: &BTreeMap<(usize, usize), IVCChannel<H>>,
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
) -> AxVmResult<usize> {
    let channel = channels.get(&(publisher_vm_id, key)).ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM {} with key {} not found",
                publisher_vm_id, key
            )
        )
    })?;

    if channel.is_unpublished() {
        return Err(ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM {} with key {} has been unpublished",
                publisher_vm_id, key
            )
        ));
    }
    channel.ensure_subscriber_available(subscriber_vm_id)?;

    Ok(channel.size())
}

fn subscribe_to_channel_from_channels<H: PagingHandler>(
    channels: &mut BTreeMap<(usize, usize), IVCChannel<H>>,
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
    subscriber_gpa: GuestPhysAddr,
) -> AxVmResult<(HostPhysAddr, usize)> {
    let channel = channels.get_mut(&(publisher_vm_id, key)).ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM [{}] key {:#x} not found",
                publisher_vm_id, key
            )
        )
    })?;
    if channel.is_unpublished() {
        return Err(ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM [{}] key {:#x} has been unpublished",
                publisher_vm_id, key
            )
        ));
    }
    // Register while holding the channel-table lock. This final check closes
    // the gap after `prepare_subscribe_channel()` and preserves the SPSC
    // protocol's one-subscriber invariant.
    channel.add_subscriber(subscriber_vm_id, subscriber_gpa)?;
    Ok((channel.base_hpa(), channel.size()))
}

/// Subcribe to a channel of a publisher VM with the given key,
/// return the shared region base address and size.
pub fn subscribe_to_channel_of_publisher(
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
    subscriber_gpa: GuestPhysAddr,
) -> AxVmResult<(HostPhysAddr, usize)> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
    subscribe_to_channel_from_channels(
        &mut channels,
        publisher_vm_id,
        key,
        subscriber_vm_id,
        subscriber_gpa,
    )
}

/// Unsubscribe from a channel of a publisher VM with the given key,
/// if the channel has been unpublished (i.e., the base GPA is None) and has no subscribers,
/// it will remove the channel from the global map.
pub fn unsubscribe_from_channel_of_publisher(
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
) -> AxVmResult<IvcTeardown> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
    let channel_key = (publisher_vm_id, key);
    let channel = channels.get_mut(&channel_key).ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!("IVC channel for publisher VM {} not found", publisher_vm_id)
        )
    })?;
    channel.begin_subscriber_teardown(channel_key, subscriber_vm_id)
}

pub fn prepare_notify_channel(
    publisher_vm_id: usize,
    key: usize,
    source_vm_id: usize,
    target_vm_id: usize,
) -> AxVmResult<IvcNotifyRoute> {
    let channels = IVC_CHANNELS.lock_unpoisoned();
    prepare_notify_channel_from_channels(
        &channels,
        publisher_vm_id,
        key,
        source_vm_id,
        target_vm_id,
    )
}

fn prepare_notify_channel_from_channels<H: PagingHandler>(
    channels: &BTreeMap<(usize, usize), IVCChannel<H>>,
    publisher_vm_id: usize,
    key: usize,
    source_vm_id: usize,
    mut target_vm_id: usize,
) -> AxVmResult<IvcNotifyRoute> {
    let channel = channels.get(&(publisher_vm_id, key)).ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM [{}] key {:#x} not found",
                publisher_vm_id, key
            )
        )
    })?;

    if channel.is_unpublished() {
        return Err(ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM [{}] key {:#x} has been unpublished",
                publisher_vm_id, key
            )
        ));
    }

    if target_vm_id == IVC_NOTIFY_PEER {
        target_vm_id = channel.peer_vm_id_for(source_vm_id).ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                format!(
                    "VM[{}] has no notifiable peer on IVC channel publisher VM[{}] key {:#x}",
                    source_vm_id, publisher_vm_id, key
                )
            )
        })?;
    }

    let source_can_notify_target = if source_vm_id == publisher_vm_id {
        channel.has_subscriber(target_vm_id)
    } else {
        channel.has_subscriber(source_vm_id) && target_vm_id == publisher_vm_id
    };
    if !source_can_notify_target {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "VM[{}] cannot notify VM[{}] on IVC channel publisher VM[{}] key {:#x}",
                source_vm_id, target_vm_id, publisher_vm_id, key
            )
        ));
    }

    Ok(IvcNotifyRoute {
        source_vm_id,
        target_vm_id,
        publisher_vm_id,
        key,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IvcSubscriberBinding {
    vm_id: usize,
    base_gpa: GuestPhysAddr,
    closing: bool,
}

pub struct IVCChannel<H: PagingHandler> {
    publisher_vm_id: usize,
    key: usize,
    /// The guest mapping attached to the channel's sole subscriber.
    ///
    /// The current guest-visible protocol uses one SPSC ring in each
    /// direction, so attaching a second subscriber would create multiple
    /// producers and consumers for those rings.
    subscriber: Option<IvcSubscriberBinding>,
    shared_region_base: HostPhysAddr,
    shared_region_size: usize,
    /// Publisher GPA binding for the shared region.
    ///
    /// `None` after publisher teardown commits. `publisher_closing` blocks new
    /// operations while the VM is unmapping this GPA but before the channel
    /// entry can be removed safely.
    base_gpa: Option<GuestPhysAddr>,
    publisher_closing: bool,
    _phantom: std::marker::PhantomData<H>,
}

#[repr(C)]
pub struct IVCChannelHeader {
    pub publisher_id: u64,
    pub key: u64,
}

impl<H: PagingHandler> IVCChannel<H> {
    #[allow(unused)]
    /// # Safety
    ///
    /// The caller must ensure `shared_region_base` is valid, mapped, and aligned
    /// for `IVCChannelHeader`, and that no mutable reference to this region exists.
    pub fn header(&self) -> &IVCChannelHeader {
        unsafe {
            // Map the shared region base to the header structure.
            &*H::phys_to_virt(self.shared_region_base).as_mut_ptr_of::<IVCChannelHeader>()
        }
    }

    /// # Safety
    ///
    /// The caller must ensure `shared_region_base` is valid, mapped, and aligned
    /// for `IVCChannelHeader`, and that no other reference to this region exists.
    pub fn header_mut(&mut self) -> &mut IVCChannelHeader {
        unsafe {
            // Map the shared region base to the mutable header structure.
            &mut *H::phys_to_virt(self.shared_region_base).as_mut_ptr_of::<IVCChannelHeader>()
        }
    }

    #[allow(unused)]
    /// # Safety
    ///
    /// The caller must ensure `shared_region_base` is valid, mapped, and that the
    /// returned pointer is only used within the lifetime of `self` and does not alias
    /// any mutable reference to the same region.
    pub fn data_region(&self) -> *const u8 {
        unsafe {
            // Return a pointer to the data region, which starts after the header.
            H::phys_to_virt(self.shared_region_base)
                .as_mut_ptr()
                .add(std::mem::size_of::<IVCChannelHeader>())
        }
    }
}

impl<H: PagingHandler> std::fmt::Debug for IVCChannel<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IVCChannel(publisher[{}], subscriber {:?}, base: {:?}, size: {:#x}, gpa: {:?})",
            self.publisher_vm_id,
            self.subscriber,
            self.shared_region_base,
            self.shared_region_size,
            self.base_gpa
        )
    }
}

impl<H: PagingHandler> Drop for IVCChannel<H> {
    fn drop(&mut self) {
        // Free the shared region frames when the channel is dropped.
        debug!(
            "Dropping IVCChannel for VM[{}], shared region base: {:?}",
            self.publisher_vm_id, self.shared_region_base
        );
        H::dealloc_frames(self.shared_region_base, self.page_count());
    }
}

impl<H: PagingHandler> IVCChannel<H> {
    pub fn alloc(
        publisher_vm_id: usize,
        key: usize,
        shared_region_size: usize,
        base_gpa: GuestPhysAddr,
    ) -> AxVmResult<Self> {
        let requested_size = align_up_4k(shared_region_size);
        let shared_region_size = requested_size.min(MAX_IVC_CHANNEL_SIZE);
        if shared_region_size == 0 {
            return Err(ax_err_type!(
                InvalidInput,
                "IVC channel shared region size must be greater than 0"
            ));
        }
        if shared_region_size < requested_size {
            warn!(
                "IVC channel requested size {requested_size:#x} exceeds \
                 {MAX_IVC_CHANNEL_SIZE:#x}; truncating to {MAX_IVC_CHANNEL_SIZE:#x}"
            );
        }
        let shared_region_base = H::alloc_frames(shared_region_size / PAGE_SIZE_4K, PAGE_SIZE_4K)
            .ok_or(AxVmError::OutOfMemory {
            operation: "allocate IVC shared region frames",
        })?;
        unsafe {
            core::ptr::write_bytes(
                H::phys_to_virt(shared_region_base).as_mut_ptr(),
                0,
                shared_region_size,
            );
        }

        let mut channel = IVCChannel {
            publisher_vm_id,
            key,
            subscriber: None,
            shared_region_base,
            shared_region_size,
            base_gpa: Some(base_gpa),
            publisher_closing: false,
            _phantom: std::marker::PhantomData,
        };

        {
            let header = channel.header_mut();
            header.publisher_id = publisher_vm_id as u64;
            header.key = key as u64;
        }
        // Make the host-initialized channel header visible through the guest
        // WB mappings before the shared pages are first exposed.
        H::clean_dcache_range(shared_region_base, shared_region_size);

        debug!("Allocated IVCChannel: {channel:?}");

        Ok(channel)
    }

    pub fn base_hpa(&self) -> HostPhysAddr {
        self.shared_region_base
    }

    pub fn size(&self) -> usize {
        self.shared_region_size
    }

    /// Number of 4K frames backing the shared region.
    ///
    /// `alloc()` guarantees the size is page-aligned, so this is exact.
    fn page_count(&self) -> usize {
        self.shared_region_size / PAGE_SIZE_4K
    }

    pub fn add_subscriber(
        &mut self,
        subscriber_vm_id: usize,
        subscriber_gpa: GuestPhysAddr,
    ) -> AxVmResult<()> {
        self.ensure_subscriber_available(subscriber_vm_id)?;
        self.subscriber = Some(IvcSubscriberBinding {
            vm_id: subscriber_vm_id,
            base_gpa: subscriber_gpa,
            closing: false,
        });
        Ok(())
    }

    fn has_subscriber_binding(&self) -> bool {
        self.subscriber.is_some()
    }

    pub fn has_subscriber(&self, subscriber_vm_id: usize) -> bool {
        self.subscriber
            .is_some_and(|binding| binding.vm_id == subscriber_vm_id && !binding.closing)
    }

    fn peer_vm_id_for(&self, source_vm_id: usize) -> Option<usize> {
        if source_vm_id == self.publisher_vm_id {
            self.subscriber
                .filter(|binding| !binding.closing)
                .map(|binding| binding.vm_id)
        } else if self.has_subscriber(source_vm_id) {
            Some(self.publisher_vm_id)
        } else {
            None
        }
    }

    fn ensure_subscriber_available(&self, subscriber_vm_id: usize) -> AxVmResult<()> {
        let Some(binding) = self.subscriber else {
            return Ok(());
        };

        if binding.vm_id == subscriber_vm_id {
            Err(ax_err_type!(
                AlreadyExists,
                format!(
                    "VM[{}] has already subscribed to publisher VM[{}] Key {:#x}",
                    subscriber_vm_id, self.publisher_vm_id, self.key
                )
            ))
        } else {
            Err(ax_err_type!(
                AlreadyExists,
                format!(
                    "IVC channel publisher VM[{}] Key {:#x} already has subscriber VM[{}]; the \
                     SPSC protocol permits only one subscriber",
                    self.publisher_vm_id, self.key, binding.vm_id
                )
            ))
        }
    }

    pub fn mark_unpublished(&mut self) {
        self.base_gpa = None;
        self.publisher_closing = false;
    }

    pub fn is_unpublished(&self) -> bool {
        self.base_gpa.is_none() || self.publisher_closing
    }

    fn can_drop_backing(&self) -> bool {
        self.base_gpa.is_none() && !self.has_subscriber_binding()
    }

    fn begin_publisher_teardown(&mut self, channel_key: (usize, usize)) -> AxVmResult<IvcTeardown> {
        let base_gpa = self.base_gpa.ok_or_else(|| {
            ax_err_type!(
                NotFound,
                format!(
                    "IVC channel for publisher VM {} with key {} has no base GPA, it may have \
                     been marked as unpublished",
                    channel_key.0, channel_key.1
                )
            )
        })?;
        self.publisher_closing = true;
        Ok(IvcTeardown::publisher(
            channel_key.0,
            channel_key.1,
            IvcGuestBinding {
                gpa: base_gpa,
                size: self.size(),
            },
        ))
    }

    fn begin_subscriber_teardown(
        &mut self,
        channel_key: (usize, usize),
        subscriber_vm_id: usize,
    ) -> AxVmResult<IvcTeardown> {
        let Some(mut binding) = self
            .subscriber
            .filter(|binding| binding.vm_id == subscriber_vm_id)
        else {
            return Err(ax_err_type!(
                NotFound,
                format!(
                    "VM[{}] tries to unsubscribe non-existed channel publisher VM[{}] Key {:#x}",
                    subscriber_vm_id, channel_key.0, channel_key.1
                )
            ));
        };
        binding.closing = true;
        self.subscriber = Some(binding);
        Ok(IvcTeardown::subscriber(
            channel_key.0,
            channel_key.1,
            subscriber_vm_id,
            IvcGuestBinding {
                gpa: binding.base_gpa,
                size: self.size(),
            },
        ))
    }
}

fn teardown_vm_from_channels<H: PagingHandler>(
    channels: &mut BTreeMap<(usize, usize), IVCChannel<H>>,
    vm_id: usize,
) -> Vec<IvcTeardown> {
    let mut teardowns = Vec::new();

    for (channel_key, channel) in channels.iter_mut() {
        if channel.publisher_vm_id == vm_id
            && let Ok(teardown) = channel.begin_publisher_teardown(*channel_key)
        {
            teardowns.push(teardown);
        }

        if let Ok(teardown) = channel.begin_subscriber_teardown(*channel_key, vm_id) {
            teardowns.push(teardown);
        }
    }

    teardowns
}

fn commit_teardown_action<H: PagingHandler>(
    channels: &mut BTreeMap<(usize, usize), IVCChannel<H>>,
    action: IvcTeardownAction,
) {
    match action {
        IvcTeardownAction::Publisher {
            publisher_vm_id,
            key,
        } => {
            let channel_key = (publisher_vm_id, key);
            let remove_channel = if let Some(channel) = channels.get_mut(&channel_key) {
                if channel.publisher_closing {
                    channel.mark_unpublished();
                }
                channel.can_drop_backing()
            } else {
                false
            };
            if remove_channel {
                channels.remove(&channel_key);
            }
        }
        IvcTeardownAction::Subscriber {
            publisher_vm_id,
            key,
            subscriber_vm_id,
        } => {
            let channel_key = (publisher_vm_id, key);
            let remove_channel = if let Some(channel) = channels.get_mut(&channel_key) {
                if channel
                    .subscriber
                    .as_ref()
                    .is_some_and(|binding| binding.vm_id == subscriber_vm_id && binding.closing)
                {
                    channel.subscriber = None;
                }
                channel.can_drop_backing()
            } else {
                false
            };
            if remove_channel {
                channels.remove(&channel_key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::{sync::Mutex, vec::Vec};

    use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};

    use super::*;

    const TEST_ARENA_SIZE: usize = 128 * 1024 * 1024;

    /// A [`PagingHandler`] backed by a bump allocator over one leaked arena.
    ///
    /// The arena address doubles as both host physical and host virtual
    /// address so that `header_mut()` writes land in real memory. Allocation
    /// and deallocation calls are recorded for assertions.
    struct MockPagingHandler;

    static ARENA_OFFSET: AtomicUsize = AtomicUsize::new(0);
    static ALLOC_FRAMES_CALLS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    static DEALLOC_FRAME_CALLS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    static DEALLOC_FRAMES_CALLS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    static DCACHE_CLEAN_CALLS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

    fn arena_base() -> usize {
        static BASE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *BASE.get_or_init(|| {
            let layout =
                std::alloc::Layout::from_size_align(TEST_ARENA_SIZE, PAGE_SIZE_4K).unwrap();
            // Safety: layout has non-zero size; null is checked immediately.
            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            assert!(!ptr.is_null());
            ptr as usize
        })
    }

    fn bump_alloc_pages(num_pages: usize) -> Option<PhysAddr> {
        let bytes = num_pages * PAGE_SIZE_4K;
        let offset = ARENA_OFFSET.fetch_add(bytes, Ordering::Relaxed);
        if offset + bytes > TEST_ARENA_SIZE {
            return None;
        }
        Some(PhysAddr::from_usize(arena_base() + offset))
    }

    fn poison_allocated_pages(base: PhysAddr, num_pages: usize) {
        unsafe {
            core::ptr::write_bytes(
                VirtAddr::from_usize(base.as_usize()).as_mut_ptr(),
                0xA5,
                num_pages * PAGE_SIZE_4K,
            );
        }
    }

    fn teardown_bindings(teardowns: &[IvcTeardown]) -> Vec<IvcGuestBinding> {
        teardowns.iter().map(IvcTeardown::binding).collect()
    }

    fn commit_teardown<H: PagingHandler>(
        channels: &mut BTreeMap<(usize, usize), IVCChannel<H>>,
        teardown: IvcTeardown,
    ) {
        commit_teardown_action(channels, teardown.action);
    }

    impl PagingHandler for MockPagingHandler {
        fn alloc_frame() -> Option<PhysAddr> {
            bump_alloc_pages(1)
        }

        fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
            assert!(align <= PAGE_SIZE_4K);
            ALLOC_FRAMES_CALLS.lock().unwrap().push((num, align));
            bump_alloc_pages(num)
        }

        fn dealloc_frame(paddr: PhysAddr) {
            DEALLOC_FRAME_CALLS.lock().unwrap().push(paddr.as_usize());
        }

        fn dealloc_frames(paddr: PhysAddr, num: usize) {
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .push((paddr.as_usize(), num));
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            VirtAddr::from_usize(paddr.as_usize())
        }

        fn clean_dcache_range(paddr: PhysAddr, size: usize) {
            DCACHE_CLEAN_CALLS
                .lock()
                .unwrap()
                .push((paddr.as_usize(), size));
        }
    }

    struct DirtyPagingHandler;

    impl PagingHandler for DirtyPagingHandler {
        fn alloc_frame() -> Option<PhysAddr> {
            let paddr = bump_alloc_pages(1)?;
            poison_allocated_pages(paddr, 1);
            Some(paddr)
        }

        fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
            assert!(align <= PAGE_SIZE_4K);
            let paddr = bump_alloc_pages(num)?;
            poison_allocated_pages(paddr, num);
            Some(paddr)
        }

        fn dealloc_frame(paddr: PhysAddr) {
            DEALLOC_FRAME_CALLS.lock().unwrap().push(paddr.as_usize());
        }

        fn dealloc_frames(paddr: PhysAddr, num: usize) {
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .push((paddr.as_usize(), num));
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            VirtAddr::from_usize(paddr.as_usize())
        }

        fn clean_dcache_range(paddr: PhysAddr, size: usize) {
            DCACHE_CLEAN_CALLS
                .lock()
                .unwrap()
                .push((paddr.as_usize(), size));
        }
    }

    #[test]
    fn ivc_stage2_mapping_flags_are_cacheable_normal_memory() {
        let flags = shared_memory_mapping_flags();

        assert!(flags.contains(MappingFlags::READ));
        assert!(flags.contains(MappingFlags::WRITE));
        assert!(!flags.contains(MappingFlags::DEVICE));
        assert!(!flags.contains(MappingFlags::UNCACHED));
    }

    #[test]
    fn allocation_cleans_shared_region_before_exposure() {
        let channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x106,
            2 * PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_6000),
        )
        .unwrap();
        let base = channel.base_hpa();

        assert!(
            DCACHE_CLEAN_CALLS
                .lock()
                .unwrap()
                .contains(&(base.as_usize(), 2 * PAGE_SIZE_4K))
        );
    }

    #[test]
    fn allocation_clears_dirty_reused_shared_region_before_exposure() {
        let channel = IVCChannel::<DirtyPagingHandler>::alloc(
            1,
            0x107,
            2 * PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_7000),
        )
        .unwrap();
        let header = channel.header();
        let region = unsafe {
            std::slice::from_raw_parts(
                DirtyPagingHandler::phys_to_virt(channel.base_hpa())
                    .as_ptr()
                    .add(core::mem::size_of::<IVCChannelHeader>()),
                channel.size() - core::mem::size_of::<IVCChannelHeader>(),
            )
        };

        assert_eq!(header.publisher_id, 1);
        assert_eq!(header.key, 0x107);
        assert!(region.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn allocates_contiguous_frames_for_multi_page_channel() {
        let channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x100,
            4 * PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_0000),
        )
        .unwrap();
        assert_eq!(channel.size(), 4 * PAGE_SIZE_4K);
        let base = channel.base_hpa();
        assert!(
            ALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .contains(&(4, PAGE_SIZE_4K))
        );

        drop(channel);
        assert!(
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .contains(&(base.as_usize(), 4))
        );
        assert!(DEALLOC_FRAME_CALLS.lock().unwrap().is_empty());
    }

    #[test]
    fn allocates_single_frame_for_page_sized_channel() {
        let channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x101,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_1000),
        )
        .unwrap();
        assert_eq!(channel.size(), PAGE_SIZE_4K);
    }

    #[test]
    fn truncates_channel_size_to_max() {
        let channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x102,
            2 * MAX_IVC_CHANNEL_SIZE,
            GuestPhysAddr::from_usize(0x7000_2000),
        )
        .unwrap();
        assert_eq!(channel.size(), MAX_IVC_CHANNEL_SIZE);
    }

    #[test]
    fn rejects_zero_sized_channel() {
        let result = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x103,
            0,
            GuestPhysAddr::from_usize(0x7000_3000),
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_second_subscriber_for_spsc_channel() {
        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x104,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_4000),
        )
        .unwrap();

        channel
            .add_subscriber(2, GuestPhysAddr::from_usize(0x7100_0000))
            .unwrap();
        assert!(channel.ensure_subscriber_available(3).is_err());
        let second = channel.add_subscriber(3, GuestPhysAddr::from_usize(0x7200_0000));

        assert!(second.is_err());
        assert!(channel.has_subscriber(2));
        assert!(!channel.has_subscriber(3));
    }

    #[test]
    fn prepared_subscriber_can_lose_the_race_to_late_registration() {
        let publisher_vm_id = 1;
        let key = 0x108;
        let first_subscriber_vm_id = 2;
        let second_subscriber_vm_id = 3;
        let first_subscriber_gpa = GuestPhysAddr::from_usize(0x7100_0000);
        let second_subscriber_gpa = GuestPhysAddr::from_usize(0x7200_0000);

        let mut channels: BTreeMap<(usize, usize), IVCChannel<MockPagingHandler>> = BTreeMap::new();
        channels.insert(
            (publisher_vm_id, key),
            IVCChannel::<MockPagingHandler>::alloc(
                publisher_vm_id,
                key,
                PAGE_SIZE_4K,
                GuestPhysAddr::from_usize(0x7000_8000),
            )
            .unwrap(),
        );

        assert_eq!(
            prepare_subscribe_channel_from_channels(
                &channels,
                publisher_vm_id,
                key,
                first_subscriber_vm_id,
            )
            .unwrap(),
            PAGE_SIZE_4K
        );

        assert_eq!(
            subscribe_to_channel_from_channels(
                &mut channels,
                publisher_vm_id,
                key,
                second_subscriber_vm_id,
                second_subscriber_gpa,
            )
            .unwrap()
            .1,
            PAGE_SIZE_4K
        );

        assert!(
            subscribe_to_channel_from_channels(
                &mut channels,
                publisher_vm_id,
                key,
                first_subscriber_vm_id,
                first_subscriber_gpa,
            )
            .is_err(),
            "the final subscribe step must re-check the channel and lose the race to the first \
             registrant"
        );
    }

    #[test]
    fn accepts_new_subscriber_after_current_subscriber_detaches() {
        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            0x105,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_5000),
        )
        .unwrap();
        let first_gpa = GuestPhysAddr::from_usize(0x7100_0000);

        channel.add_subscriber(2, first_gpa).unwrap();
        channel.subscriber = None;
        channel
            .add_subscriber(3, GuestPhysAddr::from_usize(0x7200_0000))
            .unwrap();

        assert!(!channel.has_subscriber(2));
        assert!(channel.has_subscriber(3));
    }

    #[test]
    fn publisher_teardown_marks_unpublished_and_keeps_live_subscriber() {
        let mut channels = BTreeMap::new();
        let key = (1, 0x200);
        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            key.1,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_0000),
        )
        .unwrap();
        let base_hpa = channel.base_hpa();
        channel
            .add_subscriber(2, GuestPhysAddr::from_usize(0x7100_0000))
            .unwrap();
        channels.insert(key, channel);

        let teardowns = teardown_vm_from_channels(&mut channels, 1);

        assert_eq!(
            teardown_bindings(&teardowns),
            [IvcGuestBinding {
                gpa: GuestPhysAddr::from_usize(0x7000_0000),
                size: PAGE_SIZE_4K
            }]
        );
        let channel = channels.get(&key).expect("subscriber keeps backing alive");
        assert!(channel.is_unpublished());
        assert!(channel.has_subscriber(2));
        assert!(
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .iter()
                .all(|(base, _)| *base != base_hpa.as_usize())
        );

        commit_teardown(&mut channels, teardowns[0]);
        let subscriber_teardowns = teardown_vm_from_channels(&mut channels, 2);

        assert_eq!(
            teardown_bindings(&subscriber_teardowns),
            [IvcGuestBinding {
                gpa: GuestPhysAddr::from_usize(0x7100_0000),
                size: PAGE_SIZE_4K
            }]
        );
        commit_teardown(&mut channels, subscriber_teardowns[0]);
        assert!(channels.is_empty());
        assert!(
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .contains(&(base_hpa.as_usize(), 1))
        );
    }

    #[test]
    fn subscriber_teardown_detaches_without_removing_published_channel() {
        let mut channels = BTreeMap::new();
        let key = (1, 0x201);
        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            1,
            key.1,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7200_0000),
        )
        .unwrap();
        channel
            .add_subscriber(2, GuestPhysAddr::from_usize(0x7300_0000))
            .unwrap();
        channels.insert(key, channel);

        let teardowns = teardown_vm_from_channels(&mut channels, 2);

        assert_eq!(
            teardown_bindings(&teardowns),
            [IvcGuestBinding {
                gpa: GuestPhysAddr::from_usize(0x7300_0000),
                size: PAGE_SIZE_4K
            }]
        );
        let channel = channels
            .get_mut(&key)
            .expect("publisher remains after subscriber teardown");
        assert!(!channel.is_unpublished());
        assert!(
            channel
                .subscriber
                .is_some_and(|binding| binding.vm_id == 2 && binding.closing)
        );
        commit_teardown(&mut channels, teardowns[0]);
        let channel = channels
            .get_mut(&key)
            .expect("publisher remains after subscriber teardown commits");
        channel
            .add_subscriber(3, GuestPhysAddr::from_usize(0x7400_0000))
            .unwrap();
        assert!(channel.has_subscriber(3));

        assert!(teardown_vm_from_channels(&mut channels, 2).is_empty());
    }

    #[test]
    fn notify_peer_sentinel_routes_publisher_to_subscriber() {
        let publisher_vm_id = 1;
        let subscriber_vm_id = 2;
        let key = 0x210;
        let mut channels = BTreeMap::new();
        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            publisher_vm_id,
            key,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_0000),
        )
        .unwrap();
        channel
            .add_subscriber(subscriber_vm_id, GuestPhysAddr::from_usize(0x7100_0000))
            .unwrap();
        channels.insert((publisher_vm_id, key), channel);

        let route = prepare_notify_channel_from_channels(
            &channels,
            publisher_vm_id,
            key,
            publisher_vm_id,
            IVC_NOTIFY_PEER,
        )
        .unwrap();

        assert_eq!(route.source_vm_id, publisher_vm_id);
        assert_eq!(route.target_vm_id, subscriber_vm_id);
        assert_eq!(route.publisher_vm_id, publisher_vm_id);
        assert_eq!(route.key, key);
    }

    #[test]
    fn notify_peer_sentinel_routes_subscriber_to_publisher() {
        let publisher_vm_id = 1;
        let subscriber_vm_id = 2;
        let key = 0x211;
        let mut channels = BTreeMap::new();
        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            publisher_vm_id,
            key,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_0000),
        )
        .unwrap();
        channel
            .add_subscriber(subscriber_vm_id, GuestPhysAddr::from_usize(0x7100_0000))
            .unwrap();
        channels.insert((publisher_vm_id, key), channel);

        let route = prepare_notify_channel_from_channels(
            &channels,
            publisher_vm_id,
            key,
            subscriber_vm_id,
            IVC_NOTIFY_PEER,
        )
        .unwrap();

        assert_eq!(route.source_vm_id, subscriber_vm_id);
        assert_eq!(route.target_vm_id, publisher_vm_id);
        assert_eq!(route.publisher_vm_id, publisher_vm_id);
        assert_eq!(route.key, key);
    }

    #[test]
    fn stopped_publisher_blocks_new_subscribers_and_releases_backing_after_last_peer() {
        let publisher_vm_id = 1;
        let subscriber_vm_id = 2;
        let new_subscriber_vm_id = 3;
        let key = 0x202;
        let mut channels = BTreeMap::new();

        let mut channel = IVCChannel::<MockPagingHandler>::alloc(
            publisher_vm_id,
            key,
            PAGE_SIZE_4K,
            GuestPhysAddr::from_usize(0x7000_0000),
        )
        .unwrap();
        let base_hpa = channel.base_hpa();
        channel
            .add_subscriber(subscriber_vm_id, GuestPhysAddr::from_usize(0x7100_0000))
            .unwrap();
        channels.insert((publisher_vm_id, key), channel);

        let publisher_teardowns = teardown_vm_from_channels(&mut channels, publisher_vm_id);

        assert_eq!(
            teardown_bindings(&publisher_teardowns),
            [IvcGuestBinding {
                gpa: GuestPhysAddr::from_usize(0x7000_0000),
                size: PAGE_SIZE_4K
            }]
        );
        assert!(
            prepare_subscribe_channel_from_channels(
                &channels,
                publisher_vm_id,
                key,
                new_subscriber_vm_id,
            )
            .is_err(),
            "a stopped publisher must not accept new subscribers"
        );
        assert!(!channels.is_empty());
        assert!(
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .iter()
                .all(|(base, _)| *base != base_hpa.as_usize()),
            "the live subscriber keeps the backing frames alive"
        );
        assert_eq!(
            teardown_bindings(&teardown_vm_from_channels(&mut channels, publisher_vm_id)),
            [IvcGuestBinding {
                gpa: GuestPhysAddr::from_usize(0x7000_0000),
                size: PAGE_SIZE_4K
            }],
            "a failed cleanup can be retried while the publisher binding is closing"
        );
        commit_teardown(&mut channels, publisher_teardowns[0]);

        let subscriber_teardowns = teardown_vm_from_channels(&mut channels, subscriber_vm_id);

        assert_eq!(
            teardown_bindings(&subscriber_teardowns),
            [IvcGuestBinding {
                gpa: GuestPhysAddr::from_usize(0x7100_0000),
                size: PAGE_SIZE_4K
            }]
        );
        assert!(
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .iter()
                .all(|(base, _)| *base != base_hpa.as_usize()),
            "beginning the final teardown still keeps backing alive until commit"
        );
        commit_teardown(&mut channels, subscriber_teardowns[0]);
        assert!(
            DEALLOC_FRAMES_CALLS
                .lock()
                .unwrap()
                .contains(&(base_hpa.as_usize(), 1)),
            "the final peer teardown drops the channel backing frames"
        );
        assert!(
            prepare_subscribe_channel_from_channels(
                &channels,
                publisher_vm_id,
                key,
                new_subscriber_vm_id,
            )
            .is_err(),
            "the channel entry is gone after the final peer exits"
        );
    }
}
