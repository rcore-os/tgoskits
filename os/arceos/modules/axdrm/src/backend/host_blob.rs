//! Host-visible blob mappings. The GEM owner pins each BAR slot until UNMAP
//! and resource release have been accepted, including all surviving VMAs.

use alloc::collections::BTreeMap;

use ax_gpu::rdif_gpu::{BufferHandle, GpuError, HostMemoryRegion};
use ax_memory_addr::{PhysAddr, PhysAddrRange};
use ax_task::sync::Mutex;
use drm_kapi::config::{EINVAL, ENOSPC, EOPNOTSUPP, PAGE_SIZE};

// Preserve the existing Venus path's 16 KiB minimum: host memory listeners
// can require host-page alignment larger than the guest's 4 KiB pages.
const BAR_ALIGN: u64 = 0x4000;

#[derive(Default)]
struct Slots(BTreeMap<u64, u64>);

impl Slots {
    fn reserve(&mut self, region_len: u64, size: u64, alignment: u64) -> Result<u64, i32> {
        let mut cursor = 0u64;
        for (&offset, &length) in &self.0 {
            let aligned = align_up(cursor, alignment)?;
            if aligned.checked_add(size).is_some_and(|end| end <= offset) {
                break;
            }
            cursor = offset.checked_add(length).ok_or(-EINVAL)?;
        }
        let start = align_up(cursor, alignment)?;
        if start.checked_add(size).is_none_or(|end| end > region_len) {
            return Err(-ENOSPC);
        }
        self.0.insert(start, size);
        Ok(start)
    }
}

static SLOTS: Mutex<Slots> = Mutex::new(Slots(BTreeMap::new()));

fn align_up(size: u64, alignment: u64) -> Result<u64, i32> {
    if alignment == 0 {
        return Err(-EINVAL);
    }
    size.checked_add(alignment - 1)
        .map(|size| size / alignment * alignment)
        .ok_or(-EINVAL)
}

pub(super) struct Layout {
    region: HostMemoryRegion,
    alignment: u64,
    slot_size: u64,
    pub(super) gem_size: usize,
}

impl Layout {
    pub(super) fn new(size: u64) -> Result<Self, i32> {
        let (region, device_alignment) = super::virtgpu::with_virgl(|virgl| {
            Ok((virgl.host_memory_region(), virgl.blob_alignment()))
        })?;
        let region = region.ok_or(-EOPNOTSUPP)?;
        if size == 0 || size > region.length || !region.base.is_multiple_of(PAGE_SIZE as u64) {
            return Err(-EINVAL);
        }
        if device_alignment
            .is_some_and(|align| align == 0 || !size.is_multiple_of(u64::from(align)))
        {
            return Err(-EINVAL);
        }
        let alignment = u64::from(device_alignment.unwrap_or(0)).max(BAR_ALIGN);
        if !alignment.is_multiple_of(PAGE_SIZE as u64) {
            return Err(-EINVAL);
        }
        let slot_size = align_up(size, alignment)?;
        if slot_size > region.length || region.base.checked_add(region.length).is_none() {
            return Err(-EINVAL);
        }
        let gem_size = usize::try_from(align_up(size, PAGE_SIZE as u64)?).map_err(|_| -EINVAL)?;
        Ok(Self {
            region,
            alignment,
            slot_size,
            gem_size,
        })
    }

    /// On success the caller retains the resource and the returned BAR lease.
    /// Every failure consumes the resource, including local admission failure.
    pub(super) fn map(&self, resource: BufferHandle) -> Result<HostMapping, i32> {
        let reservation = {
            let mut slots = SLOTS.lock();
            slots.reserve(self.region.length, self.slot_size, self.alignment)
        };
        let offset = match reservation {
            Ok(offset) => offset,
            Err(error) => {
                super::gem::release_resource(resource);
                return Err(error);
            }
        };
        let slot = BarSlot(offset);
        let base = match usize::try_from(self.region.base + offset) {
            Ok(base) => base,
            Err(_) => {
                super::gem::release_resource(resource);
                return Err(-EINVAL);
            }
        };
        let result = super::virtgpu::with_virgl(|virgl| {
            let result = virgl.map_blob(resource, offset);
            virgl.ctrl_notify();
            result
        });
        let info = match result {
            Ok(info) => info,
            Err(error) => {
                // Even a timed-out MAP may still reach the host. Keep the
                // slot reserved until resource release confirms quiescence.
                super::gem::release_resource(resource);
                return Err(error);
            }
        };
        Ok(HostMapping {
            physical: PhysAddrRange::from_start_size(PhysAddr::from_usize(base), self.gem_size),
            cached: info & 0xf == 1, // VIRTIO_GPU_MAP_CACHE_CACHED
            _slot: slot,
        })
    }
}

struct BarSlot(u64);

impl Drop for BarSlot {
    fn drop(&mut self) {
        SLOTS.lock().0.remove(&self.0);
    }
}

pub(super) struct HostMapping {
    pub(super) physical: PhysAddrRange,
    pub(super) cached: bool,
    _slot: BarSlot,
}

/// Do not let a failed enqueue release a still-mapped slot. Retry until UNMAP
/// is queued or RDIF confirms that device loss has stopped all device access.
pub(super) fn unmap_before_release(resource: BufferHandle) {
    loop {
        let result = ax_gpu::with_gpu_for_cleanup(|gpu| {
            let virgl = gpu.virgl().ok_or(GpuError::Unsupported)?;
            let result = virgl.unmap_blob(resource);
            virgl.ctrl_notify();
            result
        })
        .and_then(core::convert::identity);
        // Publish cleanup progress without making completion queries self-wake.
        super::fence::on_gpu_completion();
        match result {
            Ok(()) | Err(GpuError::DeviceLost) => return,
            Err(error) => {
                log::debug!("retrying host-visible blob unmap after {error}");
                if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
                    drm_kapi::task_runtime::yield_now();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_slots_recycle_holes_without_aliasing_live_mappings() {
        let mut slots = Slots::default();
        let first = slots.reserve(0x10000, 0x4000, BAR_ALIGN).unwrap();
        let second = slots.reserve(0x10000, 0x8000, BAR_ALIGN).unwrap();
        assert_eq!((first, second), (0, 0x4000));
        assert_eq!(slots.reserve(0x10000, 0x8000, BAR_ALIGN), Err(-ENOSPC));
        slots.0.remove(&first);
        assert_eq!(slots.reserve(0x10000, 0x4000, BAR_ALIGN), Ok(first));
        assert_eq!(slots.reserve(0x10000, 0x4000, BAR_ALIGN), Ok(0xc000));
        assert_eq!(slots.reserve(0x10000, 0x4000, BAR_ALIGN), Err(-ENOSPC));
        assert_eq!(align_up(u64::MAX, BAR_ALIGN), Err(-EINVAL));
    }
}
