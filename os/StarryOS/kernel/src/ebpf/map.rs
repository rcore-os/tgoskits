//! BPF map file-like wrapper and mmap glue. Ported from
//! `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/bpf/map.rs`); imports adapted
//! to tgoskits' `ax_hal` / `ax_kspin` / `ax_errno` / `ax_alloc` package
//! names per `crate-fork-audit.md §6`.

use alloc::{borrow::Cow, sync::Arc, vec::Vec};
use core::ops::{Deref, DerefMut};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use axpoll::{PollSet, Pollable};
use kbpf_basic::{
    PollWaker,
    map::{BpfMapMeta, UnifiedMap, bpf_map_create},
};

use crate::{
    ebpf::transform::{EbpfKernelAuxiliary, PerCpuImpl},
    file::{FileLike, Kstat},
    kprobe::KernelRawMutex,
    pseudofs::DeviceMmap,
};

/// File-like handle for a BPF map. Holds the `UnifiedMap` (the kbpf-basic
/// abstraction over array / hash / lru / queue / perf-array maps) and a
/// `PollSet` so `poll(2)`-based maps (e.g. ringbuf) can wake waiters.
pub struct BpfMap {
    unified_map: Arc<UnifiedMap<KernelRawMutex>>,
    poll_ready: Arc<PollSetWrapper>,
}

impl core::fmt::Debug for BpfMap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BpfMap").finish()
    }
}

impl BpfMap {
    /// Wrap a freshly-created `UnifiedMap` in the kernel file-like layer.
    pub fn new(unified_map: UnifiedMap<KernelRawMutex>, poll_ready: Arc<PollSetWrapper>) -> Self {
        BpfMap {
            unified_map: Arc::new(unified_map),
            poll_ready,
        }
    }

    /// Lock and access the underlying `UnifiedMap`.
    pub fn unified_map(&self) -> &UnifiedMap<KernelRawMutex> {
        self.unified_map.as_ref()
    }
}

impl Pollable for BpfMap {
    fn poll(&self) -> axpoll::IoEvents {
        let map = self.unified_map();
        let mut events = axpoll::IoEvents::empty();
        if map.map().readable() {
            events |= axpoll::IoEvents::IN;
        }
        if map.map().writable() {
            events |= axpoll::IoEvents::OUT;
        }
        events
    }

    fn register(&self, context: &mut core::task::Context<'_>, _events: axpoll::IoEvents) {
        self.poll_ready.register(context.waker());
    }
}

impl FileLike for BpfMap {
    fn read(&self, _dst: &mut crate::file::IoDst) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn write(&self, _src: &mut crate::file::IoSrc) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[bpf_map]".into()
    }

    fn device_mmap(&self, offset: u64, length: u64) -> AxResult<crate::pseudofs::DeviceMmap> {
        // for ringbuf maps, userland calls mmap on the map fd to get a pointer to the ringbuf;
        // the kernel must support this. For other map types, mmap is not meaningful and Linux rejects it with EINVAL.
        if !offset.is_multiple_of(PAGE_SIZE_4K as u64)
            || !length.is_multiple_of(PAGE_SIZE_4K as u64)
        {
            return Err(AxError::InvalidInput);
        }

        let unified_map = self.unified_map();
        let map = unified_map.map();
        let phy_addrs = map
            .map_mmap(offset as usize, length as usize)
            .map_err(|_| AxError::InvalidInput)?
            .iter()
            .map(|&phys_addr| PhysAddr::from_usize(phys_addr))
            .collect::<Vec<_>>();

        let retain: Arc<dyn core::any::Any + Send + Sync> = self.unified_map.clone();
        Ok(DeviceMmap::PhysicalPages(phy_addrs, Some(retain)))
    }
}

/// A `PollSet` wrapper that satisfies `kbpf_basic::PollWaker`, allowing
/// the map implementations inside kbpf-basic to wake registered tasks
/// (ringbuf reservation, queue push, etc.).
pub struct PollSetWrapper(PollSet);

impl PollSetWrapper {
    /// Create a fresh, empty poll set.
    pub fn new() -> Self {
        Self(PollSet::new())
    }
}

impl Default for PollSetWrapper {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for PollSetWrapper {
    type Target = PollSet;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PollSetWrapper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl PollWaker for PollSetWrapper {
    fn wake_up(&self) {
        self.0.wake();
    }
}

/// Create a new BPF map (factory) from a metadata descriptor.
pub fn create_map(meta: BpfMapMeta) -> kbpf_basic::BpfResult<BpfMap> {
    let waker = Arc::new(PollSetWrapper::new());
    let waker_dyn: Arc<dyn PollWaker> = waker.clone();
    let map = bpf_map_create::<EbpfKernelAuxiliary, PerCpuImpl>(meta, Some(waker_dyn))?;
    Ok(BpfMap::new(map, waker))
}
