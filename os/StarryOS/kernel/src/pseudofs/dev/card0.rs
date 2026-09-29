//! `/dev/dri/card0` — DRM character device for the registered GPU.
//!
//! Single-CRTC, single-connector, single-plane KMS over the GPU/display
//! capabilities. Covers legacy libdrm
//! (`CREATE_DUMB → ADDFB2 → SETCRTC → PAGE_FLIP`) and the atomic-KMS
//! path (`MODE_ATOMIC` + blob properties) used by modern compositors.
//!
//! Fixed IDs:
//!   crtc=0x10, encoder=0x20, connector=0x30, plane=0x40
//!
//! Simplifications vs. a real DRM driver:
//!   - `CREATE_DUMB` allocates a CPU-mappable GPU backing. `MAP_DUMB`
//!     returns a unique offset key, which mmap resolves to its retained
//!     physical pages. KMS commits the associated device buffer through
//!     the display capability. PRIME export/import retains backing
//!     independently of per-file handles.
//!   - Property validation is permissive: value ranges aren't rigorously
//!     enforced (tests drive sensible values). Atomic rejects only
//!     unknown `(obj, prop)` pairs and obviously-bad object/blob refs.
//!   - `WAIT_VBLANK` and the `CRTC_GET_SEQUENCE` / `CRTC_QUEUE_SEQUENCE`
//!     pair run on a synthesized 60 Hz vblank clock
//!     ([`super::vblank`]): the sequence is derived from elapsed
//!     monotonic time while the CRTC is active, with per-open pending
//!     events delivered when their synthesized edge is reached.
//!   - One CRTC, connector, and plane are exposed from the first output.

use alloc::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    format,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    num::NonZeroUsize,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::Duration,
};

use ax_gpu::MappableBacking;
use ax_gpu::rdif_display::{
    DisplayController, DisplayError, DisplayState, Framebuffer as ScanoutFramebuffer, GpuDisplay,
    Mode as DisplayMode, OutputId, OutputInfo, OutputKind, Rect, ScanoutBuffer,
};
use ax_gpu::rdif_gpu::{
    Backing, BufferDescriptor, BufferHandle, Completion, CompletionStatus, ContextHandle, DmaAddr,
    DmaDomainId, DmaSegment, GpuError, PixelFormat,
};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddrRange};
use ax_runtime::hal::time::monotonic_time_nanos;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;
use bytemuck::bytes_of;
use event_listener::{Event, listener};
use linux_raw_sys::general::O_CLOEXEC;

mod virtgpu;
mod virtgpu_uapi;

use self::virtgpu_uapi::*;

use super::drm::{
    DRM_CAP_ADDFB2_MODIFIERS,
    DRM_CAP_CRTC_IN_VBLANK_EVENT,
    DRM_CAP_DUMB_BUFFER,
    DRM_CAP_PRIME,
    DRM_CAP_TIMESTAMP_MONOTONIC,
    DRM_CRTC_SEQUENCE_NEXT_ON_MISS,
    DRM_CRTC_SEQUENCE_RELATIVE,
    DRM_EVENT_CRTC_SEQUENCE,
    DRM_EVENT_FLIP_COMPLETE,
    DRM_EVENT_VBLANK,
    DRM_FORMAT_ARGB8888,
    DRM_FORMAT_BGR888,
    DRM_FORMAT_MOD_INVALID,
    DRM_FORMAT_MOD_LINEAR,
    DRM_FORMAT_RGB565,
    DRM_FORMAT_RGB888,
    DRM_FORMAT_XBGR8888,
    DRM_FORMAT_XRGB8888,
    DRM_IOCTL_AUTH_MAGIC,
    DRM_IOCTL_CRTC_GET_SEQUENCE,
    DRM_IOCTL_CRTC_QUEUE_SEQUENCE,
    DRM_IOCTL_DROP_MASTER,
    DRM_IOCTL_GEM_CLOSE,
    DRM_IOCTL_GET_CAP,
    DRM_IOCTL_GET_MAGIC,
    DRM_IOCTL_GET_UNIQUE,
    DRM_IOCTL_MODE_ADDFB2,
    DRM_IOCTL_MODE_ATOMIC,
    DRM_IOCTL_MODE_CREATE_DUMB,
    DRM_IOCTL_MODE_CREATEPROPBLOB,
    DRM_IOCTL_MODE_DESTROY_DUMB,
    DRM_IOCTL_MODE_DESTROYPROPBLOB,
    DRM_IOCTL_MODE_DIRTYFB,
    DRM_IOCTL_MODE_GETCONNECTOR,
    DRM_IOCTL_MODE_GETCRTC,
    DRM_IOCTL_MODE_GETENCODER,
    DRM_IOCTL_MODE_GETPLANE,
    DRM_IOCTL_MODE_GETPLANERESOURCES,
    DRM_IOCTL_MODE_GETPROPBLOB,
    DRM_IOCTL_MODE_GETPROPERTY,
    DRM_IOCTL_MODE_GETRESOURCES,
    DRM_IOCTL_MODE_MAP_DUMB,
    DRM_IOCTL_MODE_OBJ_GETPROPERTIES,
    DRM_IOCTL_MODE_PAGE_FLIP,
    DRM_IOCTL_MODE_RMFB,
    DRM_IOCTL_MODE_SETCRTC,
    DRM_IOCTL_PRIME_FD_TO_HANDLE,
    DRM_IOCTL_PRIME_HANDLE_TO_FD,
    DRM_IOCTL_SET_CLIENT_CAP,
    DRM_IOCTL_SET_MASTER,
    DRM_IOCTL_SET_VERSION,
    DRM_IOCTL_VERSION,
    DRM_IOCTL_WAIT_VBLANK,
    DRM_MODE_ATOMIC_ALLOW_MODESET,
    DRM_MODE_ATOMIC_NONBLOCK,
    DRM_MODE_ATOMIC_TEST_ONLY,
    DRM_MODE_CONNECTED,
    DRM_MODE_DISCONNECTED,
    DRM_MODE_CONNECTOR_UNKNOWN,
    DRM_MODE_CONNECTOR_VGA,
    DRM_MODE_CONNECTOR_DISPLAYPORT,
    DRM_MODE_CONNECTOR_HDMIA,
    DRM_MODE_CONNECTOR_EDP,
    DRM_MODE_CONNECTOR_VIRTUAL,
    DRM_MODE_ENCODER_NONE,
    DRM_MODE_ENCODER_DAC,
    DRM_MODE_ENCODER_TMDS,
    DRM_MODE_ENCODER_LVDS,
    DRM_MODE_ENCODER_VIRTUAL,
    DRM_MODE_FB_MODIFIERS,
    DRM_MODE_OBJECT_CONNECTOR,
    DRM_MODE_OBJECT_CRTC,
    DRM_MODE_OBJECT_FB,
    DRM_MODE_OBJECT_PLANE,
    DRM_MODE_PAGE_FLIP_EVENT,
    DRM_MODE_PROP_ATOMIC,
    DRM_MODE_PROP_BLOB,
    DRM_MODE_PROP_ENUM,
    DRM_MODE_PROP_IMMUTABLE,
    DRM_MODE_PROP_OBJECT,
    DRM_MODE_PROP_RANGE,
    DRM_MODE_PROP_SIGNED_RANGE,
    DRM_PLANE_TYPE_PRIMARY,
    DRM_PRIME_CAP_EXPORT,
    DRM_PRIME_CAP_IMPORT,
    DRM_PROP_NAME_LEN,
    DRM_VBLANK_EVENT,
    DRM_VBLANK_FLAGS_MASK,
    DRM_VBLANK_HIGH_CRTC_MASK,
    DRM_VBLANK_NEXTONMISS,
    DRM_VBLANK_RELATIVE,
    DRM_VBLANK_SECONDARY,
    DRM_VBLANK_SIGNAL,
    DRM_VBLANK_TYPES_MASK,
    DrmAuth,
    DrmEvent,
    DrmEventCrtcSequence,
    DrmEventVblank,
    DrmGemClose,
    DrmGetCap,
    DrmModeAtomic,
    DrmModeCardRes,
    DrmModeCreateBlob,
    DrmModeCreateDumb,
    DrmModeCrtc,
    DrmModeCrtcGetSequence,
    DrmModeCrtcPageFlip,
    DrmModeCrtcQueueSequence,
    DrmModeDestroyBlob,
    DrmModeDestroyDumb,
    DrmModeDirtyFB,
    DrmModeFbCmd2,
    DrmModeGetBlob,
    DrmModeGetConnector,
    DrmModeGetEncoder,
    DrmModeGetPlane,
    DrmModeGetPlaneRes,
    DrmModeGetProperty,
    DrmModeMapDumb,
    DrmModeModeInfo,
    DrmModeObjGetProperties,
    DrmModePropertyEnum,
    DrmPrimeHandle,
    DrmSetClientCap,
    DrmSetVersion,
    DrmUnique,
    DrmVersion,
    DrmWaitVblank,
};
use super::vblank::{
    PendingVblankEvent, QueuedVblankEvent, VBLANK_PERIOD_NS, VblankClock, vblank_passed,
    widen_32_to_64,
};
use super::sync_file::{SyncFile, kick_refresher};
use crate::{
    StarryError, StarryResult,
    file::{
        File as KernelFile, FileLike, IoDst, IoSrc, Kstat, add_file_like, current_fd_table,
        dma_buf_seek, prepare_file_like, release_locks_on_close,
    },
    mm::{VmMutPtr, VmPtr, vm_load, vm_write_slice},
    pseudofs::{DeviceMmap, DeviceOps},
    sync::Mutex,
    task::{
        UserTaskRef, current_user_task,
        future::{block_on, block_on_user, poll_io, timeout_at},
    },
};

const DRM_DRIVER_DATE: &str = "20260921";

/// Fixed object IDs advertised by GETRESOURCES / GETCONNECTOR / GETENCODER.
const CRTC_ID: u32 = 0x10;
const ENCODER_ID: u32 = 0x20;
const CONNECTOR_ID: u32 = 0x30;
const PLANE_ID: u32 = 0x40;

/// First context id.  Linux starts at 1 (virglrenderer rejects id 0).
/// Allocated per-fd via `next_ctx_id` to match
/// `atomic_inc_return(&vgdev->ctx_id_cursor)` in the Linux kernel.
const FIRST_VIRGL_CTX_ID: u32 = 1;
/// First resource id owned by DRM userspace. The independent `virtio-gpu`
/// core/boot framebuffer path keeps its own id (`0xbabe`) for the boot
/// framebuffer and hands no ids to userspace, so this namespace starts low and
/// advances monotonically.
const FIRST_GPU_RESOURCE_ID: u32 = 1;

/// Which protocol a freshly created virgl context uses.
///
/// Linux's `struct virtio_gpu_fpriv` carries `context_init`, which stays 0
/// until userspace opts into the `DRM_IOCTL_VIRTGPU_CONTEXT_INIT` protocol.
/// Both kinds still send a `CTX_CREATE`; only the extra `context_init` field
/// differs, so the two are separate call shapes instead of one ambiguous
/// integer argument.
enum CreateKind {
    /// Linux `virtio_gpu_create_context()` default: `context_init = 0`,
    /// `num_rings = 1`. Selected by the legacy lazy paths whenever userspace
    /// never ran the context-init ioctl (Mesa only does so when
    /// `GETPARAM(CONTEXT_INIT)` is non-zero).
    Legacy,
    /// `DRM_IOCTL_VIRTGPU_CONTEXT_INIT`: `context_init = capset_id`, plus the
    /// user's ring count.
    Explicit { capset_id: u32, num_rings: u32 },
}

/// First dumb-buffer handle we hand out.
const FIRST_DUMB_HANDLE: u32 = 1;
/// First framebuffer id we hand out from `ADDFB2`.
const FIRST_FB_ID: u32 = 1;

/// Upper bound on `GET_CAPSET_INFO` index probing.
///
/// Linux learns the capset count from a device register and iterates exactly
/// `num_capsets` entries. The interface here only exposes per-index queries,
/// so enumeration stops at the first failing index; this bound caps how many
/// successful replies a misbehaving host can draw out.
const MAX_CAPSET_ENUM: u32 = 64;

/// Per-buffer size cap. We don't pre-reserve a heap — each
/// `CREATE_DUMB` sizes its own allocation — but we still cap individual
/// requests so a bogus width/height/bpp can't OOM the kernel. 8 MiB
/// covers 1920x1080 XRGB with headroom.
const DUMB_BUFFER_MAX_SIZE: usize = 8 * 1024 * 1024;
/// Each buffer's `MAP_DUMB` offset is a monotonic stride in this unit —
/// a synthetic, unique key into the per-card offset->buffer lookup. Must
/// be at least `DUMB_BUFFER_MAX_SIZE` so adjacent buffers don't overlap
/// when userspace mmap's `(fd, length=size_of_buffer, offset=this_key)`.
const DUMB_BUFFER_OFFSET_STRIDE: u64 = DUMB_BUFFER_MAX_SIZE as u64;

// ---- property IDs ----
// Layout: 0x1xx = plane, 0x2xx = CRTC, 0x3xx = connector.
const PROP_PLANE_TYPE: u32 = 0x100;
const PROP_PLANE_FB_ID: u32 = 0x101;
const PROP_PLANE_CRTC_ID: u32 = 0x102;
const PROP_PLANE_SRC_X: u32 = 0x103;
const PROP_PLANE_SRC_Y: u32 = 0x104;
const PROP_PLANE_SRC_W: u32 = 0x105;
const PROP_PLANE_SRC_H: u32 = 0x106;
const PROP_PLANE_CRTC_X: u32 = 0x107;
const PROP_PLANE_CRTC_Y: u32 = 0x108;
const PROP_PLANE_CRTC_W: u32 = 0x109;
const PROP_PLANE_CRTC_H: u32 = 0x10A;
/// `IN_FORMATS` — immutable blob property advertising the (format,
/// modifier) tuples this plane accepts.
const PROP_PLANE_IN_FORMATS: u32 = 0x10B;
/// Standard explicit-fence property. Only the no-fence sentinel is currently
/// supported: Starry has no sync-file producer or fence-wait implementation.
const PROP_PLANE_IN_FENCE_FD: u32 = 0x10C;

const PROP_CRTC_ACTIVE: u32 = 0x200;
const PROP_CRTC_MODE_ID: u32 = 0x201;

const PROP_CONN_CRTC_ID: u32 = 0x300;

const PLANE_PROPS: &[u32] = &[
    PROP_PLANE_TYPE,
    PROP_PLANE_FB_ID,
    PROP_PLANE_CRTC_ID,
    PROP_PLANE_SRC_X,
    PROP_PLANE_SRC_Y,
    PROP_PLANE_SRC_W,
    PROP_PLANE_SRC_H,
    PROP_PLANE_CRTC_X,
    PROP_PLANE_CRTC_Y,
    PROP_PLANE_CRTC_W,
    PROP_PLANE_CRTC_H,
    PROP_PLANE_IN_FORMATS,
    PROP_PLANE_IN_FENCE_FD,
];
const CRTC_PROPS: &[u32] = &[PROP_CRTC_ACTIVE, PROP_CRTC_MODE_ID];
const CONN_PROPS: &[u32] = &[PROP_CONN_CRTC_ID];

/// Upper bound on the pending-event queue. Matches Linux's
/// `file->event_space` of 4 KB ≈ 128 `drm_event_vblank`s.
const MAX_EVENTS: usize = 128;

/// Storage slot for a queued DRM event. `DRM_EVENT_VBLANK` and
/// `DRM_EVENT_CRTC_SEQUENCE` are both exactly 32 bytes on 64-bit; events
/// are serialized on enqueue so `read` can copy payloads of either type
/// from one queue without Rust enum layout leaking into the ABI.
const EVENT_SLOT_BYTES: usize = core::mem::size_of::<DrmEventVblank>();

/// Fixed-size serialized event slot for an open file's event queue.
type EventSlot = [u8; EVENT_SLOT_BYTES];

const _: () = {
    assert!(EVENT_SLOT_BYTES == core::mem::size_of::<DrmEventCrtcSequence>());
};

/// First blob id we hand out from `CREATEPROPBLOB`.
const FIRST_BLOB_ID: u32 = 0x1000;

/// Upper bound on `CREATEPROPBLOB` payload size.
const MAX_BLOB_BYTES: usize = 64 * 1024;
/// Upper bound for one userspace-supplied virgl command stream.
const MAX_VIRGL_COMMAND_BYTES: usize = 16 * 1024 * 1024;

/// One GEM handle backed by a CPU-mappable GPU allocation. `offset` is a
/// synthetic `MAP_DUMB` key, while each VMA holds its own mapping reference.
/// Width, height, bpp, and pitch are metadata only. A PRIME import sets them
/// to zero because the ioctl carries only the handle, flags, and fd; ADDFB2
/// supplies the image layout when the imported handle becomes a framebuffer.
struct DumbBuffer {
    /// Open-file description that owns this GEM handle.
    owner: u64,
    width: u32,
    height: u32,
    bpp: u32,
    pitch: u32,
    size: u64,
    /// Unique mmap-offset key for this buffer.
    offset: u64,
    /// CPU-mappable backing. Refcounted so user mappings keep it alive
    /// across `DESTROY_DUMB`.
    mapping: Arc<GpuMapping>,
    /// Whether this GEM object has guest memory userspace may mmap.
    mappable: bool,
    /// Host-side resource associated with this GEM object, when any.
    resource: Option<Arc<GpuResource>>,
}

/// Per-framebuffer state retained until `RMFB`. Holds the dumb
/// buffer's backing directly so a `DESTROY_DUMB` on the source
/// handle does not invalidate the fb — Linux's GEM contract says a
/// framebuffer keeps the buffer alive for as long as the fb_id is
/// live.
#[derive(Clone)]
struct Framebuffer {
    /// Open-file description that created this framebuffer.
    owner: u64,
    /// Row stride (pitch) in bytes — from ADDFB2.pitches[0].
    stride: u32,
    /// Framebuffer width in pixels — from ADDFB2.width.
    width: u32,
    /// Framebuffer height in pixels — from ADDFB2.height.
    height: u32,
    format: PixelFormat,
    /// Backing storage retained by this framebuffer.
    kind: FbBacking,
}

/// Backing storage for a DRM framebuffer.
#[derive(Clone)]
enum FbBacking {
    /// CPU-mappable backing used directly by the display or awaiting image
    /// layout from `ADDFB2`.
    Dumb { mapping: Arc<GpuMapping> },
    /// Device buffer retained across GEM handle close and scanout.
    Gpu3d { resource: Arc<GpuResource> },
}

/// StarryOS kernel-side dma-buf GEM object for DRM card0.
///
/// Wraps the physical pages backing a dumb buffer so the exported fd
/// (returned by [`Self::handle_prime_handle_to_fd`]) can be mmap'd,
/// read, or passed via SCM_RIGHTS for cross-process buffer sharing.
/// Follows the same pattern as card1.rs's `ExportedGemBuffer`.
struct DmaBufGem {
    /// DMA allocation shared with the source GEM handle and user mappings.
    mapping: Arc<GpuMapping>,
    /// The same-device GPU object, when the exporter already created one.
    /// Sharing this Arc preserves its device handle across PRIME imports.
    resource: Option<Arc<GpuResource>>,
    /// Total size in bytes.
    size: u64,
}

/// A user mapping pins both its CPU pages and the GPU object that owns them.
/// The VMA keeps this lease after the creating GEM handle or PRIME fd closes.
struct GpuMappingLease {
    _mapping: Arc<GpuMapping>,
    _resource: Option<Arc<GpuResource>>,
}

/// GPU-visible pages retained by a GEM handle, PRIME fd, framebuffer or VMA.
enum GpuMapping {
    Owned(Arc<MappableBacking>),
    Heap(Arc<HeapBacking>),
}

impl GpuMapping {
    fn backing(&self) -> Arc<dyn Backing> {
        match self {
            Self::Owned(mapping) => mapping.backing(),
            Self::Heap(backing) => backing.clone(),
        }
    }

    fn physical(&self) -> PhysAddrRange {
        match self {
            Self::Owned(mapping) => mapping.physical(),
            Self::Heap(backing) => backing.file.phys_range(),
        }
    }
}

/// Imported coherent dma-heap allocation in the direct DMA domain.
struct HeapBacking {
    file: Arc<crate::file::dmabuf::DmaBufFile>,
    segments: [DmaSegment; 1],
}

impl HeapBacking {
    fn new(file: Arc<crate::file::dmabuf::DmaBufFile>) -> Self {
        let segments = [DmaSegment::new(
            DmaAddr::from(file.phys_base() as u64),
            NonZeroUsize::new(file.size()).expect("dma-heap allocation is nonempty"),
        )];
        Self { file, segments }
    }
}

// SAFETY: DmaBufFile retains a stable contiguous CoherentArray in the direct
// DMA domain. Its allocation outlives every cloned HeapBacking, and coherent
// memory needs no explicit cache ownership transfer.
unsafe impl Backing for HeapBacking {
    fn len(&self) -> usize {
        self.file.size()
    }

    fn domain_id(&self) -> DmaDomainId {
        DmaDomainId::Direct
    }

    fn segments(&self) -> &[DmaSegment] {
        &self.segments
    }

    fn sync_for_device(&self, range: Range<usize>) -> Result<(), GpuError> {
        if range.start > range.end || range.end > self.len() {
            return Err(GpuError::InvalidArgument);
        }
        Ok(())
    }

    fn sync_for_cpu(&self, range: Range<usize>) -> Result<(), GpuError> {
        self.sync_for_device(range)
    }
}

impl FileLike for DmaBufGem {
    fn validate_write_access(&self) -> StarryResult {
        Err(StarryError::InvalidInput)
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:dmabuf".into()
    }

    fn seek(&self, pos: ax_io::SeekFrom) -> StarryResult<u64> {
        dma_buf_seek(self.size, pos)
    }

    fn device_mmap(&self, offset: u64, length: u64) -> StarryResult<DeviceMmap> {
        // Validate that the requested sub-range fits within the buffer.
        // `checked_add` guards against a wrapping length that would
        // bypass the > self.size check.
        let end = offset
            .checked_add(length)
            .ok_or(StarryError::InvalidInput)?;
        if end > self.size {
            return Err(StarryError::InvalidInput);
        }
        // Return the *full* backing range.  The generic mmap layer
        // (mmap.rs, Physical arm) adds `offset` to `range.start` and
        // clamps `length` to `range.size()`, producing the correct
        // sub-mapping of [base+offset, base+offset+length).  Returning
        // the full range (rather than a length-clamped subset) avoids
        // the double-accounting bug where the generic layer would
        // shrink or invalidate the range after shifting it.
        Ok(DeviceMmap::Physical(
            self.mapping.physical(),
            Some(Arc::new(GpuMappingLease {
                _mapping: self.mapping.clone(),
                _resource: self.resource.clone(),
            })),
        ))
    }
}

impl Pollable for DmaBufGem {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

/// A mode's identity and backing travel with the candidate/committed state.
#[derive(Debug, Clone)]
struct ModeBlob {
    id: u32,
    info: DrmModeModeInfo,
    bytes: Arc<Vec<u8>>,
}

/// The single committed state shared by legacy and atomic KMS. Cloned
/// candidates keep mode references alive and publish only after validation.
#[derive(Debug, Default, Clone)]
struct ModesetState {
    crtc_active: u64,
    mode: Option<ModeBlob>,
    conn_crtc_id: u32,
    plane_fb_id: u32,
    plane_crtc_id: u32,
    plane_src_x: u64,
    plane_src_y: u64,
    plane_src_w: u64,
    plane_src_h: u64,
    plane_crtc_x: i64,
    plane_crtc_y: i64,
    plane_crtc_w: u64,
    plane_crtc_h: u64,
}

/// Metadata for a 3D GPU resource created via RESOURCE_CREATE or
/// RESOURCE_CREATE_BLOB. Tracks the association between the virtio-gpu
/// resource ID (used in virgl commands) and the GEM handle (used in
/// DRM ioctls).
#[allow(dead_code)]
struct GpuResource {
    /// Open-file description that created the original GEM handle.
    owner: u64,
    /// Virtio-gpu resource id.
    res_handle: u32,
    /// Device-scoped resource handle; only this enters the portable GPU API.
    device_handle: BufferHandle,
    /// The GEM handle associated with this resource (from CREATE_DUMB or
    /// allocated by RESOURCE_CREATE).
    bo_handle: u32,
    /// Resource width in pixels.
    width: u32,
    /// Resource height in pixels.
    height: u32,
    /// Row stride in bytes.
    stride: u32,
    /// Format used when this resource was created as a dumb image.
    format: Option<PixelFormat>,
    /// Resource size in bytes.
    size: u64,
    /// blob_mem from RESOURCE_CREATE_BLOB (`VIRTGPU_BLOB_MEM_*`); 0 for
    /// non-blob (classic 3D) resources.
    blob_mem: u32,
    /// blob_flags from RESOURCE_CREATE_BLOB (`VIRTGPU_BLOB_FLAG_*`); 0 for
    /// non-blob resources.
    blob_flags: u32,
    /// True when this resource is a guest-backed 2D resource created by
    /// CREATE_DUMB. present_fb must `TRANSFER_TO_HOST_2D` from the guest
    /// backing before `RESOURCE_FLUSH`; 3D virgl/blob resources are
    /// host-rendered and skip the transfer.
    is_dumb_2d: bool,
    /// Last submit fence that referenced this object; 0 means no outstanding
    /// fence (the object was never submitted or the driver completed
    /// synchronously). `VIRTGPU_WAIT` waits or probes exactly this fence.
    last_fence: AtomicU64,
}

impl Drop for GpuResource {
    fn drop(&mut self) {
        if let Err(error) = ax_gpu::with_gpu_for_cleanup(|device| device.release_buffer(self.device_handle))
            .and_then(core::convert::identity)
        {
            warn!("failed to release GPU buffer {:?}: {error}", self.device_handle);
        }
    }
}

/// Kernel-side dma-buf for a *host* 3D resource (blob or classic virgl
/// resource) exported via PRIME. Unlike [`DmaBufGem`], which wraps guest
/// RAM, the backing lives on the host GPU: a same-device import resolves
/// back to the same host resource through [`Card0::blob_aliases`], so the
/// importer (weston/Mesa) reuses the host texture zero-copy — exactly what
/// Linux does by exporting the GEM object itself (`virtgpu_prime.c`).
///
/// Each open dma-buf fd holds one reference on the host resource (Linux:
/// the `dma_buf` file pins the GEM object via `drm_gem_prime_export`).
/// [`Drop`] releases that reference when the last fd closes, so the host
/// resource is not freed while a file descriptor still refers to it — even
/// after the exporter's GEM handle is gone.
struct HostResourceDmaBuf {
    /// Canonical GEM resource. Export, import, framebuffer and scanout all
    /// clone this same Arc, so RESOURCE_UNREF occurs at the real last use.
    resource: Arc<GpuResource>,
}

impl FileLike for HostResourceDmaBuf {
    fn validate_write_access(&self) -> StarryResult {
        Err(StarryError::InvalidInput)
    }

    fn seek(&self, pos: ax_io::SeekFrom) -> StarryResult<u64> {
        dma_buf_seek(self.resource.size, pos)
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:dmabuf".into()
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> StarryResult<DeviceMmap> {
        // The host resource is not guest-mappable without RESOURCE_MAP_BLOB
        // (not needed by the present path — virgl reuses the host texture
        // zero-copy). Rejecting mmap is safer than mapping the guest shadow
        // pages, which do not hold the rendered content.
        Err(StarryError::Unsupported)
    }
}

impl Pollable for HostResourceDmaBuf {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

/// Host-resource info for a blob dma-buf imported via `PRIME_FD_TO_HANDLE`.
///
/// Linux returns the *same* GEM object on a same-device import, so
/// `RESOURCE_INFO` on the imported handle must resolve back to the
/// exporter's host resource and blob_mem — this is what makes Mesa take the
/// `maybe_untyped` path and reuse the host texture (`virgl_drm_winsys.c`).
struct ImportedBlob {
    owner: u64,
    resource: Arc<GpuResource>,
}

/// Per-open virgl context state.
struct PerFdCtx {
    /// CONTEXT_INIT parameters. Kept for per-fd semantics (a future
    /// context-info query may report them); not read today.
    #[allow(dead_code)]
    capset_id: u32,
    #[allow(dead_code)]
    num_rings: u32,
    ctx_id: ContextHandle,
    attached_resources: BTreeMap<u32, Arc<GpuResource>>,
}

/// Whether this ioctl changed the context's resource attachments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachmentState {
    NoContext,
    AlreadyAttached,
    NewlyAttached,
}

/// Per-open DRM state, equivalent to Linux `struct drm_file` plus the
/// virtio-gpu private context. `dup` and `fork` share this Arc; reopening the
/// device creates a new handle namespace and rendering context.
struct Card0File {
    base: KernelFile,
    card: Arc<Card0>,
    is_primary: bool,
    file_id: u64,
    events: Mutex<Card0Events>,
    poll_rx: PollSet,
    context: Mutex<Option<PerFdCtx>>,
    operation: Mutex<()>,
}

pub struct Card0 {
    /// Weak self-reference used only to create per-open file descriptions.
    self_weak: Weak<Card0>,
    /// Device-wide sequence clock; event ownership belongs to each open file.
    vblank: VblankClock,
    /// Wakes all per-file deadline workers when scanout or pending work changes.
    vblank_event: Arc<Event>,
    /// Serializes cross-fd modesets while the state lock is released for I/O.
    modeset_operation: Mutex<()>,
    /// Published modeset state. Cross-fd operations use `modeset_operation`;
    /// this short-lived lock is released before GPU/display calls, user copies
    /// and event wakeups. Display IRQ handling never takes it.
    state: Mutex<ModesetState>,
    /// `CREATE_DUMB`-allocated buffers keyed by handle. Dropping an
    /// entry releases Card0's strong ref on the backing pages; user
    /// mappings hold their own refs via `LinearBackend::retain`.
    dumbs: Mutex<BTreeMap<u32, DumbBuffer>>,
    /// Next dumb handle to hand out.
    next_dumb_handle: AtomicU32,
    /// Monotonic counter for the mmap-offset key each `MAP_DUMB`
    /// returns. Advanced by [`DUMB_BUFFER_OFFSET_STRIDE`] per allocation
    /// so no two buffers share an offset, even across destroy+recreate.
    next_offset: AtomicU64,
    /// `ADDFB2`-registered framebuffer ids, mapped to the dumb handle
    /// they were built over. Cleared on `RMFB`.
    fbs: Mutex<BTreeMap<u32, Framebuffer>>,
    /// Scanout pins its backing or device resource after the framebuffer and
    /// its GEM handle have been removed.
    scanout_pin: Mutex<Option<FbBacking>>,
    /// Old scanouts awaiting confirmation that the device stopped using them.
    retired_scanouts: Mutex<Vec<(Completion, FbBacking)>>,
    scanout_reaper_running: AtomicBool,
    /// Next fb id to hand out.
    next_fb_id: AtomicU32,
    /// User-published blobs. A committed mode pins its own reference in state.
    blobs: Mutex<BTreeMap<u32, Arc<Vec<u8>>>>,
    /// Next blob id to hand out.
    next_blob_id: AtomicU32,
    /// Kernel-owned immutable blobs (e.g. plane `IN_FORMATS`) keyed by
    /// blob_id. Read-only after publish; never freed; DESTROY_BLOB
    /// refuses to remove ids in this table.
    system_blobs: Mutex<BTreeMap<u32, Arc<Vec<u8>>>>,
    /// Cached blob_id for the `IN_FORMATS` property. Allocated once
    /// under `system_blobs_init` so concurrent first-callers cannot
    /// each leak their own copy into `system_blobs`.
    in_formats_blob: AtomicU32,
    /// Serializes the lazy initialization of `in_formats_blob` so
    /// only one allocation lands in `system_blobs`.
    system_blobs_init: Mutex<()>,
    // ---- 3D (virgl) resource management ----
    /// 3D resources keyed by virtio-gpu resource ID. Each resource tracks
    /// its associated GEM handle, geometry, and size for transfer validation.
    gpu_resources: Mutex<BTreeMap<u32, Arc<GpuResource>>>,
    /// Imported blob dma-bufs: GEM handle (from `PRIME_FD_TO_HANDLE`) →
    /// host resource info. A same-device import is the same host resource
    /// the exporter created, so `RESOURCE_INFO` on an imported handle
    /// resolves back to the original `res_handle` + `blob_mem`.
    ///
    /// Each entry holds one reference on the host resource for the importer.
    blob_aliases: Mutex<BTreeMap<u32, ImportedBlob>>,
    /// Next virtio-gpu resource ID to allocate. Starts at 1 (0 is reserved).
    next_res_handle: AtomicU32,
    /// Next virgl context ID. Linux: `atomic_inc_return(&vgdev->ctx_id_cursor)`.
    /// Each CONTEXT_INIT call gets a unique id so multiple fds/clients
    /// don't share (and corrupt) the same virgl context state.
    next_ctx_id: AtomicU32,
    /// Stable identity assigned to each open file description.
    next_file_id: AtomicU64,
    /// Live primary-node file descriptions, serialized with final close by
    /// `modeset_operation` during open and drop.
    open_files: Mutex<usize>,
    /// Weak per-open references for completing queued events on CRTC disable.
    vblank_files: Mutex<Vec<Weak<Card0File>>>,
    /// Cached capset data keyed by (capset_id, version). GET_CAPS results
    /// are cached here so repeated queries don't round-trip to the host.
    capset_cache: Mutex<BTreeMap<(u32, u32), Vec<u8>>>,
}

#[derive(Default)]
struct Card0Events {
    ready: VecDeque<EventSlot>,
    pending: Vec<PendingVblankEvent>,
    reserved: usize,
    shutdown: bool,
}

struct EventReservation<'a> {
    events: &'a Mutex<Card0Events>,
    poll_rx: &'a PollSet,
    reserved: bool,
}

impl EventReservation<'_> {
    fn new<'a>(events: &'a Mutex<Card0Events>, poll_rx: &'a PollSet) -> VfsResult<EventReservation<'a>> {
        let mut state = events.lock();
        if !state.has_space() {
            return Err(VfsError::NoMemory);
        }
        state.reserved += 1;
        Ok(EventReservation {
            events,
            poll_rx,
            reserved: true,
        })
    }

    fn enqueue<E: bytemuck::NoUninit>(mut self, event: &E) {
        let mut state = self.events.lock();
        state.reserved -= 1;
        state.ready.push_back(serialize_event(event));
        self.reserved = false;
        drop(state);
        unsafe { self.poll_rx.wake(IoEvents::IN) };
    }
}

impl Drop for EventReservation<'_> {
    fn drop(&mut self) {
        if self.reserved {
            self.events.lock().reserved -= 1;
        }
    }
}

impl Card0Events {
    fn has_space(&self) -> bool {
        self.ready.len() + self.pending.len() + self.reserved < MAX_EVENTS
    }

    fn next_deadline(&self, clock: &VblankClock, now_ns: u64) -> Option<core::time::Duration> {
        self.pending
            .iter()
            .filter_map(|event| clock.deadline_ns(event.target_sequence, now_ns))
            .map(core::time::Duration::from_nanos)
            .min()
    }

    fn complete_pending(&mut self, sequence: u64, edge_ns: u64) -> bool {
        let pending = core::mem::take(&mut self.pending);
        let delivered = !pending.is_empty();
        for event in pending {
            self.ready.push_back(vblank_event_slot(event, sequence, edge_ns));
        }
        delivered
    }
}

impl Card0File {
    fn card(&self) -> &Card0 {
        &self.card
    }

    fn reserve_event(&self) -> VfsResult<EventReservation<'_>> {
        EventReservation::new(&self.events, &self.poll_rx)
    }

    fn enqueue_event<E: bytemuck::NoUninit>(&self, event: &E) -> VfsResult<()> {
        let mut state = self.events.lock();
        if !state.has_space() {
            return Err(VfsError::NoMemory);
        }
        state.ready.push_back(serialize_event(event));
        drop(state);
        unsafe { self.poll_rx.wake(IoEvents::IN) };
        Ok(())
    }

    fn queue_pending(&self, event: PendingVblankEvent) -> VfsResult<()> {
        let mut state = self.events.lock();
        if !state.has_space() {
            return Err(VfsError::NoMemory);
        }
        state.pending.push(event);
        Ok(())
    }

    fn serve_pending_vblank_events(&self) {
        let card = self.card();
        let _mode = card.state.lock();
        let Some((current, edge_ns)) = card.vblank.active_at(monotonic_time_nanos()) else {
            return;
        };
        let mut state = self.events.lock();
        let mut delivered = false;
        let mut idx = 0;
        while idx < state.pending.len() {
            if vblank_passed(current, state.pending[idx].target_sequence) {
                let event = state.pending.remove(idx);
                state.ready.push_back(vblank_event_slot(event, current, edge_ns));
                delivered = true;
            } else {
                idx += 1;
            }
        }
        drop(state);
        drop(_mode);
        if delivered {
            unsafe { self.poll_rx.wake(IoEvents::IN) };
        }
    }

    fn read_events(&self, dst: &mut IoDst) -> StarryResult<usize> {
        self.serve_pending_vblank_events();
        let mut state = self.events.lock();
        let mut written = 0;
        while dst.remaining_mut() >= EVENT_SLOT_BYTES {
            let Some(event) = state.ready.front() else {
                break;
            };
            if let Err(error) = dst.write_all(event) {
                return if written == 0 {
                    Err(error.into())
                } else {
                    Ok(written)
                };
            }
            state.ready.pop_front();
            written += EVENT_SLOT_BYTES;
        }
        if written == 0 {
            Err(VfsError::WouldBlock.into())
        } else {
            Ok(written)
        }
    }
}

fn serialize_event<E: bytemuck::NoUninit>(event: &E) -> EventSlot {
    let bytes = bytes_of(event);
    debug_assert_eq!(bytes.len(), EVENT_SLOT_BYTES);
    let mut slot = [0; EVENT_SLOT_BYTES];
    slot[..bytes.len()].copy_from_slice(bytes);
    slot
}

fn vblank_event_slot(pending: PendingVblankEvent, sequence: u64, edge_ns: u64) -> EventSlot {
    match pending.event {
        QueuedVblankEvent::Vblank { user_data } => serialize_event(&DrmEventVblank {
            base: DrmEvent {
                event_type: DRM_EVENT_VBLANK,
                length: EVENT_SLOT_BYTES as u32,
            },
            user_data,
            tv_sec: (edge_ns / 1_000_000_000) as u32,
            tv_usec: ((edge_ns % 1_000_000_000) / 1_000) as u32,
            sequence: sequence as u32,
            crtc_id: CRTC_ID,
        }),
        QueuedVblankEvent::CrtcSequence { user_data } => serialize_event(&DrmEventCrtcSequence {
            base: DrmEvent {
                event_type: DRM_EVENT_CRTC_SEQUENCE,
                length: EVENT_SLOT_BYTES as u32,
            },
            user_data,
            tv_ns: edge_ns as i64,
            sequence,
        }),
    }
}

async fn run_vblank_timer(weak: Weak<Card0File>) {
    loop {
        let arm_event = {
            let Some(file) = weak.upgrade() else { return };
            file.card.vblank_event.clone()
        };
        listener!(arm_event => listener);
        let deadline = {
            let Some(file) = weak.upgrade() else { return };
            let state = file.events.lock();
            if state.shutdown {
                return;
            }
            state.next_deadline(&file.card().vblank, monotonic_time_nanos())
        };
        match deadline {
            None => listener.await,
            Some(deadline) => {
                if timeout_at(Some(deadline), listener).await.is_err() {
                    let Some(file) = weak.upgrade() else { return };
                    file.serve_pending_vblank_events();
                }
            }
        }
    }
}

fn run_scanout_reaper(weak: Weak<Card0>) {
    loop {
        crate::task::sleep(Duration::from_millis(20));
        let Some(card) = weak.upgrade() else { return };
        card.reap_retired_scanouts();
        if card.retired_scanouts.lock().is_empty() {
            card.scanout_reaper_running.store(false, Ordering::Release);
            // A modeset may have queued another completion while this worker
            // was shutting down. It will either start a worker or be seen here.
            card.start_scanout_reaper();
            return;
        }
    }
}

fn display_device_lost(error: &DisplayError) -> bool {
    matches!(
        error,
        DisplayError::DeviceLost | DisplayError::Gpu(GpuError::DeviceLost)
    )
}

fn pending_scanouts_after_query<T>(
    retired: Vec<(Completion, T)>,
    mut query: impl FnMut(Completion) -> Result<CompletionStatus, DisplayError>,
) -> (Vec<(Completion, T)>, bool) {
    let mut pending = Vec::new();
    for (completion, pin) in retired {
        match query(completion) {
            Ok(CompletionStatus::Complete) => {}
            // A device reset ends every outstanding scanout, including the
            // earlier pending entries and those not queried yet.
            Err(error) if display_device_lost(&error) => return (Vec::new(), true),
            _ => pending.push((completion, pin)),
        }
    }
    (pending, false)
}

impl Card0 {
    /// Called with the modeset state locked, so queue submission cannot race
    /// the transition from an active clock to a completed pending event.
    fn set_vblank_active(
        &self,
        state: &ModesetState,
        now_ns: u64,
    ) -> Option<Vec<(Arc<Card0File>, bool)>> {
        let active = state.crtc_active != 0;
        let period_ns = state
            .mode
            .as_ref()
            .map_or(VBLANK_PERIOD_NS, |mode| mode_period_ns(&mode.info));
        if !self.vblank.set_active(active, now_ns, period_ns) {
            return None;
        }
        let mut completed = Vec::new();
        if !active {
            let (sequence, edge_ns) = self.vblank.snapshot_at(now_ns);
            let files = {
                let mut registry = self.vblank_files.lock();
                registry.retain(|file| file.strong_count() > 0);
                registry.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
            };
            for file in files {
                let delivered = file.events.lock().complete_pending(sequence, edge_ns);
                // Even idle files must remain alive until the modeset lock is released.
                completed.push((file, delivered));
            }
        }
        Some(completed)
    }

    fn notify_vblank_change(&self, completed: Option<Vec<(Arc<Card0File>, bool)>>) {
        if let Some(completed) = completed {
            self.vblank_event.notify(usize::MAX);
            for (file, delivered) in completed {
                if delivered {
                    unsafe { file.poll_rx.wake(IoEvents::IN) };
                }
            }
        }
    }

    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            self_weak: weak.clone(),
            vblank: VblankClock::new(monotonic_time_nanos()),
            vblank_event: Arc::new(Event::new()),
            modeset_operation: Mutex::new(()),
            state: Mutex::new(ModesetState::default()),
            dumbs: Mutex::new(BTreeMap::new()),
            next_dumb_handle: AtomicU32::new(FIRST_DUMB_HANDLE),
            // Start at STRIDE rather than 0 so a zero `offset` argument
            // on `mmap` is unambiguous (it means "hasn't called
            // MAP_DUMB yet").
            next_offset: AtomicU64::new(DUMB_BUFFER_OFFSET_STRIDE),
            fbs: Mutex::new(BTreeMap::new()),
            scanout_pin: Mutex::new(None),
            retired_scanouts: Mutex::new(Vec::new()),
            scanout_reaper_running: AtomicBool::new(false),
            next_fb_id: AtomicU32::new(FIRST_FB_ID),
            blobs: Mutex::new(BTreeMap::new()),
            next_blob_id: AtomicU32::new(FIRST_BLOB_ID),
            system_blobs: Mutex::new(BTreeMap::new()),
            in_formats_blob: AtomicU32::new(0),
            system_blobs_init: Mutex::new(()),
            // 3D resource management
            gpu_resources: Mutex::new(BTreeMap::new()),
            blob_aliases: Mutex::new(BTreeMap::new()),
            next_res_handle: AtomicU32::new(FIRST_GPU_RESOURCE_ID),
            next_ctx_id: AtomicU32::new(FIRST_VIRGL_CTX_ID),
            next_file_id: AtomicU64::new(1),
            open_files: Mutex::new(0),
            vblank_files: Mutex::new(Vec::new()),
            capset_cache: Mutex::new(BTreeMap::new()),
        })
    }

    /// Lazily construct the `IN_FORMATS` blob the first time a caller
    /// asks for plane properties. Holds `system_blobs_init` across the
    /// allocate-and-publish so a concurrent first-caller cannot leak
    /// a parallel copy into `system_blobs`. The blob lives there
    /// permanently — `handle_destroy_blob` refuses ids it covers.
    fn ensure_in_formats_blob(&self) -> VfsResult<u32> {
        let cur = self.in_formats_blob.load(Ordering::Acquire);
        if cur != 0 {
            return Ok(cur);
        }
        let formats = display_plane_formats()?;
        let _guard = self.system_blobs_init.lock();
        let cur = self.in_formats_blob.load(Ordering::Acquire);
        if cur != 0 {
            return Ok(cur);
        }
        let bytes = build_in_formats_blob(&formats);
        let id = self.next_blob_id.fetch_add(1, Ordering::Relaxed);
        self.system_blobs.lock().insert(id, Arc::new(bytes));
        self.in_formats_blob.store(id, Ordering::Release);
        Ok(id)
    }
}

/// True if `inner` is the shared `/dev/dri/card0` or render-node device.
pub(crate) fn is_card0_device(inner: &dyn Any) -> bool {
    inner.is::<Card0>()
}

/// Build one Linux-style open file description for card0/renderD128.
pub(crate) fn open_card0_file(
    inner: &dyn Any,
    file: ax_fs_ng::File,
    open_flags: u32,
) -> StarryResult<Arc<dyn FileLike>> {
    let card = inner
        .downcast_ref::<Card0>()
        .ok_or(StarryError::NoSuchDevice)?;
    let card = card.self_weak.upgrade().ok_or(StarryError::NoSuchDevice)?;
    let is_primary = file.location().metadata()?.rdev == DeviceId::new(226, 0);
    let opened = Arc::new(Card0File::new(
        KernelFile::new(file, open_flags),
        card,
        is_primary,
    ));
    let weak = Arc::downgrade(&opened);
    crate::task::kernel_thread_builder(format!("card0-vblank-{}", opened.file_id))
        .spawn({
            let weak = weak.clone();
            move || block_on(run_vblank_timer(weak))
        })
        .map_err(|_| StarryError::NoMemory)?;
    opened.card.vblank_files.lock().push(weak);
    Ok(opened)
}

/// Write a kernel-owned `src` into a user buffer. Returns the number of
/// bytes the kernel tried to write (for the truncated-write `*_len =
/// len(src)` convention DRM's VERSION ioctl uses).
fn write_user_string(
    current: &crate::task::UserTaskRef,
    user_ptr: u64,
    user_cap: usize,
    src: &str,
) -> VfsResult<usize> {
    let n = user_cap.min(src.len());
    if n > 0 {
        vm_write_slice(current, user_ptr as *mut u8, &src.as_bytes()[..n])
            .map_err(|_| VfsError::BadAddress)?;
    }
    Ok(src.len())
}

/// Write up to `cap` `T`s from `src` into `user_ptr`; returns the total
/// source length.
fn report_user_array<T: bytemuck::NoUninit>(
    current: &crate::task::UserTaskRef,
    user_ptr: u64,
    cap: u32,
    src: &[T],
) -> VfsResult<u32> {
    if user_ptr != 0 {
        let to_write = (cap as usize).min(src.len());
        vm_write_slice(current, user_ptr as *mut T, &src[..to_write])
            .map_err(|_| VfsError::BadAddress)?;
    }
    Ok(src.len() as u32)
}

fn display_output_info() -> VfsResult<OutputInfo> {
    ax_gpu::with_display(|device| {
        let first = device.output(OutputId::new(0))?;
        if first.connected {
            return Ok(first);
        }
        Ok((1..device.output_count())
            .map(OutputId::new)
            .find_map(|id| device.output(id).ok().filter(|output| output.connected))
            .unwrap_or(first))
    })
        .map_err(map_display_err)?
        .map_err(map_display_err)
}

fn kms_available() -> bool {
    ax_gpu::has_display_controller()
}

fn dumb_supported() -> bool {
    kms_available()
        || ax_gpu::capabilities().is_some_and(|capabilities| capabilities.supports_image_2d)
}

fn format_to_fourcc(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Rgb565 => DRM_FORMAT_RGB565,
        PixelFormat::Rgb888 => DRM_FORMAT_RGB888,
        PixelFormat::Bgr888 => DRM_FORMAT_BGR888,
        PixelFormat::Xrgb8888 => DRM_FORMAT_XRGB8888,
        PixelFormat::Argb8888 => DRM_FORMAT_ARGB8888,
        PixelFormat::Xbgr8888 => DRM_FORMAT_XBGR8888,
    }
}

fn fourcc_to_format(fourcc: u32) -> Option<PixelFormat> {
    match fourcc {
        DRM_FORMAT_RGB565 => Some(PixelFormat::Rgb565),
        DRM_FORMAT_RGB888 => Some(PixelFormat::Rgb888),
        DRM_FORMAT_BGR888 => Some(PixelFormat::Bgr888),
        DRM_FORMAT_XRGB8888 => Some(PixelFormat::Xrgb8888),
        DRM_FORMAT_ARGB8888 => Some(PixelFormat::Argb8888),
        DRM_FORMAT_XBGR8888 => Some(PixelFormat::Xbgr8888),
        _ => None,
    }
}

fn display_plane_formats() -> VfsResult<Vec<u32>> {
    let mut formats = Vec::new();
    for format in display_output_info()?.formats {
        let fourcc = format_to_fourcc(format);
        if !formats.contains(&fourcc) {
            formats.push(fourcc);
        }
    }
    Ok(formats)
}

/// Resolution advertised by the bound output.
fn display_resolution() -> (u32, u32) {
    display_output_info()
        .ok()
        .and_then(|output| output.preferred_mode.or_else(|| output.modes.first().copied()))
        .map_or((0, 0), |mode| (mode.width, mode.height))
}

/// VESA CVT-RBv1 (Coordinated Video Timings, Reduced Blanking — 2003)
/// constants. A device may omit timings, but userspace mode validators
/// require a self-consistent DRM mode structure.
const CVT_RB_HFRONT_PORCH: u16 = 48;
const CVT_RB_HSYNC_WIDTH: u16 = 32;
const CVT_RB_HBACK_PORCH: u16 = 80;
const CVT_RB_VFRONT_PORCH: u16 = 3;
const CVT_RB_VSYNC_WIDTH: u16 = 8;
const CVT_RB_VBACK_PORCH: u16 = 6;

/// Default output refresh rate.
const DEFAULT_VREFRESH: u32 = 60;

fn rounded_refresh_hz(refresh_millihz: u32) -> u32 {
    (refresh_millihz.saturating_add(500) / 1000).max(1)
}

fn mode_period_ns(mode: &DrmModeModeInfo) -> u64 {
    let pixels = u64::from(mode.htotal) * u64::from(mode.vtotal);
    let period = if pixels != 0 && mode.clock != 0 {
        pixels * 1_000_000 / u64::from(mode.clock)
    } else if mode.vrefresh != 0 {
        1_000_000_000 / u64::from(mode.vrefresh)
    } else {
        VBLANK_PERIOD_NS
    };
    // A user-supplied mode must not arm a busy-looping deadline worker.
    period.clamp(1_000_000, 1_000_000_000)
}

/// Synthesized mode matching the display's current resolution.
fn current_mode(mode: DisplayMode) -> DrmModeModeInfo {
    let (w, h) = (mode.width, mode.height);
    let mut name = [0u8; 32];
    let s = b"current";
    name[..s.len()].copy_from_slice(s);

    let hdisplay = w as u16;
    let hsync_start = hdisplay + CVT_RB_HFRONT_PORCH;
    let hsync_end = hsync_start + CVT_RB_HSYNC_WIDTH;
    let htotal = hsync_end + CVT_RB_HBACK_PORCH;

    let vdisplay = h as u16;
    let vsync_start = vdisplay + CVT_RB_VFRONT_PORCH;
    let vsync_end = vsync_start + CVT_RB_VSYNC_WIDTH;
    let vtotal = vsync_end + CVT_RB_VBACK_PORCH;

    let refresh_millihz = if mode.refresh_millihz == 0 {
        DEFAULT_VREFRESH * 1000
    } else {
        mode.refresh_millihz
    };
    let vrefresh = rounded_refresh_hz(refresh_millihz);
    let clock = ((u64::from(htotal) * u64::from(vtotal) * u64::from(refresh_millihz)
        + 500_000)
        / 1_000_000)
        .min(u64::from(u32::MAX)) as u32;

    DrmModeModeInfo {
        clock,
        hdisplay,
        hsync_start,
        hsync_end,
        htotal,
        hskew: 0,
        vdisplay,
        vsync_start,
        vsync_end,
        vtotal,
        vscan: 0,
        vrefresh,
        flags: 0,
        kind: 0,
        name,
    }
}

impl DeviceOps for Card0 {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::BadFileDescriptor)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::BadFileDescriptor)
    }

    fn ioctl(&self, _current: &UserTaskRef, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        // The shared device node has no Linux `drm_file` state. `open(2)`
        // wraps it in `Card0File`, which owns handles, events and context.
        Err(VfsError::NotATty)
    }

    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        DeviceMmap::None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

impl Card0File {
    fn new(base: KernelFile, card: Arc<Card0>, is_primary: bool) -> Self {
        let file_id = card.next_file_id.fetch_add(1, Ordering::Relaxed);
        // A final close cannot clear scanout between this open and its count.
        if is_primary {
            let _modeset = card.modeset_operation.lock();
            *card.open_files.lock() += 1;
        }
        Self {
            base,
            card,
            is_primary,
            file_id,
            events: Mutex::new(Card0Events::default()),
            poll_rx: PollSet::new(),
            context: Mutex::new(None),
            operation: Mutex::new(()),
        }
    }

    /// Creates a per-fd virgl context on the host and publishes it, returning
    /// the new `ctx_id`.
    ///
    /// Shared by the explicit `DRM_IOCTL_VIRTGPU_CONTEXT_INIT` handler and the
    /// legacy lazy paths so both allocate a unique host context id, generate a
    /// unique debug name, and publish `PerFdCtx` in the same order.
    ///
    /// Publish order matters: the id is reserved first (Linux
    /// `atomic_inc_return(&vgdev->ctx_id_cursor)`), the host call runs next,
    /// and only a successful host create installs the state. A failed host
    /// create therefore leaves `context` untouched — no partially built
    /// `PerFdCtx` is visible to `attach_resource` or `Drop`. Sparse ids after a
    /// failure are fine; Linux never reuses a context id either.
    ///
    /// Fails with `Unsupported` when virgl was not negotiated, matching the
    /// device core, which rejects every 3D command without that feature.
    fn create_context(&self, kind: CreateKind) -> VfsResult<ContextHandle> {
        let (context_init, num_rings) = match kind {
            CreateKind::Legacy => (0, 1),
            CreateKind::Explicit {
                capset_id,
                num_rings,
            } => (capset_id, num_rings),
        };

        // The device core rejects 3D commands without VIRGL; failing here keeps
        // the error a single, predictable `Unsupported` instead of a host
        // round-trip that can only fail.
        if !ax_gpu::capabilities().is_some_and(|caps| caps.supports_3d) {
            return Err(VfsError::Unsupported);
        }

        let label_id = self.card.next_ctx_id.fetch_add(1, Ordering::Relaxed);
        // Name encodes the unique `ctx_id` rather than the capset, so two
        // contexts never share the debug label and host-side error logs
        // ("starry-ctx-2") stay attributable to one client.
        let ctx_name = format!("starry-ctx-{label_id}");
        let ctx_id = ax_gpu::with_gpu(|device| {
            device
                .virgl()
                .ok_or(GpuError::Unsupported)?
                .create_context(&ctx_name, context_init)
        })
        .map_err(map_gpu_err)?
        .map_err(map_gpu_err)?;

        // Published only after the host accepted the create.
        *self.context.lock() = Some(PerFdCtx {
            capset_id: context_init,
            num_rings,
            ctx_id,
            attached_resources: BTreeMap::new(),
        });
        Ok(ctx_id)
    }

    /// Returns this fd's `ctx_id`, creating the legacy default context if
    /// userspace never ran `DRM_IOCTL_VIRTGPU_CONTEXT_INIT`.
    ///
    /// Mirrors `virtio_gpu_create_context()`: the lazy virgl paths
    /// (`RESOURCE_CREATE`, `EXECBUFFER`, the 3D transfers and host-backed
    /// blobs) call it before any `CTX`-tagged command, so they work even
    /// without an explicit context-init ioctl. Linux takes this create path on
    /// every such ioctl regardless of whether `CONTEXT_INIT` was negotiated,
    /// and it always builds the default `context_init = 0` context; an fd that
    /// already holds one just gets it reported back unchanged.
    ///
    /// The explicit `DRM_IOCTL_VIRTGPU_CONTEXT_INIT` path is what userspace
    /// uses once `GETPARAM(CONTEXT_INIT)` reports 1 (Mesa does this before its
    /// first submit), so in practice the lazy fallback is a safety net that
    /// supplies the default context when userspace skipped that ioctl.
    ///
    /// Callers must already hold `operation`, which serializes every mutable
    /// `Card0File` ioctl on this open file description; the probe-then-create
    /// sequence below relies on that serialization to stay race-free.
    fn ensure_lazy_context(&self) -> VfsResult<ContextHandle> {
        if let Some(context) = self.context.lock().as_ref() {
            return Ok(context.ctx_id);
        }
        self.create_context(CreateKind::Legacy)
    }

    /// Attaches `resource` to this fd's context when one exists.
    ///
    /// A missing context is a successful no-op for context-conditional PRIME
    /// imports. Only a newly attached resource belongs to this ioctl's
    /// failure rollback; an earlier attachment must stay in place.
    fn attach_resource_if_ready(&self, resource: &Arc<GpuResource>) -> VfsResult<AttachmentState> {
        let ctx_id = {
            let guard = self.context.lock();
            let Some(context) = guard.as_ref() else {
                return Ok(AttachmentState::NoContext);
            };
            if context.attached_resources.contains_key(&resource.res_handle) {
                return Ok(AttachmentState::AlreadyAttached);
            }
            context.ctx_id
        };
        ax_gpu::with_gpu(|device| {
            device
                .virgl()
                .ok_or(GpuError::Unsupported)?
                .attach_resource(ctx_id, resource.device_handle)
        })
        .map_err(map_gpu_err)?
        .map_err(map_gpu_err)?;
        self.context
            .lock()
            .as_mut()
            .ok_or(VfsError::InvalidInput)?
            .attached_resources
            .insert(resource.res_handle, resource.clone());
        Ok(AttachmentState::NewlyAttached)
    }

    /// Attaches `resource` to the fd context, failing when the fd has none.
    ///
    /// Used by paths that have already created the context, so a missing one
    /// is a real error rather than an expected state.
    fn attach_resource(&self, resource: &Arc<GpuResource>) -> VfsResult<()> {
        self.attach_resource_if_ready(resource)
            .and_then(|state| (state != AttachmentState::NoContext).then_some(()).ok_or(VfsError::InvalidInput))
    }

    fn detach_resource(&self, resource_id: u32) {
        let (ctx_id, resource) = {
            let guard = self.context.lock();
            let Some(context) = guard.as_ref() else {
                return;
            };
            let Some(resource) = context.attached_resources.get(&resource_id).cloned() else {
                return;
            };
            (context.ctx_id, resource)
        };
        let result = ax_gpu::with_gpu_for_cleanup(|device| {
            device
                .virgl()
                .ok_or(GpuError::Unsupported)?
                .detach_resource(ctx_id, resource.device_handle)
        });
        if matches!(result, Ok(Ok(())))
            && let Some(context) = self.context.lock().as_mut()
        {
            context.attached_resources.remove(&resource_id);
        }
    }

    fn ioctl_inner(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        let card = &self.card;
        let number = cmd & 0xff;
        if (0x41..=0x4b).contains(&number)
            && ax_gpu::identity().is_none_or(|identity| identity.driver_name != "virtio_gpu")
        {
            return Err(VfsError::NotATty);
        }
        let kms_ioctl = matches!(
            cmd,
            DRM_IOCTL_CRTC_GET_SEQUENCE | DRM_IOCTL_CRTC_QUEUE_SEQUENCE
        ) || (number >= 0xa0
            && !matches!(
                cmd,
                DRM_IOCTL_MODE_CREATE_DUMB
                    | DRM_IOCTL_MODE_MAP_DUMB
                    | DRM_IOCTL_MODE_DESTROY_DUMB
            ));
        if !kms_available() && kms_ioctl {
            return Err(VfsError::Unsupported);
        }
        match cmd {
            DRM_IOCTL_VERSION => handle_version(current, arg),
            DRM_IOCTL_GET_UNIQUE => handle_get_unique(current, arg),
            DRM_IOCTL_SET_VERSION => handle_set_version(current, arg),
            DRM_IOCTL_GET_CAP => handle_get_cap(current, arg),
            DRM_IOCTL_SET_CLIENT_CAP => handle_set_client_cap(current, arg),
            DRM_IOCTL_SET_MASTER | DRM_IOCTL_DROP_MASTER => Ok(0),
            DRM_IOCTL_MODE_GETRESOURCES => handle_get_resources(current, arg),
            DRM_IOCTL_MODE_GETCRTC => card.handle_get_crtc(current, arg),
            DRM_IOCTL_MODE_SETCRTC => card.handle_set_crtc(current, arg),
            DRM_IOCTL_MODE_GETENCODER => handle_get_encoder(current, arg),
            DRM_IOCTL_MODE_GETCONNECTOR => handle_get_connector(current, arg),
            DRM_IOCTL_MODE_ADDFB2 => card.handle_addfb2(self, current, arg),
            DRM_IOCTL_MODE_RMFB => card.handle_rmfb(self, current, arg),
            DRM_IOCTL_MODE_CREATE_DUMB => card.handle_create_dumb(self, current, arg),
            DRM_IOCTL_MODE_MAP_DUMB => card.handle_map_dumb(self, current, arg),
            DRM_IOCTL_MODE_DESTROY_DUMB => card.handle_destroy_dumb(self, current, arg),
            // GEM_CLOSE is the release path Mesa uses for virgl 3D/blob
            // resources (incl. PRIME imports). Linux funnels both GEM_CLOSE
            // and DESTROY_DUMB through `drm_gem_handle_delete`; we mirror
            // that by sharing one cleanup helper.
            DRM_IOCTL_GEM_CLOSE => card.handle_gem_close(self, current, arg),

            DRM_IOCTL_MODE_GETPLANERESOURCES => handle_get_plane_resources(current, arg),
            DRM_IOCTL_MODE_GETPLANE => card.handle_get_plane(current, arg),
            DRM_IOCTL_MODE_OBJ_GETPROPERTIES => card.handle_obj_get_properties(current, arg),
            DRM_IOCTL_MODE_GETPROPERTY => handle_get_property(current, arg),
            DRM_IOCTL_MODE_PAGE_FLIP => card.handle_page_flip(self, current, arg),
            DRM_IOCTL_CRTC_GET_SEQUENCE | DRM_IOCTL_CRTC_QUEUE_SEQUENCE if !self.is_primary => {
                Err(VfsError::PermissionDenied)
            }
            DRM_IOCTL_CRTC_GET_SEQUENCE => card.handle_crtc_get_sequence(current, arg),
            DRM_IOCTL_CRTC_QUEUE_SEQUENCE => card.handle_crtc_queue_sequence(self, current, arg),

            DRM_IOCTL_MODE_ATOMIC => card.handle_atomic(self, current, arg),
            DRM_IOCTL_MODE_CREATEPROPBLOB => card.handle_create_blob(current, arg),
            DRM_IOCTL_MODE_DESTROYPROPBLOB => card.handle_destroy_blob(current, arg),
            DRM_IOCTL_MODE_GETPROPBLOB => card.handle_get_blob(current, arg),

            DRM_IOCTL_GET_MAGIC => handle_get_magic(current, arg),
            DRM_IOCTL_AUTH_MAGIC => handle_auth_magic(current, arg),
            DRM_IOCTL_MODE_DIRTYFB => card.handle_dirty_fb(self, current, arg),
            DRM_IOCTL_PRIME_HANDLE_TO_FD => card.handle_prime_handle_to_fd(self, current, arg),
            DRM_IOCTL_PRIME_FD_TO_HANDLE => card.handle_prime_fd_to_handle(self, current, arg),

            // ---- virtgpu 3D ioctls ----
            DRM_IOCTL_VIRTGPU_GETPARAM => card.handle_virtgpu_getparam(current, arg),
            DRM_IOCTL_VIRTGPU_CONTEXT_INIT => card.handle_virtgpu_context_init(self, current, arg),
            DRM_IOCTL_VIRTGPU_GET_CAPS => card.handle_virtgpu_get_caps(current, arg),
            DRM_IOCTL_VIRTGPU_RESOURCE_CREATE => {
                card.handle_virtgpu_resource_create(self, current, arg)
            }
            DRM_IOCTL_VIRTGPU_RESOURCE_INFO => {
                card.handle_virtgpu_resource_info(self, current, arg)
            }
            DRM_IOCTL_VIRTGPU_MAP => card.handle_virtgpu_map(self, current, arg),
            // DRM_IOCTL_VIRTGPU_EXECBUFFER is dispatched by `Card0File::ioctl`
            // on the `StarryResult` path so a fence-fd reservation can report
            // `EMFILE` (TooManyOpenFiles); `VfsError` has no such variant.
            DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST => {
                card.handle_virtgpu_transfer_to_host(self, current, arg)
            }
            DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST => {
                card.handle_virtgpu_transfer_from_host(self, current, arg)
            }
            DRM_IOCTL_VIRTGPU_WAIT => card.handle_virtgpu_wait(self, current, arg),
            DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB => {
                card.handle_virtgpu_resource_create_blob(self, current, arg)
            }

            _ => {
                warn!("[card0] unsupported ioctl cmd=0x{:08x}", cmd);
                Err(VfsError::OperationNotSupported)
            }
        }
    }
}

// Mirrors the DRM_RENDER_ALLOW entries in Linux's core and virtio-gpu ioctl
// tables. Render clients may manage GPU resources but cannot own KMS state.
fn render_node_allows_ioctl(cmd: u32) -> bool {
    matches!(
        cmd,
        DRM_IOCTL_VERSION
            | DRM_IOCTL_GET_CAP
            | DRM_IOCTL_GEM_CLOSE
            | DRM_IOCTL_PRIME_HANDLE_TO_FD
            | DRM_IOCTL_PRIME_FD_TO_HANDLE
            | DRM_IOCTL_VIRTGPU_MAP
            | DRM_IOCTL_VIRTGPU_EXECBUFFER
            | DRM_IOCTL_VIRTGPU_GETPARAM
            | DRM_IOCTL_VIRTGPU_RESOURCE_CREATE
            | DRM_IOCTL_VIRTGPU_RESOURCE_INFO
            | DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST
            | DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST
            | DRM_IOCTL_VIRTGPU_WAIT
            | DRM_IOCTL_VIRTGPU_GET_CAPS
            | DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB
            | DRM_IOCTL_VIRTGPU_CONTEXT_INIT
    )
}

impl Pollable for Card0File {
    fn poll(&self) -> IoEvents {
        self.serve_pending_vblank_events();
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, !self.events.lock().ready.is_empty());
        events
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        if events.contains(IoEvents::IN) {
            unsafe { sink.register_shared(&self.poll_rx, IoEvents::IN) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        if events.contains(IoEvents::IN) {
            unsafe { sink.register_exclusive(&self.poll_rx, IoEvents::IN) };
        }
    }
}

impl FileLike for Card0File {
    fn validate_write_access(&self) -> StarryResult {
        self.base.validate_write_access()
    }

    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if dst.remaining_mut() == 0 {
            return Ok(0);
        }
        if dst.remaining_mut() < EVENT_SLOT_BYTES {
            return Err(StarryError::InvalidInput);
        }
        let task = current_user_task();
        block_on_user(
            &task,
            poll_io(self, IoEvents::IN, self.nonblocking(), || self.read_events(dst)),
        )
        .into_result()?
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        Err(StarryError::BadFileDescriptor)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        self.base.path()
    }

    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        if !self.is_primary && !render_node_allows_ioctl(cmd) {
            return Err(VfsError::PermissionDenied.into());
        }
        if arg == 0 && !matches!(cmd, DRM_IOCTL_SET_MASTER | DRM_IOCTL_DROP_MASTER) {
            return Err(StarryError::BadAddress);
        }
        // A vblank wait can outlive many other ioctls on this open file.
        // Its CRTC and event state has separate locks from the 3D operation state.
        if cmd == DRM_IOCTL_WAIT_VBLANK {
            if !kms_available() {
                return Err(VfsError::Unsupported.into());
            }
            if !self.is_primary {
                return Err(VfsError::PermissionDenied.into());
            }
            return Ok(self.card.handle_wait_vblank(self, current, arg)?);
        }
        let _operation = self.operation.lock();
        if cmd == DRM_IOCTL_VIRTGPU_EXECBUFFER {
            if ax_gpu::identity().is_none_or(|identity| identity.driver_name != "virtio_gpu") {
                return Err(StarryError::NotATty);
            }
            // Fence-fd reservation must surface `EMFILE`
            // (`StarryError::TooManyOpenFiles`), which the `VfsError` domain
            // cannot represent. This one command therefore runs on the
            // `StarryResult` path; the lock scope above still covers it.
            return self.card.handle_virtgpu_execbuffer(self, current, arg);
        }
        Ok(self.ioctl_inner(current, cmd, arg)?)
    }

    fn device_mmap(&self, offset: u64, length: u64) -> StarryResult<DeviceMmap> {
        let _operation = self.operation.lock();
        let dumbs = self.card.dumbs.lock();
        let buffer = dumbs
            .values()
            .find(|buffer| {
                buffer.owner == self.file_id && buffer.mappable && buffer.offset == offset
            })
            .ok_or(StarryError::InvalidInput)?;
        let physical = buffer.mapping.physical();
        let range = PhysAddrRange::from_start_size(
            physical.start,
            length.min(buffer.mapping.backing().len() as u64) as usize,
        );
        let retain: Arc<dyn Any + Send + Sync> = Arc::new(GpuMappingLease {
            _mapping: buffer.mapping.clone(),
            _resource: buffer.resource.clone(),
        });
        // MAP_DUMB/VIRTGPU_MAP offsets are synthetic lookup keys, not byte
        // offsets within the returned physical range. Mark the mapping as
        // resolved so the generic mmap path does not add the key again.
        Ok(DeviceMmap::PhysicalResolved(range, Some(retain)))
    }

    fn open_flags(&self) -> u32 {
        self.base.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.base.nonblocking()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
        self.base.set_nonblocking(nonblocking)
    }
}

impl Drop for Card0File {
    fn drop(&mut self) {
        let mut events = self.events.lock();
        events.shutdown = true;
        events.pending.clear();
        drop(events);
        self.card.vblank_event.notify(usize::MAX);

        if let Some(context) = self.context.lock().take()
            && let Err(error) = ax_gpu::with_gpu_for_cleanup(|device| {
                device
                    .virgl()
                    .ok_or(GpuError::Unsupported)?
                    .destroy_context(context.ctx_id)
            })
            .and_then(core::convert::identity)
        {
            warn!("failed to destroy GPU context on close: {error}");
        }

        let _modeset = self.card.modeset_operation.lock();
        let last_file = {
            let mut open_files = self.card.open_files.lock();
            let last_file = self.is_primary && *open_files == 1;
            if self.is_primary {
                *open_files -= 1;
            }
            last_file
        };
        let active_id = self.card.state.lock().plane_fb_id;
        let ids = self
            .card
            .fbs
            .lock()
            .iter()
            .filter_map(|(&id, framebuffer)| (framebuffer.owner == self.file_id).then_some(id))
            .collect::<Vec<_>>();
        let clear_active = last_file || ids.contains(&active_id);
        let keep_active = clear_active
            && match self.card.clear_scanout(false) {
                Ok(()) => false,
                Err(error) if display_device_lost(&error) => {
                    self.card.release_scanout_pins_after_device_loss();
                    false
                }
                Err(_) => true,
            };
        if clear_active && !keep_active {
            *self.card.state.lock() = ModesetState::default();
        }
        let removed_framebuffers = {
            let mut framebuffers = self.card.fbs.lock();
            if last_file {
                core::mem::take(&mut *framebuffers)
                    .into_iter()
                    .filter_map(|(id, framebuffer)| {
                        if keep_active && id == active_id {
                            framebuffers.insert(id, framebuffer);
                            None
                        } else {
                            Some(framebuffer)
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                ids.into_iter()
                    .filter(|id| !keep_active || *id != active_id)
                    .filter_map(|id| framebuffers.remove(&id))
                    .collect::<Vec<_>>()
            }
        };
        if last_file {
            self.card.blobs.lock().clear();
        }
        let removed_dumbs = {
            let mut dumbs = self.card.dumbs.lock();
            let handles = dumbs
                .iter()
                .filter_map(|(&handle, buffer)| (buffer.owner == self.file_id).then_some(handle))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .filter_map(|handle| dumbs.remove(&handle))
                .collect::<Vec<_>>()
        };
        let removed_aliases = {
            let mut aliases = self.card.blob_aliases.lock();
            let handles = aliases
                .iter()
                .filter_map(|(&handle, alias)| (alias.owner == self.file_id).then_some(handle))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .filter_map(|handle| aliases.remove(&handle))
                .collect::<Vec<_>>()
        };
        let removed_resources = {
            let mut resources = self.card.gpu_resources.lock();
            let ids = resources
                .iter()
                .filter_map(|(&id, resource)| (resource.owner == self.file_id).then_some(id))
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| resources.remove(&id))
                .collect::<Vec<_>>()
        };
        self.card
            .vblank_files
            .lock()
            .retain(|file| file.strong_count() > 0);
        let state = self.card.state.lock();
        let completed = self
            .card
            .set_vblank_active(&state, monotonic_time_nanos());
        drop((state, _modeset));
        self.card.notify_vblank_change(completed);
        drop((removed_framebuffers, removed_dumbs, removed_aliases, removed_resources));
    }
}

impl Card0 {
    fn release_scanout_pins_after_device_loss(&self) {
        let active = self.scanout_pin.lock().take();
        let retired = core::mem::take(&mut *self.retired_scanouts.lock());
        drop((active, retired));
    }

    fn reap_retired_scanouts(&self) {
        let retired = core::mem::take(&mut *self.retired_scanouts.lock());
        let (pending, device_lost) = pending_scanouts_after_query(retired, |completion| {
            ax_gpu::with_display_for_cleanup(|device| device.commit_status(completion))
                .and_then(core::convert::identity)
        });
        if device_lost {
            self.release_scanout_pins_after_device_loss();
        } else {
            self.retired_scanouts.lock().extend(pending);
        }
    }

    fn retain_old_scanout(&self, completion: Completion, next: Option<FbBacking>) {
        let previous = core::mem::replace(&mut *self.scanout_pin.lock(), next);
        if let (Completion::Pending(_), Some(previous)) = (completion, previous) {
            self.retired_scanouts.lock().push((completion, previous));
        }
        self.reap_retired_scanouts();
        self.start_scanout_reaper();
    }

    fn start_scanout_reaper(&self) {
        if self.retired_scanouts.lock().is_empty()
            || self.scanout_reaper_running.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let weak = self.self_weak.clone();
        if let Err(error) = crate::task::kernel_thread_builder(String::from("card0-scanout-reaper"))
            .spawn(move || run_scanout_reaper(weak))
        {
            self.scanout_reaper_running.store(false, Ordering::Release);
            warn!("failed to start scanout completion worker: {error:?}");
        }
    }

    fn clear_scanout(&self, test_only: bool) -> Result<(), DisplayError> {
        let clear = |device: &mut dyn GpuDisplay| {
            let output = scanout_output_to_clear(device)?;
            let state = DisplayState {
                output,
                mode: None,
                framebuffer: None,
                damage: Vec::new(),
            };
            device.check(&state)?;
            if test_only {
                Ok(None)
            } else {
                device.commit(&state).map(Some)
            }
        };
        let completion = if test_only {
            ax_gpu::with_display(clear)
        } else {
            ax_gpu::with_display_for_cleanup(clear)
        }
        .and_then(core::convert::identity)?;
        if let Some(completion) = completion {
            self.retain_old_scanout(completion, None);
        }
        Ok(())
    }

    /// Commit one GEM framebuffer through the device-independent display API.
    /// The old scanout remains pinned until a successful commit completes.
    fn present_fb(&self, fb_id: u32, proposed: &ModesetState, test_only: bool) -> VfsResult<()> {
        let fb = self
            .fbs
            .lock()
            .get(&fb_id)
            .cloned()
            .ok_or(VfsError::InvalidInput)?;
        let buffer = match &fb.kind {
            FbBacking::Gpu3d { resource } => ScanoutBuffer::Gpu(resource.device_handle),
            FbBacking::Dumb { mapping } => ScanoutBuffer::Backing(mapping.backing()),
        };
        let requested_mode = proposed.mode.as_ref().map(|mode| DisplayMode {
            width: u32::from(mode.info.hdisplay),
            height: u32::from(mode.info.vdisplay),
            refresh_millihz: mode.info.vrefresh.saturating_mul(1000),
        });
        let completion = ax_gpu::with_display(|device| {
            let output = select_output_for_present(device)?;
            let mode = if let Some(requested) = requested_mode {
                output
                    .modes
                    .iter()
                    .copied()
                    .find(|available| {
                        available.width == requested.width
                            && available.height == requested.height
                            && (available.refresh_millihz == 0
                                || rounded_refresh_hz(available.refresh_millihz)
                                    == requested.refresh_millihz / 1000)
                    })
                    .ok_or(DisplayError::InvalidState)?
            } else {
                output
                    .preferred_mode
                    .or_else(|| output.modes.first().copied())
                    .ok_or(DisplayError::InvalidState)?
            };
            let state = DisplayState {
                output: output.id,
                mode: Some(mode),
                framebuffer: Some(ScanoutFramebuffer {
                    buffer,
                    width: fb.width,
                    height: fb.height,
                    stride: fb.stride,
                    offset: 0,
                    format: fb.format,
                }),
                damage: vec![Rect {
                    x: 0,
                    y: 0,
                    width: fb.width,
                    height: fb.height,
                }],
            };
            device.check(&state)?;
            if test_only {
                Ok(None)
            } else {
                device.commit(&state).map(Some)
            }
        })
        .map_err(map_display_err)?
        .map_err(map_display_err)?;
        if let Some(completion) = completion {
            self.retain_old_scanout(completion, Some(fb.kind));
        }
        Ok(())
    }

    fn handle_create_dumb(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCreateDumb;
        let mut c: DrmModeCreateDumb = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if c.width == 0
            || c.height == 0
            || c.bpp == 0
            || c.bpp > 64
            || !c.bpp.is_multiple_of(8)
            || c.flags != 0
        {
            return Err(VfsError::InvalidInput);
        }
        if c.width > 16384 || c.height > 16384 {
            return Err(VfsError::InvalidInput);
        }
        if !dumb_supported() {
            return Err(VfsError::InvalidInput);
        }
        let bytes_per_pixel = c.bpp / 8;
        let pitch = c
            .width
            .checked_mul(bytes_per_pixel)
            .ok_or(VfsError::InvalidInput)?;
        let size = (pitch as u64)
            .checked_mul(c.height as u64)
            .ok_or(VfsError::InvalidInput)?;
        if size as usize > DUMB_BUFFER_MAX_SIZE {
            return Err(VfsError::NoMemory);
        }
        c.pitch = pitch;
        // The runtime allocates CPU-mappable pages in this GPU's DMA domain.
        // The mapping retains both CPU physical pages and the device address.
        let size_aligned = (size as usize).next_multiple_of(PAGE_SIZE_4K);
        c.size = size_aligned as u64;
        let mapping = ax_gpu::allocate_mappable_backing(
            NonZeroUsize::new(size_aligned).ok_or(VfsError::InvalidInput)?,
        )
        .map_err(map_gpu_err)?;
        let mapping = Arc::new(GpuMapping::Owned(mapping));
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);

        let dumb_format = match c.bpp {
            16 => Some(PixelFormat::Rgb565),
            24 => Some(PixelFormat::Rgb888),
            32 => Some(PixelFormat::Xrgb8888),
            _ => None,
        };
        let descriptor = match dumb_format {
            Some(format) => BufferDescriptor::Image2d {
                width: c.width,
                height: c.height,
                stride: pitch,
                format,
            },
            None => BufferDescriptor::Linear { size: size as usize },
        };
        let resource = if ax_gpu::capabilities().is_some_and(|caps| caps.supports_image_2d) {
            let device_handle = ax_gpu::with_gpu(|device| {
                device.create_buffer(descriptor, mapping.backing())
            })
            .map_err(map_gpu_err)?
            .map_err(|error| match error {
                GpuError::Unsupported => VfsError::InvalidInput,
                other => map_gpu_err(other),
            })?;
            let command_id = ax_gpu::with_gpu(|device| {
                device
                    .virgl()
                    .map(|virgl| virgl.command_resource_id(device_handle))
            })
            .and_then(|id| id.transpose());
            let res_handle = match command_id {
                Ok(Some(id)) => id,
                Ok(None) => self.next_res_handle.fetch_add(1, Ordering::Relaxed),
                Err(error) => {
                    let _ = ax_gpu::with_gpu_for_cleanup(|device| device.release_buffer(device_handle));
                    return Err(map_gpu_err(error));
                }
            };
            Some(Arc::new(GpuResource {
                owner: file.file_id,
                res_handle,
                device_handle,
                bo_handle: handle,
                width: c.width,
                height: c.height,
                stride: pitch,
                format: dumb_format,
                size: c.size,
                blob_mem: 0,
                blob_flags: 0,
                is_dumb_2d: true,
                last_fence: AtomicU64::new(0),
            }))
        } else {
            None
        };

        let buffer = DumbBuffer {
            owner: file.file_id,
            width: c.width,
            height: c.height,
            bpp: c.bpp,
            pitch: c.pitch,
            size: c.size,
            offset,
            mapping,
            mappable: true,
            resource: resource.clone(),
        };
        c.handle = handle;
        ptr.vm_write(current, c).map_err(|_| VfsError::BadAddress)?;
        self.dumbs.lock().insert(handle, buffer);
        if let Some(resource) = resource {
            self.gpu_resources
                .lock()
                .insert(resource.res_handle, resource);
        }
        Ok(0)
    }

    /// Shared cleanup for both DESTROY_DUMB and GEM_CLOSE. Linux funnels
    /// both through the same `drm_gem_handle_delete`, so mirroring that
    /// here keeps the two release paths consistent.
    ///
    /// The handle may be an exporter's creating handle (a `gpu_resources`
    /// entry), a same-device import alias (a `blob_aliases` entry), or a
    /// dumb-buffer handle (`dumbs`). Each of these holders keeps the host
    /// resource alive: the host-side `RESOURCE_UNREF` is sent by
    /// [`GpuResource::drop`] only after the last strong reference is released.
    fn destroy_handle(&self, file: &Card0File, handle: u32) -> VfsResult<()> {
        let removed_dumb = {
            let mut dumbs = self.dumbs.lock();
            dumbs
                .get(&handle)
                .is_some_and(|buffer| buffer.owner == file.file_id)
                .then(|| dumbs.remove(&handle))
                .flatten()
        };
        let removed_alias = {
            let mut aliases = self.blob_aliases.lock();
            aliases
                .get(&handle)
                .is_some_and(|alias| alias.owner == file.file_id)
                .then(|| aliases.remove(&handle))
                .flatten()
        };
        let removed_resources = {
            let mut resources = self.gpu_resources.lock();
            let ids = resources
                .iter()
                .filter_map(|(&id, resource)| {
                    (resource.owner == file.file_id && resource.bo_handle == handle).then_some(id)
                })
                .collect::<Vec<_>>();
            let mut removed = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(resource) = resources.remove(&id) {
                    removed.push(resource);
                }
            }
            removed
        };
        let found =
            removed_dumb.is_some() || removed_alias.is_some() || !removed_resources.is_empty();
        let mut detached_ids = BTreeSet::new();
        if let Some(resource) = removed_dumb
            .as_ref()
            .and_then(|buffer| buffer.resource.as_ref())
        {
            detached_ids.insert(resource.res_handle);
        }
        if let Some(alias) = &removed_alias {
            detached_ids.insert(alias.resource.res_handle);
        }
        for resource in &removed_resources {
            detached_ids.insert(resource.res_handle);
        }
        for resource_id in detached_ids {
            if !self.resource_is_referenced_by_file(file.file_id, resource_id) {
                file.detach_resource(resource_id);
            }
        }
        drop((removed_dumb, removed_alias, removed_resources));
        if found {
            Ok(())
        } else {
            Err(VfsError::InvalidInput)
        }
    }

    fn resource_is_referenced_by_file(&self, file_id: u64, resource_id: u32) -> bool {
        if self.dumbs.lock().values().any(|buffer| {
            buffer.owner == file_id
                && buffer
                    .resource
                    .as_ref()
                    .is_some_and(|resource| resource.res_handle == resource_id)
        }) {
            return true;
        }
        if self
            .blob_aliases
            .lock()
            .values()
            .any(|alias| alias.owner == file_id && alias.resource.res_handle == resource_id)
        {
            return true;
        }
        self.gpu_resources
            .lock()
            .values()
            .any(|resource| resource.owner == file_id && resource.res_handle == resource_id)
    }

    /// DESTROY_DUMB: dumb-buffer specific release path. Linux falls through
    /// to the same `drm_gem_handle_delete` as GEM_CLOSE; StarryOS shares
    /// one cleanup helper for both.
    fn handle_destroy_dumb(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDestroyDumb;
        let d: DrmModeDestroyDumb = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        self.destroy_handle(file, d.handle)?;
        Ok(0)
    }

    /// GEM_CLOSE: the only release channel Mesa uses to destroy virgl
    /// 3D/blob resources (including PRIME imports).
    fn handle_gem_close(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *const DrmGemClose;
        let c: DrmGemClose = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        self.destroy_handle(file, c.handle)?;
        Ok(0)
    }

    fn handle_map_dumb(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeMapDumb;
        let mut m: DrmModeMapDumb = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        let offset = self
            .dumbs
            .lock()
            .get(&m.handle)
            .filter(|buffer| buffer.owner == file.file_id)
            .map(|buffer| buffer.offset)
            .ok_or(VfsError::InvalidInput)?;
        m.offset = offset;
        ptr.vm_write(current, m).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }
}

fn active_scanout_output<D: DisplayController + ?Sized>(
    device: &D,
) -> Result<Option<OutputId>, DisplayError> {
    for id in (0..device.output_count()).map(OutputId::new) {
        if device
            .current_state(id)?
            .as_ref()
            .is_some_and(|state| state.framebuffer.is_some())
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn scanout_output_to_clear<D: DisplayController + ?Sized>(
    device: &D,
) -> Result<OutputId, DisplayError> {
    if let Some(id) = active_scanout_output(device)? {
        return Ok(id);
    }
    (device.output_count() > 0)
        .then_some(OutputId::new(0))
        .ok_or(DisplayError::NotAvailable)
}

fn select_output_for_present<D: DisplayController + ?Sized>(
    device: &D,
) -> Result<OutputInfo, DisplayError> {
    if let Some(id) = active_scanout_output(device)? {
        let output = device.output(id)?;
        return output
            .connected
            .then_some(output)
            .ok_or(DisplayError::NotAvailable);
    }
    (0..device.output_count())
        .map(OutputId::new)
        .find_map(|id| device.output(id).ok().filter(|info| info.connected))
        .ok_or(DisplayError::NotAvailable)
}

fn handle_version(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmVersion;
    let mut v: DrmVersion = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    let identity = ax_gpu::identity().ok_or(VfsError::Io)?;
    v.version_major = identity.driver_version.major as i32;
    v.version_minor = identity.driver_version.minor as i32;
    v.version_patchlevel = identity.driver_version.patch as i32;
    v.name_len = write_user_string(current, v.name, v.name_len, &identity.driver_name)?;
    v.date_len = write_user_string(current, v.date, v.date_len, DRM_DRIVER_DATE)?;
    v.desc_len = write_user_string(current, v.desc, v.desc_len, &identity.description)?;
    ptr.vm_write(current, v).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_unique(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmUnique;
    let mut u: DrmUnique = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    let identity = ax_gpu::identity().ok_or(VfsError::Io)?;
    let unique: String = identity.device_name;
    u.unique_len = write_user_string(current, u.unique, u.unique_len, &unique)?;
    ptr.vm_write(current, u).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_set_version(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmSetVersion;
    let mut sv: DrmSetVersion = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    if sv.drm_di_major < 0 {
        sv.drm_di_major = 1;
    }
    if sv.drm_di_minor < 0 {
        sv.drm_di_minor = 4;
    }
    let identity = ax_gpu::identity().ok_or(VfsError::Io)?;
    sv.drm_dd_major = identity.driver_version.major as i32;
    sv.drm_dd_minor = identity.driver_version.minor as i32;
    ptr.vm_write(current, sv)
        .map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_cap(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmGetCap;
    let mut cap: DrmGetCap = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    // Unknown caps return value=0 rather than EINVAL.
    cap.value = match cap.capability {
        DRM_CAP_DUMB_BUFFER => u64::from(dumb_supported()),
        DRM_CAP_TIMESTAMP_MONOTONIC => 1,
        DRM_CAP_CRTC_IN_VBLANK_EVENT => u64::from(kms_available()),
        DRM_CAP_ADDFB2_MODIFIERS => u64::from(kms_available()),
        DRM_CAP_PRIME => DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT,
        _ => 0,
    };
    ptr.vm_write(current, cap)
        .map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_set_client_cap(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *const DrmSetClientCap;
    let _scc: DrmSetClientCap = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    // Linux rejects all settable client caps when DRIVER_MODESET is absent.
    if !kms_available() {
        return Err(VfsError::OperationNotSupported);
    }
    Ok(0)
}

fn handle_get_magic(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmAuth;
    let magic = DrmAuth { magic: 1 };
    ptr.vm_write(current, magic)
        .map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_auth_magic(_current: &crate::task::UserTaskRef, _arg: usize) -> VfsResult<usize> {
    Ok(0)
}

impl Card0 {
    /// Export a GEM handle as a dma-buf file descriptor via PRIME.
    ///
    /// A handle backed by a 3D resource (blob or classic virgl) exports the
    /// *host* resource as a [`HostResourceDmaBuf`] — mirroring Linux
    /// `virtgpu_gem_prime_export`, which exports the GEM object itself.
    /// A handle backed by guest RAM (dumb buffer) exports a [`DmaBufGem`]
    /// wrapping its physical pages.
    fn handle_prime_handle_to_fd(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmPrimeHandle;
        let mut req: DrmPrimeHandle = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        let dma_buf: Arc<dyn FileLike> = {
            // 3D resource (blob or classic) → export the host resource.
            let imported = self
                .blob_aliases
                .lock()
                .get(&req.handle)
                .filter(|alias| alias.owner == file.file_id)
                .map(|alias| alias.resource.clone());
            let host_res = imported.or_else(|| {
                self.gpu_resources
                    .lock()
                    .values()
                    .find(|resource| {
                        resource.owner == file.file_id
                            && resource.bo_handle == req.handle
                            && !resource.is_dumb_2d
                            && resource.blob_mem != VIRTGPU_BLOB_MEM_GUEST
                    })
                    .cloned()
            });

            if let Some(resource) = host_res {
                Arc::new(HostResourceDmaBuf { resource })
            } else {
                // 2D dumb buffer → export guest RAM (original path).
                let dumbs = self.dumbs.lock();
                let buf = dumbs
                    .get(&req.handle)
                    .filter(|buffer| buffer.owner == file.file_id)
                    .ok_or(VfsError::InvalidInput)?;

                Arc::new(DmaBufGem {
                    mapping: buf.mapping.clone(),
                    resource: buf.resource.clone(),
                    size: buf.size,
                })
            }
        };

        let cloexec = req.flags & O_CLOEXEC != 0;
        let fd = add_file_like(dma_buf.clone(), cloexec).map_err(|_| VfsError::NoMemory)?;
        req.fd = fd;

        if ptr.vm_write(current, req).is_err() {
            close_exported_fd(fd, &dma_buf);
            return Err(VfsError::BadAddress);
        }
        Ok(0)
    }

    /// Import a dma-buf fd back into the card's GEM handle namespace.
    ///
    /// Resolves `req.fd` to a [`DmaBufGem`] object, then registers it in
    /// our dumbs table with a fresh handle so the calling process can use
    /// it with other DRM ioctls (e.g. `ADDFB2`).
    ///
    /// # Why this cannot be an identity mapping
    ///
    /// The prior implementation (`req.handle = req.fd as u32`) treated the
    /// fd number directly as a GEM handle. This is incorrect because:
    ///
    /// - fd numbers and GEM handles live in **separate namespaces**.  A
    ///   process may have fd 5 pointing to a socket, not a dma-buf, and
    ///   fd_to_handle would blindly mint handle=5 in the dumbs table,
    ///   creating a dangling entry that refers to un-related memory.
    /// - No type check: any fd (pipe, socket, regular file) was accepted
    ///   without verifying it is actually a dma-buf backed by our card.
    /// - No reference counting: the imported "handle" had no `Arc` bump on
    ///   the backing pages.  A concurrent `DESTROY_DUMB` on the source
    ///   handle (or `close` on the fd) could free the pages while the
    ///   importer still holds the fake handle.
    ///
    /// The current implementation uses `downcast_ref::<DmaBufGem>` to
    /// reject non-dma-buf fds and `Arc::clone` to participate in the GEM
    /// refcount contract, matching Linux's behaviour.
    fn handle_prime_fd_to_handle(
        &self,
        card_file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmPrimeHandle;
        let mut req: DrmPrimeHandle = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        let file = crate::file::get_file_like(req.fd).map_err(|_| VfsError::BadFileDescriptor)?;

        // Imported *host* 3D resource (blob dma-buf): same-device import is
        // the same host resource — record an alias so RESOURCE_INFO on the
        // new handle resolves back to the exporter's res_handle + blob_mem.
        if let Some(dma) = file.as_any().downcast_ref::<HostResourceDmaBuf>() {
            let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
            // Linux `virtio_gpu_gem_object_open()`: a legacy device creates
            // the context here, because the first ioctl on this fd may be
            // `PRIME_FD_TO_HANDLE` rather than anything else that would have
            // triggered the lazy create. With `CONTEXT_INIT` negotiated Linux
            // deliberately does *not* create one (userspace owns that choice),
            // and an attachment is only made when a context exists.
            if ax_gpu::capabilities()
                .is_some_and(|caps| caps.supports_3d && !caps.supports_context_init)
            {
                let _ctx_id = card_file.ensure_lazy_context()?;
            }
            // Import succeeds even with no context yet: the resource is
            // registered under a fresh handle and only attached to the host
            // context when this fd already has one. A missing context is a
            // successful no-op, while an attach failure aborts the import.
            let attachment = card_file.attach_resource_if_ready(&dma.resource)?;
            req.handle = handle;
            if ptr.vm_write(current, req).is_err() {
                if attachment == AttachmentState::NewlyAttached {
                    card_file.detach_resource(dma.resource.res_handle);
                }
                return Err(VfsError::BadAddress);
            }
            self.blob_aliases.lock().insert(
                handle,
                ImportedBlob {
                    owner: card_file.file_id,
                    resource: dma.resource.clone(),
                },
            );
            return Ok(0);
        }

        // Guest-RAM dma-buf or a coherent dma-heap allocation. The latter
        // carries a direct DMA address and is imported only when this GPU
        // uses the same direct domain; other domains need an explicit map.
        let (mapping, resource, size) = if let Some(dma_buf) =
            file.as_any().downcast_ref::<DmaBufGem>()
        {
            (
                dma_buf.mapping.clone(),
                dma_buf.resource.clone(),
                dma_buf.size,
            )
        } else if let Ok(heap_file) = file
            .clone()
            .downcast_arc::<crate::file::dmabuf::DmaBufFile>()
        {
            if ax_gpu::capabilities().is_none_or(|caps| caps.dma_domain != DmaDomainId::Direct) {
                return Err(VfsError::Unsupported);
            }
            let size = heap_file.size() as u64;
            (
                Arc::new(GpuMapping::Heap(Arc::new(HeapBacking::new(heap_file)))),
                None,
                size,
            )
        } else {
            return Err(VfsError::InvalidInput);
        };

        let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        let buffer =
            // NOTE: width/height/bpp/pitch are zero because the
            // PRIME_FD_TO_HANDLE ioctl does not carry geometry
            // information — the kernel only receives {handle, flags, fd}
            // from userspace and has no way to learn the original
            // CREATE_DUMB parameters.  These fields are metadata-only
            // (see the DumbBuffer doc comment) and no ioctl handler
            // reads them, so zero is safe.  A future code path that
            // inspects .width / .height / .bpp / .pitch on an
            // arbitrary buffer must tolerate zero for imports.
            DumbBuffer {
                owner: card_file.file_id,
                width: 0,
                height: 0,
                bpp: 0,
                pitch: 0,
                size,
                offset,
                mapping,
                mappable: true,
                resource,
            };
        req.handle = handle;

        ptr.vm_write(current, req)
            .map_err(|_| VfsError::BadAddress)?;
        self.dumbs.lock().insert(handle, buffer);
        Ok(0)
    }
}

fn close_exported_fd(fd: i32, expected: &Arc<dyn FileLike>) {
    let removed = {
        let table = current_fd_table();
        let mut table = table.write();
        let matches = table
            .get(fd as usize)
            .is_some_and(|descriptor| Arc::ptr_eq(&descriptor.inner, expected));
        matches.then(|| table.remove(fd as usize)).flatten()
    };
    if let Some(descriptor) = removed {
        release_locks_on_close(descriptor);
    }
}

fn handle_get_resources(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeCardRes;
    let mut r: DrmModeCardRes = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

    let (w, h) = display_resolution();
    r.min_width = w;
    r.max_width = w;
    r.min_height = h;
    r.max_height = h;

    r.count_fbs = 0;
    r.count_crtcs = report_user_array(current, r.crtc_id_ptr, r.count_crtcs, &[CRTC_ID])?;
    r.count_encoders =
        report_user_array(current, r.encoder_id_ptr, r.count_encoders, &[ENCODER_ID])?;
    r.count_connectors = report_user_array(
        current,
        r.connector_id_ptr,
        r.count_connectors,
        &[CONNECTOR_ID],
    )?;

    ptr.vm_write(current, r).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

impl Card0 {
    fn handle_get_crtc(&self, current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtc;
        let mut c: DrmModeCrtc = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if c.crtc_id != CRTC_ID {
            return Err(VfsError::InvalidInput);
        }
        let state = self.state.lock().clone();
        c.gamma_size = 0;
        c.x = (state.plane_src_x >> 16) as u32;
        c.y = (state.plane_src_y >> 16) as u32;
        c.fb_id = state.plane_fb_id;
        c.mode_valid = u32::from(state.mode.is_some());
        c.mode = state
            .mode
            .as_ref()
            .map_or_else(Default::default, |mode| mode.info);
        let connectors = [CONNECTOR_ID];
        c.count_connectors = report_user_array(
            current,
            c.set_connectors_ptr,
            c.count_connectors,
            if state.conn_crtc_id != 0 {
                &connectors
            } else {
                &[]
            },
        )?;
        ptr.vm_write(current, c).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_set_crtc(&self, current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtc;
        let c: DrmModeCrtc = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if c.crtc_id != CRTC_ID {
            return Err(VfsError::InvalidInput);
        }
        let _operation = self.modeset_operation.lock();

        // Linux uses mode_valid to request disable, but still rejects a
        // disable request that names connectors.
        if c.mode_valid == 0 {
            if c.count_connectors != 0 {
                return Err(VfsError::InvalidInput);
            }
            self.clear_scanout(false).map_err(map_display_err)?;
            let mut state = self.state.lock();
            *state = ModesetState::default();
            let completed = self.set_vblank_active(&state, monotonic_time_nanos());
            drop(state);
            drop(_operation);
            self.notify_vblank_change(completed);
            return Ok(0);
        }

        // A non-disable SETCRTC must list at least one connector and
        // every listed id must exist.
        if c.count_connectors == 0 || c.set_connectors_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }
        // Bound the user count so a bogus value can't try to allocate
        // unbounded kernel memory.
        if c.count_connectors > 16 {
            return Err(VfsError::InvalidInput);
        }
        let connectors: Vec<u32> = vm_load(
            current,
            c.set_connectors_ptr as *const u32,
            c.count_connectors as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
        for &id in &connectors {
            if id != CONNECTOR_ID {
                return Err(VfsError::InvalidInput);
            }
        }

        if c.fb_id == 0 || !self.fbs.lock().contains_key(&c.fb_id) {
            return Err(VfsError::InvalidInput);
        }
        // Legacy SETCRTC uses the same mode/plane state as an atomic commit.
        let proposed = ModesetState {
            crtc_active: 1,
            mode: Some(ModeBlob {
                id: self.next_blob_id.fetch_add(1, Ordering::Relaxed),
                info: c.mode,
                bytes: Arc::new(bytes_of(&c.mode).to_vec()),
            }),
            conn_crtc_id: CRTC_ID,
            plane_crtc_id: CRTC_ID,
            plane_fb_id: c.fb_id,
            plane_src_x: u64::from(c.x) << 16,
            plane_src_y: u64::from(c.y) << 16,
            plane_src_w: u64::from(c.mode.hdisplay) << 16,
            plane_src_h: u64::from(c.mode.vdisplay) << 16,
            plane_crtc_w: u64::from(c.mode.hdisplay),
            plane_crtc_h: u64::from(c.mode.vdisplay),
            ..ModesetState::default()
        };
        self.present_fb(c.fb_id, &proposed, false)?;
        let mut state = self.state.lock();
        *state = proposed;
        let completed = self.set_vblank_active(&state, monotonic_time_nanos());
        drop(state);
        drop(_operation);
        self.notify_vblank_change(completed);
        Ok(0)
    }
}

fn handle_get_encoder(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetEncoder;
    let mut e: DrmModeGetEncoder = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    if e.encoder_id != ENCODER_ID {
        return Err(VfsError::InvalidInput);
    }
    e.encoder_type = match display_output_info()?.kind {
        OutputKind::Virtual => DRM_MODE_ENCODER_VIRTUAL,
        OutputKind::Internal => DRM_MODE_ENCODER_LVDS,
        OutputKind::Hdmi | OutputKind::DisplayPort => DRM_MODE_ENCODER_TMDS,
        OutputKind::Vga => DRM_MODE_ENCODER_DAC,
        OutputKind::Unknown => DRM_MODE_ENCODER_NONE,
    };
    e.crtc_id = CRTC_ID;
    e.possible_crtcs = 1;
    e.possible_clones = 0;
    ptr.vm_write(current, e).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_connector(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetConnector;
    let mut c: DrmModeGetConnector = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    if c.connector_id != CONNECTOR_ID {
        return Err(VfsError::InvalidInput);
    }
    let output = display_output_info()?;
    c.encoder_id = if output.connected { ENCODER_ID } else { 0 };
    c.connector_type = match output.kind {
        OutputKind::Virtual => DRM_MODE_CONNECTOR_VIRTUAL,
        OutputKind::Internal => DRM_MODE_CONNECTOR_EDP,
        OutputKind::Hdmi => DRM_MODE_CONNECTOR_HDMIA,
        OutputKind::DisplayPort => DRM_MODE_CONNECTOR_DISPLAYPORT,
        OutputKind::Vga => DRM_MODE_CONNECTOR_VGA,
        OutputKind::Unknown => DRM_MODE_CONNECTOR_UNKNOWN,
    };
    c.connector_type_id = 1;
    c.connection = if output.connected {
        DRM_MODE_CONNECTED
    } else {
        DRM_MODE_DISCONNECTED
    };
    let (mm_width, mm_height) = output.physical_size_mm.unwrap_or((0, 0));
    c.mm_width = mm_width;
    c.mm_height = mm_height;
    c.subpixel = 0;

    c.count_encoders = report_user_array(current, c.encoders_ptr, c.count_encoders, &[ENCODER_ID])?;

    let modes = output
        .modes
        .iter()
        .copied()
        .map(current_mode)
        .collect::<Vec<_>>();
    c.count_modes = report_user_array(current, c.modes_ptr, c.count_modes, &modes)?;
    c.count_props = 0;

    ptr.vm_write(current, c).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

impl Card0 {
    fn handle_addfb2(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeFbCmd2;
        let mut f: DrmModeFbCmd2 = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        let handle = f.handles[0];
        // Resolve the backing kind + capacity under the resource/dumb
        // locks so a concurrent DESTROY_DUMB can't race the fb's
        // initial Arc bump. A handle may be:
        //   1) a host 3D resource (virgl scanout — Weston/glamor's GBM
        //      buffers are host textures from RESOURCE_CREATE_3D, which
        //      also register a shadow `dumbs` entry). Check this first so
        //      present binds it with SET_SCANOUT instead of copying empty
        //      guest RAM (the shadow pages are never attached to the host).
        //   2) a plain guest-RAM dumb buffer (2D path).
        let (mut kind, size) = {
            let imported = self
                .blob_aliases
                .lock()
                .get(&handle)
                .filter(|alias| alias.owner == file.file_id)
                .map(|alias| alias.resource.clone());
            let owned = self
                .gpu_resources
                .lock()
                .values()
                .find(|resource| resource.owner == file.file_id && resource.bo_handle == handle)
                .cloned();
            if let Some(resource) = imported.or(owned) {
                let size = resource.size;
                (FbBacking::Gpu3d { resource }, size)
            } else {
                let dumbs = self.dumbs.lock();
                let Some(buffer) = dumbs
                    .get(&handle)
                    .filter(|buffer| buffer.owner == file.file_id)
                else {
                    return Err(VfsError::InvalidInput);
                };
                if let Some(resource) = &buffer.resource {
                    (
                        FbBacking::Gpu3d {
                            resource: resource.clone(),
                        },
                        buffer.size,
                    )
                } else {
                    (
                        FbBacking::Dumb {
                            mapping: buffer.mapping.clone(),
                        },
                        buffer.size,
                    )
                }
            }
        };
        // Use the plane stride from the ADDFB2 request (f.pitches[0])
        // rather than the dumb buffer's pitch.  PRIME/import buffers may
        // have a dumb pitch of 0 even when userspace supplies a valid
        // stride in the ADDFB2 call.
        let fb_stride = f.pitches[0];
        let fb_width = f.width;
        let fb_height = f.height;
        let fb_pixel_format = f.pixel_format;

        let pixel_format = fourcc_to_format(fb_pixel_format).ok_or(VfsError::InvalidInput)?;
        if !display_output_info()?.formats.contains(&pixel_format) {
            return Err(VfsError::InvalidInput);
        }
        if let FbBacking::Gpu3d { resource } = &kind
            && resource.is_dumb_2d
            && (resource.width != fb_width
                || resource.height != fb_height
                || resource.stride != fb_stride
                || resource.format != Some(pixel_format))
        {
            return Err(VfsError::InvalidInput);
        }
        let visible_bytes = fb_width
            .checked_mul(pixel_format.bytes_per_pixel() as u32)
            .ok_or(VfsError::InvalidInput)?;
        if fb_stride < visible_bytes {
            warn!(
                "ADDFB2: stride {} < visible bytes {} ({}px)",
                fb_stride, visible_bytes, fb_width
            );
            return Err(VfsError::InvalidInput);
        }
        let fb_total = fb_stride as u64 * fb_height as u64;
        // Guest-RAM buffers must actually hold the full frame. Host 3D
        // resources hold their storage on the GPU — the shadow `size` can
        // even be the kernel's PAGE_SIZE default when Mesa passes 0 — so
        // skip the capacity check there.
        if matches!(&kind, FbBacking::Dumb { .. }) && size < fb_total {
            warn!(
                "ADDFB2: buffer size {} < fb_total {} ({}stride × {}height)",
                size, fb_total, fb_stride, fb_height
            );
            return Err(VfsError::InvalidInput);
        }
        // PRIME import does not carry image geometry. Bind its existing GEM
        // backing to a device image only after ADDFB2 supplies the layout.
        if let FbBacking::Dumb { mapping } = &kind
            && ax_gpu::capabilities().is_some_and(|caps| caps.supports_image_2d)
        {
            let device_handle = ax_gpu::with_gpu(|device| {
                device.create_buffer(
                    BufferDescriptor::Image2d {
                        width: fb_width,
                        height: fb_height,
                        stride: fb_stride,
                        format: pixel_format,
                    },
                    mapping.backing(),
                )
            })
            .map_err(map_gpu_err)?
            .map_err(map_gpu_err)?;
            kind = FbBacking::Gpu3d {
                resource: Arc::new(GpuResource {
                    owner: file.file_id,
                    res_handle: self.next_res_handle.fetch_add(1, Ordering::Relaxed),
                    device_handle,
                    bo_handle: handle,
                    width: fb_width,
                    height: fb_height,
                    stride: fb_stride,
                    format: Some(pixel_format),
                    size,
                    blob_mem: 0,
                    blob_flags: 0,
                    is_dumb_2d: true,
                    last_fence: AtomicU64::new(0),
                }),
            };
        }
        if f.flags & DRM_MODE_FB_MODIFIERS != 0 {
            for i in 0..4 {
                if f.handles[i] == 0 {
                    continue;
                }
                let m = f.modifier[i];
                if m != DRM_FORMAT_MOD_LINEAR && m != DRM_FORMAT_MOD_INVALID {
                    return Err(VfsError::InvalidInput);
                }
            }
        }
        let fb_id = self.next_fb_id.fetch_add(1, Ordering::Relaxed);
        let framebuffer = Framebuffer {
            owner: file.file_id,
            stride: fb_stride,
            width: fb_width,
            height: fb_height,
            format: pixel_format,
            kind,
        };
        f.fb_id = fb_id;
        ptr.vm_write(current, f).map_err(|_| VfsError::BadAddress)?;
        self.fbs.lock().insert(fb_id, framebuffer);
        Ok(0)
    }

    fn handle_rmfb(&self, file: &Card0File, current: &UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const u32;
        let fb_id: u32 = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        let _operation = self.modeset_operation.lock();
        let owned = self
            .fbs
            .lock()
            .get(&fb_id)
            .is_some_and(|framebuffer| framebuffer.owner == file.file_id);
        if !owned {
            return Err(VfsError::InvalidInput);
        }
        let active = self.state.lock().plane_fb_id == fb_id;
        let completed = if active {
            self.clear_scanout(false).map_err(map_display_err)?;
            let mut state = self.state.lock();
            *state = ModesetState::default();
            let completed = self.set_vblank_active(&state, monotonic_time_nanos());
            drop(state);
            completed
        } else {
            None
        };
        let removed = self.fbs.lock().remove(&fb_id);
        drop(_operation);
        self.notify_vblank_change(completed);
        drop(removed);
        Ok(0)
    }
}

// ======== M4b: planes, properties, page flip, vblank ========

fn handle_get_plane_resources(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetPlaneRes;
    let mut r: DrmModeGetPlaneRes = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    let planes: &[u32] = &[PLANE_ID];
    r.count_planes = report_user_array(current, r.plane_id_ptr, r.count_planes, planes)?;
    ptr.vm_write(current, r).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

impl Card0 {
    fn handle_get_plane(&self, current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeGetPlane;
        let mut p: DrmModeGetPlane = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if p.plane_id != PLANE_ID {
            return Err(VfsError::InvalidInput);
        }
        let state = self.state.lock().clone();
        p.crtc_id = state.plane_crtc_id;
        p.fb_id = state.plane_fb_id;
        p.possible_crtcs = 1;
        p.gamma_size = 0;
        let formats = display_plane_formats()?;
        p.count_format_types = report_user_array(
            current,
            p.format_type_ptr,
            p.count_format_types,
            &formats,
        )?;
        ptr.vm_write(current, p).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_obj_get_properties(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeObjGetProperties;
        let mut q: DrmModeObjGetProperties =
            ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        let state = self.state.lock().clone();
        let (prop_ids, prop_vals): (&[u32], Vec<u64>) = match (q.obj_type, q.obj_id) {
            (DRM_MODE_OBJECT_PLANE, PLANE_ID) => {
                let blob_id = self.ensure_in_formats_blob()? as u64;
                (PLANE_PROPS, plane_prop_values(&state, blob_id))
            }
            (DRM_MODE_OBJECT_CRTC, CRTC_ID) => (CRTC_PROPS, crtc_prop_values(&state)),
            (DRM_MODE_OBJECT_CONNECTOR, CONNECTOR_ID) => (CONN_PROPS, conn_prop_values(&state)),
            (DRM_MODE_OBJECT_FB, fb_id) => {
                // Linux rejects objects without a property container after
                // lookup, distinguishing them from nonexistent object IDs.
                return Err(if self.fbs.lock().contains_key(&fb_id) {
                    VfsError::InvalidInput
                } else {
                    VfsError::NotFound
                });
            }
            _ => return Err(VfsError::NotFound),
        };
        report_user_array(current, q.props_ptr, q.count_props, prop_ids)?;
        report_user_array(current, q.prop_values_ptr, q.count_props, &prop_vals)?;
        q.count_props = prop_ids.len() as u32;
        ptr.vm_write(current, q).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }
}

fn plane_prop_values(s: &ModesetState, in_formats: u64) -> Vec<u64> {
    vec![
        DRM_PLANE_TYPE_PRIMARY,
        s.plane_fb_id as u64,
        s.plane_crtc_id as u64,
        s.plane_src_x,
        s.plane_src_y,
        s.plane_src_w,
        s.plane_src_h,
        s.plane_crtc_x as u64,
        s.plane_crtc_y as u64,
        s.plane_crtc_w,
        s.plane_crtc_h,
        in_formats,
        // No fence is ever pending outside a commit request.
        u64::MAX,
    ]
}

/// Construct the `IN_FORMATS` blob for the bound output's linear formats.
fn build_in_formats_blob(formats: &[u32]) -> Vec<u8> {
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::NoUninit)]
    struct Header {
        version: u32,
        flags: u32,
        count_formats: u32,
        formats_offset: u32,
        count_modifiers: u32,
        modifiers_offset: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::NoUninit)]
    struct ModifierEntry {
        formats: u64,
        offset: u32,
        _pad: u32,
        modifier: u64,
    }
    let n_formats = formats.len() as u32;
    let formats_off = size_of::<Header>() as u32;
    let modifiers_off = formats_off + n_formats * 4;
    let hdr = Header {
        version: 1,
        flags: 0,
        count_formats: n_formats,
        formats_offset: formats_off,
        count_modifiers: 1,
        modifiers_offset: modifiers_off,
    };
    let format_mask = (1u64 << n_formats) - 1;
    let me = ModifierEntry {
        formats: format_mask,
        offset: 0,
        _pad: 0,
        modifier: DRM_FORMAT_MOD_LINEAR,
    };
    let mut buf = Vec::with_capacity(
        size_of::<Header>() + (n_formats as usize) * 4 + size_of::<ModifierEntry>(),
    );
    buf.extend_from_slice(bytes_of(&hdr));
    for fmt in formats {
        buf.extend_from_slice(&fmt.to_le_bytes());
    }
    buf.extend_from_slice(bytes_of(&me));
    buf
}

fn crtc_prop_values(s: &ModesetState) -> Vec<u64> {
    vec![
        s.crtc_active,
        s.mode.as_ref().map_or(0, |mode| u64::from(mode.id)),
    ]
}

fn conn_prop_values(s: &ModesetState) -> Vec<u64> {
    vec![s.conn_crtc_id as u64]
}

/// `GETPROPERTY` — describe a single property by id.
fn handle_get_property(current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetProperty;
    let mut g: DrmModeGetProperty = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
    let meta = property_meta(g.prop_id).ok_or(VfsError::NotFound)?;

    g.flags = meta.flags;
    g.name = [0; DRM_PROP_NAME_LEN];
    let nb = meta.name.as_bytes();
    let n = nb.len().min(DRM_PROP_NAME_LEN - 1);
    g.name[..n].copy_from_slice(&nb[..n]);

    match meta.kind {
        PropKind::Enum(enums) => {
            g.count_values = enums.len() as u32;
            g.count_enum_blobs =
                report_user_array(current, g.enum_blob_ptr, g.count_enum_blobs, enums)?;
        }
        PropKind::RangeU64 { min, max } => {
            let limits = [min, max];
            g.count_values = report_user_array(current, g.values_ptr, g.count_values, &limits)?;
            g.count_enum_blobs = 0;
        }
        PropKind::Object(object_type) => {
            let values = [object_type as u64];
            g.count_values = report_user_array(current, g.values_ptr, g.count_values, &values)?;
            g.count_enum_blobs = 0;
        }
        PropKind::Blob => {
            g.count_values = 0;
            g.count_enum_blobs = 0;
        }
    }
    ptr.vm_write(current, g).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

struct PropMeta {
    name: &'static str,
    flags: u32,
    kind: PropKind,
}

enum PropKind {
    Enum(&'static [DrmModePropertyEnum]),
    RangeU64 { min: u64, max: u64 },
    Object(u32),
    Blob,
}

const fn enum_entry(value: u64, name: &[u8]) -> DrmModePropertyEnum {
    let mut e = DrmModePropertyEnum {
        value,
        name: [0; DRM_PROP_NAME_LEN],
    };
    let n = if name.len() < DRM_PROP_NAME_LEN - 1 {
        name.len()
    } else {
        DRM_PROP_NAME_LEN - 1
    };
    let mut i = 0;
    while i < n {
        e.name[i] = name[i];
        i += 1;
    }
    e
}

const PLANE_TYPE_ENUMS: &[DrmModePropertyEnum] = &[
    enum_entry(0, b"Overlay"),
    enum_entry(1, b"Primary"),
    enum_entry(2, b"Cursor"),
];
fn property_meta(id: u32) -> Option<PropMeta> {
    let atomic = DRM_MODE_PROP_ATOMIC;
    let meta = match id {
        PROP_PLANE_TYPE => PropMeta {
            name: "type",
            flags: DRM_MODE_PROP_ENUM | DRM_MODE_PROP_IMMUTABLE,
            kind: PropKind::Enum(PLANE_TYPE_ENUMS),
        },
        PROP_PLANE_FB_ID => PropMeta {
            name: "FB_ID",
            flags: DRM_MODE_PROP_OBJECT | atomic,
            kind: PropKind::Object(DRM_MODE_OBJECT_FB),
        },
        PROP_PLANE_CRTC_ID => PropMeta {
            name: "CRTC_ID",
            flags: DRM_MODE_PROP_OBJECT | atomic,
            kind: PropKind::Object(DRM_MODE_OBJECT_CRTC),
        },
        PROP_PLANE_SRC_X => range_u32("SRC_X", atomic),
        PROP_PLANE_SRC_Y => range_u32("SRC_Y", atomic),
        PROP_PLANE_SRC_W => range_u32("SRC_W", atomic),
        PROP_PLANE_SRC_H => range_u32("SRC_H", atomic),
        PROP_PLANE_CRTC_X => range_u32("CRTC_X", atomic),
        PROP_PLANE_CRTC_Y => range_u32("CRTC_Y", atomic),
        PROP_PLANE_CRTC_W => range_u32("CRTC_W", atomic),
        PROP_PLANE_CRTC_H => range_u32("CRTC_H", atomic),
        PROP_PLANE_IN_FORMATS => PropMeta {
            name: "IN_FORMATS",
            flags: DRM_MODE_PROP_BLOB | DRM_MODE_PROP_IMMUTABLE,
            kind: PropKind::Blob,
        },
        PROP_PLANE_IN_FENCE_FD => PropMeta {
            name: "IN_FENCE_FD",
            // Linux declares this as a signed range [-1, INT_MAX] where -1
            // (reported as u64::MAX) means "no fence".
            flags: DRM_MODE_PROP_SIGNED_RANGE | atomic,
            kind: PropKind::RangeU64 {
                min: u64::MAX,
                max: i32::MAX as u64,
            },
        },
        PROP_CRTC_ACTIVE => PropMeta {
            name: "ACTIVE",
            // weston's drm-backend specifically rejects ACTIVE if it
            // isn't declared as a u32 range [0,1] — see submission I.
            flags: DRM_MODE_PROP_RANGE | atomic,
            kind: PropKind::RangeU64 { min: 0, max: 1 },
        },
        PROP_CRTC_MODE_ID => PropMeta {
            name: "MODE_ID",
            flags: DRM_MODE_PROP_BLOB | atomic,
            kind: PropKind::Blob,
        },
        PROP_CONN_CRTC_ID => PropMeta {
            name: "CRTC_ID",
            flags: DRM_MODE_PROP_OBJECT | atomic,
            kind: PropKind::Object(DRM_MODE_OBJECT_CRTC),
        },
        _ => return None,
    };
    Some(meta)
}

fn range_u32(name: &'static str, atomic: u32) -> PropMeta {
    PropMeta {
        name,
        flags: DRM_MODE_PROP_RANGE | atomic,
        kind: PropKind::RangeU64 {
            min: 0,
            max: u32::MAX as u64,
        },
    }
}

impl Card0 {
    fn handle_dirty_fb(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDirtyFB;
        let dirty: DrmModeDirtyFB = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if !self
            .fbs
            .lock()
            .get(&dirty.fb_id)
            .is_some_and(|framebuffer| framebuffer.owner == file.file_id)
        {
            return Err(VfsError::InvalidInput);
        }
        let _operation = self.modeset_operation.lock();
        let state = self.state.lock().clone();
        if state.crtc_active != 0 && state.plane_fb_id == dirty.fb_id {
            self.present_fb(state.plane_fb_id, &state, false)?;
        }
        Ok(0)
    }

    fn handle_page_flip(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeCrtcPageFlip;
        let f: DrmModeCrtcPageFlip = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if f.flags & !DRM_MODE_PAGE_FLIP_EVENT != 0
            || f.reserved != 0
            || f.crtc_id != CRTC_ID
            || !self
                .fbs
                .lock()
                .get(&f.fb_id)
                .is_some_and(|framebuffer| framebuffer.owner == file.file_id)
        {
            return Err(VfsError::InvalidInput);
        }
        let _operation = self.modeset_operation.lock();
        let mut state = self.state.lock().clone();
        if state.plane_fb_id == 0 {
            return Err(VfsError::ResourceBusy);
        }
        if state.crtc_active == 0 || !self.fbs.lock().contains_key(&f.fb_id) {
            return Err(VfsError::InvalidInput);
        }
        let reservation = if f.flags & DRM_MODE_PAGE_FLIP_EVENT != 0 {
            Some(file.reserve_event()?)
        } else {
            None
        };
        state.plane_fb_id = f.fb_id;
        self.present_fb(f.fb_id, &state, false)?;
        *self.state.lock() = state;
        let flip_edge = reservation
            .as_ref()
            .map(|_| self.vblank.snapshot_at(monotonic_time_nanos()));
        if let (Some(reservation), Some((sequence, edge_ns))) = (reservation, flip_edge) {
            self.queue_flip_event(reservation, f.user_data, sequence, edge_ns);
        }
        Ok(0)
    }

    /// Report whether the committed CRTC remains enabled, independent of its plane.
    fn crtc_active(&self) -> bool {
        let state = self.state.lock();
        state.crtc_active != 0
    }

    /// Flip-completion and its timestamp refer to the same vblank edge.
    fn queue_flip_event(
        &self,
        reservation: EventReservation<'_>,
        user_data: u64,
        sequence: u64,
        edge_ns: u64,
    ) {
        let ev = DrmEventVblank {
            base: DrmEvent {
                event_type: DRM_EVENT_FLIP_COMPLETE,
                length: core::mem::size_of::<DrmEventVblank>() as u32,
            },
            user_data,
            tv_sec: (edge_ns / 1_000_000_000) as u32,
            tv_usec: ((edge_ns % 1_000_000_000) / 1_000) as u32,
            sequence: sequence as u32,
            crtc_id: CRTC_ID,
        };
        reservation.enqueue(&ev);
    }

    /// `CRTC_GET_SEQUENCE` — report the synthesized counter's most recent
    /// edge, mirroring `drm_crtc_get_sequence_ioctl()` (Linux 4.19
    /// `drm_vblank.c`): `active` reflects the CRTC's scanout state,
    /// `sequence` the most recent vblank, `sequence_ns` that edge's
    /// `CLOCK_MONOTONIC` timestamp. An inactive CRTC fails with `EINVAL`
    /// like Linux's failed `drm_crtc_vblank_get`.
    fn handle_crtc_get_sequence(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtcGetSequence;
        let mut g: DrmModeCrtcGetSequence = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if g.crtc_id != CRTC_ID {
            return Err(VfsError::NotFound);
        }
        if !self.crtc_active() {
            return Err(VfsError::InvalidInput);
        }
        let now_ns = monotonic_time_nanos();
        let (sequence, edge_ns) = self
            .vblank
            .active_at(now_ns)
            .ok_or(VfsError::InvalidInput)?;
        g.active = 1;
        g.sequence = sequence;
        g.sequence_ns = edge_ns as i64;
        ptr.vm_write(current, g).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// `CRTC_QUEUE_SEQUENCE` — deliver a `DRM_EVENT_CRTC_SEQUENCE` when
    /// the counter reaches the target, mirroring
    /// `drm_crtc_queue_sequence_ioctl()` + `drm_queue_vblank_event()`
    /// (Linux 4.19): unknown flags → `EINVAL`, unknown CRTC → `ENOENT`,
    /// inactive CRTC → `EINVAL`, a missed target fires immediately, and
    /// the reply's `sequence` reports the target (or the current counter
    /// when it fired immediately).
    fn handle_crtc_queue_sequence(
        &self,
        file: &Card0File,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtcQueueSequence;
        let mut q: DrmModeCrtcQueueSequence =
            ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if q.crtc_id != CRTC_ID {
            return Err(VfsError::NotFound);
        }
        if q.flags & !(DRM_CRTC_SEQUENCE_RELATIVE | DRM_CRTC_SEQUENCE_NEXT_ON_MISS) != 0 {
            return Err(VfsError::InvalidInput);
        }
        file.serve_pending_vblank_events();
        let mode = self.state.lock();
        if mode.crtc_active == 0 {
            return Err(VfsError::InvalidInput);
        }

        let now_ns = monotonic_time_nanos();
        let (current_sequence, current_edge_ns) = self
            .vblank
            .active_at(now_ns)
            .ok_or(VfsError::InvalidInput)?;
        let mut target = if q.flags & DRM_CRTC_SEQUENCE_RELATIVE != 0 {
            current_sequence.wrapping_add(q.sequence)
        } else {
            q.sequence
        };
        if q.flags & DRM_CRTC_SEQUENCE_NEXT_ON_MISS != 0
            && vblank_passed(current_sequence, target)
        {
            target = current_sequence + 1;
        }

        let queued = !vblank_passed(current_sequence, target);
        if !queued {
            // Missed: fire synchronously with the current counter, like
            // Linux's immediate `send_vblank_event`.
            let ev = DrmEventCrtcSequence {
                base: DrmEvent {
                    event_type: DRM_EVENT_CRTC_SEQUENCE,
                    length: core::mem::size_of::<DrmEventCrtcSequence>() as u32,
                },
                user_data: q.user_data,
                tv_ns: current_edge_ns as i64,
                sequence: current_sequence,
            };
            file.enqueue_event(&ev)?;
            q.sequence = current_sequence;
        } else {
            file.queue_pending(PendingVblankEvent {
                event: QueuedVblankEvent::CrtcSequence {
                    user_data: q.user_data,
                },
                target_sequence: target,
            })?;
            q.sequence = target;
        }
        drop(mode);
        if queued {
            self.vblank_event.notify(usize::MAX);
        }
        ptr.vm_write(current, q).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// `WAIT_VBLANK` — wait for the synthesized counter to reach a
    /// target, mirroring `drm_wait_vblank_ioctl()` (Linux 4.19
    /// `drm_vblank.c`): `_DRM_VBLANK_SIGNAL` and unknown type bits →
    /// `EINVAL`, an inactive CRTC → `EINVAL` (Linux's `drm_vblank_get`
    /// fails when the counter is disabled), the query variant
    /// (relative + zero + no flags) short-circuits, `_DRM_VBLANK_EVENT`
    /// queues a `DRM_EVENT_VBLANK` instead of blocking, and the blocking
    /// variant sleeps until the target edge. Replies carry the vblank
    /// timestamp like `drm_wait_vblank_reply()`.
    fn handle_wait_vblank(
        &self,
        file: &Card0File,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmWaitVblank;
        let mut req: DrmWaitVblank = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        if req.rep_type & DRM_VBLANK_SIGNAL != 0 {
            return Err(VfsError::InvalidInput);
        }
        if req.rep_type & !(DRM_VBLANK_TYPES_MASK | DRM_VBLANK_FLAGS_MASK | DRM_VBLANK_HIGH_CRTC_MASK)
            != 0
        {
            return Err(VfsError::InvalidInput);
        }
        // Bits 1..6 of `type` are a CRTC index; the secondary flag picks
        // CRTC 1. This card exposes exactly one CRTC at index 0.
        if req.rep_type & (DRM_VBLANK_HIGH_CRTC_MASK | DRM_VBLANK_SECONDARY) != 0 {
            return Err(VfsError::InvalidInput);
        }
        if req.rep_type & DRM_VBLANK_EVENT != 0 {
            file.serve_pending_vblank_events();
        }
        let mode = self.state.lock();
        if mode.crtc_active == 0 {
            return Err(VfsError::InvalidInput);
        }

        let vtype = req.rep_type;
        let now_ns = monotonic_time_nanos();
        let (current_sequence, current_edge_ns) = self
            .vblank
            .active_at(now_ns)
            .ok_or(VfsError::InvalidInput)?;

        // Query short-circuit: relative with a zero target and no event
        // or next-on-miss flags just reports the current counter.
        if req.sequence == 0
            && (vtype & (DRM_VBLANK_RELATIVE | DRM_VBLANK_EVENT | DRM_VBLANK_NEXTONMISS))
                == DRM_VBLANK_RELATIVE
        {
            let reply = self.wait_vblank_reply(vtype, current_sequence, current_edge_ns);
            ptr.vm_write(current, reply).map_err(|_| VfsError::BadAddress)?;
            return Ok(0);
        }

        let mut target = match vtype & DRM_VBLANK_TYPES_MASK {
            DRM_VBLANK_RELATIVE => current_sequence.wrapping_add(u64::from(req.sequence)),
            // Absolute: widen the u32 counter against the current u64.
            0 => widen_32_to_64(req.sequence, current_sequence),
            _ => return Err(VfsError::InvalidInput),
        };
        // Linux converts relative requests to absolute and clears the
        // bit in the echoed-back type.
        if vtype & DRM_VBLANK_RELATIVE != 0 {
            req.rep_type &= !DRM_VBLANK_RELATIVE;
        }
        req.sequence = target as u32;
        if vtype & DRM_VBLANK_NEXTONMISS != 0 && vblank_passed(current_sequence, target) {
            target = current_sequence + 1;
            req.sequence = target as u32;
            req.rep_type &= !DRM_VBLANK_NEXTONMISS;
        }

        if vtype & DRM_VBLANK_EVENT != 0 {
            // `_DRM_VBLANK_EVENT`: `request.signal` (overlaid on
            // `tv_sec`) is the user_data of the delivered event.
            let user_data = req.tv_sec as u64;
            let fired = vblank_passed(current_sequence, target);
            if fired {
                self.queue_vblank_event(file, user_data, current_sequence, current_edge_ns)?;
            } else {
                file.queue_pending(PendingVblankEvent {
                    event: QueuedVblankEvent::Vblank { user_data },
                    target_sequence: target,
                })?;
            }
            drop(mode);
            if !fired {
                self.vblank_event.notify(usize::MAX);
            }
            let reply_sequence = if fired { current_sequence } else { target };
            req.sequence = reply_sequence as u32;
            ptr.vm_write(current, req).map_err(|_| VfsError::BadAddress)?;
            return Ok(0);
        }

        let disable_generation = self.vblank.disable_generation();
        drop(mode);

        // A signal interrupts without a reply. Disabling the CRTC completes
        // the wait at the frozen edge; the total wait is capped at 3 seconds.
        let timeout_ns = monotonic_time_nanos().saturating_add(3_000_000_000);
        let (mut sequence, mut edge_ns) = (current_sequence, current_edge_ns);
        let mut timed_out = false;
        while !vblank_passed(sequence, target) {
            listener!(self.vblank_event => listener);
            let now_ns = monotonic_time_nanos();
            let (active, current_sequence, edge) =
                self.vblank.status_since(now_ns, disable_generation);
            (sequence, edge_ns) = (current_sequence, edge);
            if !active || vblank_passed(sequence, target) {
                break;
            }
            if now_ns >= timeout_ns {
                timed_out = true;
                break;
            }
            let deadline_ns = self.vblank.deadline_ns(target, now_ns).unwrap_or(timeout_ns);
            let deadline = core::time::Duration::from_nanos(deadline_ns.min(timeout_ns));
            let _ = block_on_user(current, timeout_at(Some(deadline), listener))
            .into_result()
            .map_err(|_| VfsError::Interrupted)?;
        }
        let reply = self.wait_vblank_reply(req.rep_type, sequence, edge_ns);
        ptr.vm_write(current, reply).map_err(|_| VfsError::BadAddress)?;
        if timed_out {
            Err(VfsError::ResourceBusy)
        } else {
            Ok(0)
        }
    }

    /// Builds a `WAIT_VBLANK` reply reporting `sequence`'s edge time,
    /// mirroring `drm_wait_vblank_reply()`: the truncated counter plus
    /// the timestamp of the most recent vblank edge.
    fn wait_vblank_reply(&self, rep_type: u32, sequence: u64, edge_ns: u64) -> DrmWaitVblank {
        let edge_ns = edge_ns as i64;
        DrmWaitVblank {
            rep_type,
            sequence: sequence as u32,
            tv_sec: edge_ns / 1_000_000_000,
            tv_usec: (edge_ns % 1_000_000_000) / 1_000,
        }
    }

    /// Queues an immediate `DRM_EVENT_VBLANK` for a fired target.
    fn queue_vblank_event(
        &self,
        file: &Card0File,
        user_data: u64,
        sequence: u64,
        edge_ns: u64,
    ) -> VfsResult<()> {
        let ev = DrmEventVblank {
            base: DrmEvent {
                event_type: DRM_EVENT_VBLANK,
                length: core::mem::size_of::<DrmEventVblank>() as u32,
            },
            user_data,
            tv_sec: (edge_ns / 1_000_000_000) as u32,
            tv_usec: ((edge_ns % 1_000_000_000) / 1_000) as u32,
            sequence: sequence as u32,
            crtc_id: CRTC_ID,
        };
        file.enqueue_event(&ev)
    }

    // ======== M4c: atomic commit + blob properties ========

    fn handle_atomic(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeAtomic;
        let a: DrmModeAtomic = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        let known = DRM_MODE_ATOMIC_TEST_ONLY
            | DRM_MODE_ATOMIC_NONBLOCK
            | DRM_MODE_ATOMIC_ALLOW_MODESET
            | DRM_MODE_PAGE_FLIP_EVENT;
        if a.flags & !known != 0 {
            return Err(VfsError::InvalidInput);
        }

        let n = a.count_objs as usize;
        let objs: Vec<u32> =
            vm_load(current, a.objs_ptr as *const u32, n).map_err(|_| VfsError::BadAddress)?;
        let counts: Vec<u32> = vm_load(current, a.count_props_ptr as *const u32, n)
            .map_err(|_| VfsError::BadAddress)?;
        let total_props: usize = counts.iter().map(|c| *c as usize).sum();
        let props: Vec<u32> = vm_load(current, a.props_ptr as *const u32, total_props)
            .map_err(|_| VfsError::BadAddress)?;
        let values: Vec<u64> = vm_load(current, a.prop_values_ptr as *const u64, total_props)
            .map_err(|_| VfsError::BadAddress)?;

        let _operation = self.modeset_operation.lock();
        let old_state = self.state.lock().clone();
        let mut proposed = old_state.clone();
        let mut idx = 0;
        for (obj_i, &obj_id) in objs.iter().enumerate() {
            let obj_type = object_type_of(obj_id).ok_or(VfsError::NotFound)?;
            for _ in 0..counts[obj_i] {
                let prop_id = props[idx];
                let value = values[idx];
                idx += 1;
                if !self.apply_prop(obj_type, prop_id, value, &mut proposed)? {
                    return Err(VfsError::InvalidInput);
                }
            }
        }

        if a.flags & DRM_MODE_ATOMIC_TEST_ONLY != 0 {
            if proposed.plane_fb_id != 0 && proposed.crtc_active != 0 {
                self.present_fb(proposed.plane_fb_id, &proposed, true)?;
            } else {
                self.clear_scanout(true).map_err(map_display_err)?;
            }
            return Ok(0);
        }
        let reservation = if a.flags & DRM_MODE_PAGE_FLIP_EVENT != 0 {
            Some(file.reserve_event()?)
        } else {
            None
        };
        let current_fb = proposed.plane_fb_id;
        if current_fb != 0 && proposed.crtc_active != 0 {
            self.present_fb(current_fb, &proposed, false)?;
        } else if old_state.plane_fb_id != 0 {
            self.clear_scanout(false).map_err(map_display_err)?;
        }
        let mut state = self.state.lock();
        *state = proposed;
        let completed = self.set_vblank_active(&state, monotonic_time_nanos());
        let flip_edge = reservation
            .as_ref()
            .map(|_| self.vblank.snapshot_at(monotonic_time_nanos()));
        drop(state);
        drop(_operation);
        self.notify_vblank_change(completed);
        if let (Some(reservation), Some((sequence, edge_ns))) = (reservation, flip_edge) {
            self.queue_flip_event(reservation, a.user_data, sequence, edge_ns);
        }
        Ok(0)
    }

    /// Apply one `(prop_id, value)` tuple onto `s`. Returns `Ok(true)`
    /// if the tuple is valid for the given object type, `Ok(false)` if
    /// the property isn't one the object exposes.
    fn apply_prop(
        &self,
        obj_type: u32,
        prop_id: u32,
        value: u64,
        s: &mut ModesetState,
    ) -> VfsResult<bool> {
        match (obj_type, prop_id) {
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_TYPE) => {
                // IMMUTABLE: accept only the plane's own type.
                if value != DRM_PLANE_TYPE_PRIMARY {
                    return Err(VfsError::InvalidInput);
                }
            }
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_FB_ID) => {
                let fb = value as u32;
                if fb != 0 && !self.fbs.lock().contains_key(&fb) {
                    return Err(VfsError::InvalidInput);
                }
                s.plane_fb_id = fb;
            }
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_CRTC_ID) => {
                let c = value as u32;
                if c != 0 && c != CRTC_ID {
                    return Err(VfsError::InvalidInput);
                }
                s.plane_crtc_id = c;
            }
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_SRC_X) => s.plane_src_x = value,
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_SRC_Y) => s.plane_src_y = value,
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_SRC_W) => s.plane_src_w = value,
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_SRC_H) => s.plane_src_h = value,
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_CRTC_X) => {
                s.plane_crtc_x = checked_i32(value)? as i64;
            }
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_CRTC_Y) => {
                s.plane_crtc_y = checked_i32(value)? as i64;
            }
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_CRTC_W) => s.plane_crtc_w = value,
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_CRTC_H) => s.plane_crtc_h = value,
            (DRM_MODE_OBJECT_PLANE, PROP_PLANE_IN_FENCE_FD) => {
                // Linux borrows a sync-file fence and never closes the user's
                // fd, including on TEST_ONLY or failure. No Starry file type
                // provides a sync-file yet, so every non-sentinel is invalid.
                if value != u64::MAX {
                    return Err(VfsError::InvalidInput);
                }
            }
            (DRM_MODE_OBJECT_CRTC, PROP_CRTC_ACTIVE) => {
                if value > 1 {
                    return Err(VfsError::InvalidInput);
                }
                s.crtc_active = value;
            }
            (DRM_MODE_OBJECT_CRTC, PROP_CRTC_MODE_ID) => {
                let id = u32::try_from(value).map_err(|_| VfsError::InvalidInput)?;
                s.mode = if id == 0 {
                    None
                } else {
                    let bytes = self
                        .blobs
                        .lock()
                        .get(&id)
                        .cloned()
                        .or_else(|| {
                            s.mode
                                .as_ref()
                                .filter(|mode| mode.id == id)
                                .map(|mode| mode.bytes.clone())
                        })
                        .ok_or(VfsError::InvalidInput)?;
                    if bytes.len() != size_of::<DrmModeModeInfo>() {
                        return Err(VfsError::InvalidInput);
                    }
                    let info = bytemuck::try_pod_read_unaligned(&bytes)
                        .map_err(|_| VfsError::InvalidInput)?;
                    Some(ModeBlob { id, info, bytes })
                };
            }
            (DRM_MODE_OBJECT_CONNECTOR, PROP_CONN_CRTC_ID) => {
                let c = value as u32;
                if c != 0 && c != CRTC_ID {
                    return Err(VfsError::InvalidInput);
                }
                s.conn_crtc_id = c;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn handle_create_blob(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCreateBlob;
        let mut c: DrmModeCreateBlob = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if c.length == 0 || c.length as usize > MAX_BLOB_BYTES {
            return Err(VfsError::InvalidInput);
        }
        let bytes: Vec<u8> = vm_load(current, c.data as *const u8, c.length as usize)
            .map_err(|_| VfsError::BadAddress)?;
        let id = self.next_blob_id.fetch_add(1, Ordering::Relaxed);
        self.blobs.lock().insert(id, Arc::new(bytes));
        c.blob_id = id;
        ptr.vm_write(current, c).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_destroy_blob(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDestroyBlob;
        let d: DrmModeDestroyBlob = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        // System (kernel-owned) blobs are not user-destroyable. Linux's
        // DRM rejects ENOTSUPP for this; we map it to PermissionDenied
        // (EPERM) since VfsError lacks a finer-grained variant.
        if self.system_blobs.lock().contains_key(&d.blob_id) {
            return Err(VfsError::PermissionDenied);
        }
        // Drop the user-publish reference. If `state.mode` still
        // holds the same Arc (i.e. a commit pinned this blob as the CRTC's
        // `MODE_ID`), the blob data stays alive and
        // `GETPROPBLOB` keeps succeeding via the committed-state lookup
        // below until a later atomic commit replaces `MODE_ID`.
        self.blobs
            .lock()
            .remove(&d.blob_id)
            .ok_or(VfsError::NotFound)?;
        Ok(0)
    }

    fn handle_get_blob(&self, current: &crate::task::UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeGetBlob;
        let mut g: DrmModeGetBlob = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        // Never hold a registry/state lock across faultable output copies.
        // Separate lookups also avoid nesting blobs -> state against commits.
        let published = self.blobs.lock().get(&g.blob_id).cloned();
        let bytes = published
            .or_else(|| {
                self.state
                    .lock()
                    .mode
                    .as_ref()
                    .filter(|mode| mode.id == g.blob_id)
                    .map(|mode| mode.bytes.clone())
            })
            .or_else(|| self.system_blobs.lock().get(&g.blob_id).cloned())
            .ok_or(VfsError::NotFound)?;
        if g.data != 0 && g.length > 0 {
            let n = (g.length as usize).min(bytes.len());
            vm_write_slice(current, g.data as *mut u8, &bytes[..n])
                .map_err(|_| VfsError::BadAddress)?;
        }
        g.length = bytes.len() as u32;
        ptr.vm_write(current, g).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn resource_for_handle(&self, file: &Card0File, handle: u32) -> Option<Arc<GpuResource>> {
        if let Some(resource) = self
            .blob_aliases
            .lock()
            .get(&handle)
            .filter(|alias| alias.owner == file.file_id)
            .map(|alias| alias.resource.clone())
        {
            return Some(resource);
        }
        if let Some(resource) = self
            .dumbs
            .lock()
            .get(&handle)
            .filter(|buffer| buffer.owner == file.file_id)
            .and_then(|buffer| buffer.resource.clone())
        {
            return Some(resource);
        }
        self.gpu_resources
            .lock()
            .values()
            .find(|resource| resource.owner == file.file_id && resource.bo_handle == handle)
            .cloned()
    }
}

/// Map a fixed object id to its `DRM_MODE_OBJECT_*` type tag.
fn object_type_of(id: u32) -> Option<u32> {
    match id {
        CRTC_ID => Some(DRM_MODE_OBJECT_CRTC),
        CONNECTOR_ID => Some(DRM_MODE_OBJECT_CONNECTOR),
        PLANE_ID => Some(DRM_MODE_OBJECT_PLANE),
        _ => None,
    }
}

/// Map a GPU 3D error from the display layer to a VfsError.
fn map_gpu_err(err: GpuError) -> VfsError {
    match err {
        GpuError::Unsupported => VfsError::Unsupported,
        GpuError::NotAvailable | GpuError::NotReady => VfsError::WouldBlock,
        GpuError::InvalidArgument | GpuError::InvalidHandle => VfsError::InvalidInput,
        GpuError::Busy => VfsError::ResourceBusy,
        GpuError::OutOfMemory => VfsError::NoMemory,
        // A bounded device wait expired (stalled host). Linux has no single
        // errno for this: VIRTGPU_WAIT reports -EBUSY (mapped at its call
        // site), while our teardown drains surface it as the hard ETIMEDOUT.
        GpuError::TimedOut => VfsError::TimedOut,
        GpuError::DeviceLost | GpuError::Io => VfsError::Io,
    }
}

fn map_display_err(err: DisplayError) -> VfsError {
    match err {
        DisplayError::Unsupported => VfsError::Unsupported,
        DisplayError::NotAvailable | DisplayError::NotReady => VfsError::WouldBlock,
        DisplayError::InvalidOutput | DisplayError::InvalidState => VfsError::InvalidInput,
        DisplayError::Busy => VfsError::ResourceBusy,
        DisplayError::DeviceLost | DisplayError::Io => VfsError::Io,
        DisplayError::Gpu(error) => map_gpu_err(error),
    }
}

/// Narrow a userspace-supplied u64 to an i32-range signed integer.
fn checked_i32(value: u64) -> VfsResult<i32> {
    let v = value as i64;
    if (i32::MIN as i64..=i32::MAX as i64).contains(&v) {
        Ok(v as i32)
    } else {
        Err(VfsError::InvalidInput)
    }
}

// Suppress dead_code for `DumbBuffer.width/height/bpp/pitch`.  These
// four fields are metadata-only (see the struct-level doc comment) and
// are never consumed by any ioctl handler, but keeping them in the
// struct makes a potential future `GET_DUMB_INFO` possible and makes
// debug dumps informative.  The closure below signals to the compiler
// that the field access is intentional — they are not "unnecessary".
#[allow(dead_code)]
const _DUMB_BUFFER_FIELDS_USED: fn(&DumbBuffer) = |b| {
    let _ = (b.width, b.height, b.bpp, b.pitch);
    let _ = (b.size, b.offset, &b.mapping);
};

/// The `fence_fd` value written back by EXECBUFFER: an out-fence fd replaces
/// the field, and an IN-only request keeps its input fd untouched (Linux only
/// updates the field when it created an out-fence).
fn writeback_fence_fd(current: i32, out_fd: Option<i32>) -> i32 {
    out_fd.unwrap_or(current)
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;
    use ax_gpu::rdif_display::{DisplayEvent, DriverGeneric};
    use core::num::NonZeroU64;

    #[test]
    fn device_loss_releases_all_retired_scanout_pins() {
        let first = Arc::new(());
        let second = Arc::new(());
        let first_weak = Arc::downgrade(&first);
        let second_weak = Arc::downgrade(&second);
        let first_completion = Completion::Pending(NonZeroU64::new(1).unwrap());
        let second_completion = Completion::Pending(NonZeroU64::new(2).unwrap());
        let retired = vec![
            (first_completion, Arc::clone(&first)),
            (second_completion, Arc::clone(&second)),
        ];
        drop((first, second));

        let (pending, device_lost) = pending_scanouts_after_query(retired, |completion| {
            if completion == first_completion {
                Ok(CompletionStatus::Pending)
            } else {
                Err(DisplayError::DeviceLost)
            }
        });
        assert!(device_lost);
        assert!(pending.is_empty());
        assert!(first_weak.upgrade().is_none());
        assert!(second_weak.upgrade().is_none());
    }

    struct DisconnectedDisplay {
        connected: [bool; 2],
        states: [Option<DisplayState>; 2],
    }

    impl DriverGeneric for DisconnectedDisplay {
        fn name(&self) -> &str {
            "disconnected-display"
        }
    }

    impl DisplayController for DisconnectedDisplay {
        fn output_count(&self) -> u32 {
            2
        }

        fn output(&self, id: OutputId) -> Result<OutputInfo, DisplayError> {
            let connected = *self
                .connected
                .get(id.id() as usize)
                .ok_or(DisplayError::InvalidOutput)?;
            Ok(OutputInfo {
                id,
                name: String::from("Disconnected"),
                kind: OutputKind::Virtual,
                connected,
                physical_size_mm: None,
                modes: Vec::new(),
                preferred_mode: None,
                formats: Vec::new(),
            })
        }

        fn current_state(&self, id: OutputId) -> Result<Option<DisplayState>, DisplayError> {
            self.states
                .get(id.id() as usize)
                .cloned()
                .ok_or(DisplayError::InvalidOutput)
        }

        fn check(&self, _state: &DisplayState) -> Result<(), DisplayError> {
            unreachable!()
        }

        fn commit(&mut self, _state: &DisplayState) -> Result<Completion, DisplayError> {
            unreachable!()
        }

        fn commit_status(
            &mut self,
            _completion: Completion,
        ) -> Result<CompletionStatus, DisplayError> {
            unreachable!()
        }

        fn poll_event(&mut self) -> Option<DisplayEvent> {
            unreachable!()
        }
    }

    #[test]
    fn disconnected_output_with_active_scanout_can_be_cleared() {
        let output = OutputId::new(1);
        let display = DisconnectedDisplay {
            connected: [true, false],
            states: [None, Some(DisplayState {
                output,
                mode: Some(DisplayMode {
                    width: 1,
                    height: 1,
                    refresh_millihz: 0,
                }),
                framebuffer: Some(ScanoutFramebuffer {
                    buffer: ScanoutBuffer::Gpu(BufferHandle::new(NonZeroU64::new(1).unwrap())),
                    width: 1,
                    height: 1,
                    stride: 4,
                    offset: 0,
                    format: PixelFormat::Xrgb8888,
                }),
                damage: Vec::new(),
            })],
        };

        assert_eq!(scanout_output_to_clear(&display), Ok(output));
        assert_eq!(
            select_output_for_present(&display),
            Err(DisplayError::NotAvailable)
        );
        let cleared = DisconnectedDisplay {
            connected: [true, false],
            states: [None, None],
        };
        assert_eq!(
            select_output_for_present(&cleared).unwrap().id,
            OutputId::new(0)
        );
        let all_disconnected = DisconnectedDisplay {
            connected: [false, false],
            ..display
        };
        assert_eq!(scanout_output_to_clear(&all_disconnected), Ok(output));
    }

    #[test]
    fn disabling_completes_all_pending_event_types_at_the_frozen_edge() {
        let mut events = Card0Events::default();
        events.pending.push(PendingVblankEvent {
            event: QueuedVblankEvent::CrtcSequence { user_data: 12 },
            target_sequence: 100,
        });
        events.pending.push(PendingVblankEvent {
            event: QueuedVblankEvent::Vblank { user_data: 34 },
            target_sequence: 100,
        });
        assert!(events.complete_pending(3, 5_000));
        assert!(events.pending.is_empty());
        assert_eq!(events.ready.len(), 2);
        assert_eq!(
            events.ready.pop_front(),
            Some(vblank_event_slot(
                PendingVblankEvent {
                    event: QueuedVblankEvent::CrtcSequence { user_data: 12 },
                    target_sequence: 100,
                },
                3,
                5_000
            ))
        );
        assert_eq!(
            events.ready.pop_front(),
            Some(vblank_event_slot(
                PendingVblankEvent {
                    event: QueuedVblankEvent::Vblank { user_data: 34 },
                    target_sequence: 100,
                },
                3,
                5_000
            ))
        );
        assert!(events.has_space());
    }
}
