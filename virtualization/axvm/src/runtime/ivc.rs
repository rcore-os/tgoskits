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
    sync::Mutex,
};

use ax_memory_addr::{PAGE_SIZE_4K, align_up_4k};

use crate::{
    AxVmError, AxVmResult, GuestPhysAddr, HostPhysAddr, ax_err_type, host::PagingHandler,
    sync::MutexExt,
};

/// A global btree map to store IVC channels,
/// indexed by (publisher_vm_id, channel_key).
type HostIVCChannel = IVCChannel<crate::HostPagingHandler>;

static IVC_CHANNELS: Mutex<BTreeMap<(usize, usize), HostIVCChannel>> = Mutex::new(BTreeMap::new());

/// Maximum size of one IVC channel's shared region.
///
/// Requests larger than this are truncated; the hypercall ABI always writes
/// the actual granted size back to the guest, so guests must check it.
pub const MAX_IVC_CHANNEL_SIZE: usize = 0x10_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcNotifyRoute {
    pub source_vm_id: usize,
    pub target_vm_id: usize,
    pub publisher_vm_id: usize,
    pub key: usize,
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
pub fn unpublish_channel(publisher_vm_id: usize, key: usize) -> AxVmResult<(GuestPhysAddr, usize)> {
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
    let base_gpa = channel.base_gpa_in_publisher().ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!(
                "IVC channel for publisher VM {} with key {} has no base GPA, it may have been \
                 marked as unpublished",
                publisher_vm_id, key
            )
        )
    })?;
    let size = channel.size();

    if channel.has_subscribers() {
        channel.mark_unpublished();
    } else {
        channels.remove(&channel_key);
    }

    Ok((base_gpa, size))
}

pub fn prepare_subscribe_channel(
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
) -> AxVmResult<usize> {
    let channels = IVC_CHANNELS.lock_unpoisoned();
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

/// Subcribe to a channel of a publisher VM with the given key,
/// return the shared region base address and size.
pub fn subscribe_to_channel_of_publisher(
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
    subscriber_gpa: GuestPhysAddr,
) -> AxVmResult<(HostPhysAddr, usize)> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
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

/// Unsubscribe from a channel of a publisher VM with the given key,
/// if the channel has been unpublished (i.e., the base GPA is None) and has no subscribers,
/// it will remove the channel from the global map.
pub fn unsubscribe_from_channel_of_publisher(
    publisher_vm_id: usize,
    key: usize,
    subscriber_vm_id: usize,
) -> AxVmResult<(GuestPhysAddr, usize)> {
    let mut channels = IVC_CHANNELS.lock_unpoisoned();
    let (base_gpa, size) = if let Some(channel) = channels.get_mut(&(publisher_vm_id, key)) {
        // Remove the subscriber VM ID from the channel.
        if let Some(subscriber_gpa) = channel.remove_subscriber(subscriber_vm_id) {
            Ok((subscriber_gpa, channel.size()))
        } else {
            Err(ax_err_type!(
                NotFound,
                format!(
                    "VM[{}] tries to unsubscribe non-existed channel publisher VM[{}] Key {:#x}",
                    subscriber_vm_id, publisher_vm_id, key
                )
            ))
        }
    } else {
        Err(ax_err_type!(
            NotFound,
            format!("IVC channel for publisher VM {} not found", publisher_vm_id)
        ))
    }?;

    // If the channel has no subscribers and has been unpublished (base GPA is None),
    // remove it from the global map.
    if channels
        .get(&(publisher_vm_id, key))
        .is_some_and(|c| !c.has_subscribers() && c.is_unpublished())
    {
        channels.remove(&(publisher_vm_id, key));
    }

    Ok((base_gpa, size))
}

pub fn prepare_notify_channel(
    publisher_vm_id: usize,
    key: usize,
    source_vm_id: usize,
    target_vm_id: usize,
) -> AxVmResult<IvcNotifyRoute> {
    let channels = IVC_CHANNELS.lock_unpoisoned();
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
    /// The base address of the shared memory region in guest physical address of the publisher VM.
    /// `None` if the channel has been unpublished (but still has subscribers).
    base_gpa: Option<GuestPhysAddr>,
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

        let mut channel = IVCChannel {
            publisher_vm_id,
            key,
            subscriber: None,
            shared_region_base,
            shared_region_size,
            base_gpa: Some(base_gpa),
            _phantom: std::marker::PhantomData,
        };

        {
            let header = channel.header_mut();
            header.publisher_id = publisher_vm_id as u64;
            header.key = key as u64;
        }

        debug!("Allocated IVCChannel: {channel:?}");

        Ok(channel)
    }

    pub fn base_hpa(&self) -> HostPhysAddr {
        self.shared_region_base
    }

    pub fn base_gpa_in_publisher(&self) -> Option<GuestPhysAddr> {
        self.base_gpa
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
        });
        Ok(())
    }

    pub fn remove_subscriber(&mut self, subscriber_vm_id: usize) -> Option<GuestPhysAddr> {
        let binding = self
            .subscriber
            .filter(|binding| binding.vm_id == subscriber_vm_id)?;
        self.subscriber = None;
        Some(binding.base_gpa)
    }

    pub fn has_subscribers(&self) -> bool {
        self.subscriber.is_some()
    }

    pub fn has_subscriber(&self, subscriber_vm_id: usize) -> bool {
        self.subscriber
            .is_some_and(|binding| binding.vm_id == subscriber_vm_id)
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
    }

    pub fn is_unpublished(&self) -> bool {
        self.base_gpa.is_none()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::{sync::Mutex, vec::Vec};

    use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};

    use super::*;

    const TEST_ARENA_SIZE: usize = 8 * 1024 * 1024;

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
        assert_eq!(channel.remove_subscriber(2), Some(first_gpa));
        channel
            .add_subscriber(3, GuestPhysAddr::from_usize(0x7200_0000))
            .unwrap();

        assert!(!channel.has_subscriber(2));
        assert!(channel.has_subscriber(3));
    }
}
