//! `/dev/dri/card0` — minimal DRM character device.
//!
//! Single-CRTC, single-connector, single-plane simpledrm-class driver
//! over the existing `axdisplay` framebuffer. Covers legacy libdrm
//! (`CREATE_DUMB → ADDFB2 → SETCRTC → PAGE_FLIP`) and the atomic-KMS
//! path (`MODE_ATOMIC` + blob properties) used by modern compositors.
//!
//! Fixed IDs:
//!   crtc=0x10, encoder=0x20, connector=0x30, plane=0x40
//!
//! Simplifications vs. a real DRM driver:
//!   - Each `CREATE_DUMB` allocates its own page-aligned `GlobalPage`
//!     sized for the requested geometry; `MAP_DUMB` returns a unique
//!     monotonic offset key; `Card0::mmap(offset, length)` resolves that key
//!     back to the buffer's per-allocation physical range. On
//!     `SETCRTC` / `PAGE_FLIP` / non-`TEST_ONLY` atomic commit,
//!     `present_fb` presents the committed buffer: guest-RAM dumb
//!     buffers are memcpy'd into the axdisplay scanout framebuffer and
//!     `framebuffer_flush` kicked, while host-side virgl 3D resources
//!     (Weston/glamor GBM scanout buffers) are bound with
//!     `SET_SCANOUT` + `RESOURCE_FLUSH` — matching Linux
//!     `virtio_gpu_plane_atomic_update`. PRIME export and virtio-gpu
//!     zero-copy resource plumbing land in follow-on PRs.
//!   - Property validation is permissive: value ranges aren't rigorously
//!     enforced (tests drive sensible values). Atomic rejects only
//!     unknown `(obj, prop)` pairs and obviously-bad object/blob refs.
//!   - `WAIT_VBLANK` returns immediately with a bumped sequence number;
//!     there's no real vblank source to wait on.
//!   - Mode list: one mode matching axdisplay's resolution at a
//!     synthesized 60 Hz.

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
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    task::Context,
};

use ax_alloc::GlobalPage;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddrRange};
use ax_runtime::hal::{mem::virt_to_phys, time::monotonic_time};
use ax_task::current_may_uninit;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use bytemuck::bytes_of;
use linux_raw_sys::general::O_CLOEXEC;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use super::drm::{
    DRM_CAP_ADDFB2_MODIFIERS,
    DRM_CAP_CRTC_IN_VBLANK_EVENT,
    DRM_CAP_DUMB_BUFFER,
    DRM_CAP_PRIME,
    DRM_CAP_TIMESTAMP_MONOTONIC,
    DRM_EVENT_FLIP_COMPLETE,
    DRM_FORMAT_ARGB8888,
    DRM_FORMAT_MOD_INVALID,
    DRM_FORMAT_MOD_LINEAR,
    DRM_FORMAT_XRGB8888,
    DRM_IOCTL_AUTH_MAGIC,
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
    // virtgpu structs and constants
    DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
    DRM_IOCTL_VIRTGPU_EXECBUFFER,
    DRM_IOCTL_VIRTGPU_GET_CAPS,
    DRM_IOCTL_VIRTGPU_GETPARAM,
    DRM_IOCTL_VIRTGPU_MAP,
    DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
    DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB,
    DRM_IOCTL_VIRTGPU_RESOURCE_INFO,
    DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST,
    DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST,
    DRM_IOCTL_VIRTGPU_WAIT,
    DRM_IOCTL_WAIT_VBLANK,
    DRM_MODE_ATOMIC_ALLOW_MODESET,
    DRM_MODE_ATOMIC_NONBLOCK,
    DRM_MODE_ATOMIC_TEST_ONLY,
    DRM_MODE_CONNECTED,
    DRM_MODE_CONNECTOR_VIRTUAL,
    DRM_MODE_ENCODER_VIRTUAL,
    DRM_MODE_FB_MODIFIERS,
    DRM_MODE_OBJECT_CONNECTOR,
    DRM_MODE_OBJECT_CRTC,
    DRM_MODE_OBJECT_PLANE,
    DRM_MODE_PAGE_FLIP_EVENT,
    DRM_MODE_PROP_ATOMIC,
    DRM_MODE_PROP_BLOB,
    DRM_MODE_PROP_ENUM,
    DRM_MODE_PROP_IMMUTABLE,
    DRM_MODE_PROP_OBJECT,
    DRM_MODE_PROP_RANGE,
    DRM_PLANE_TYPE_PRIMARY,
    DRM_PRIME_CAP_EXPORT,
    DRM_PRIME_CAP_IMPORT,
    DRM_PROP_NAME_LEN,
    DrmAuth,
    DrmClipRect,
    DrmEvent,
    DrmEventVblank,
    DrmGemClose,
    DrmGetCap,
    DrmModeAtomic,
    DrmModeCardRes,
    DrmModeCreateBlob,
    DrmModeCreateDumb,
    DrmModeCrtc,
    DrmModeCrtcPageFlip,
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
    DrmVirtgpu3dTransferFromHost,
    DrmVirtgpu3dTransferToHost,
    DrmVirtgpu3dWait,
    DrmVirtgpuContextInit,
    DrmVirtgpuContextSetParam,
    DrmVirtgpuExecbuffer,
    DrmVirtgpuGetCaps,
    DrmVirtgpuGetparam,
    DrmVirtgpuMap,
    DrmVirtgpuResourceCreate,
    DrmVirtgpuResourceCreateBlob,
    DrmVirtgpuResourceInfo,
    DrmWaitVblank,
    VIRTGPU_BLOB_MEM_GUEST,
    VIRTGPU_BLOB_MEM_HOST3D,
    VIRTGPU_BLOB_MEM_HOST3D_GUEST,
    VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
    VIRTGPU_CONTEXT_PARAM_NUM_RINGS,
    VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK,
    VIRTGPU_DRM_CAPSET_DRM,
    VIRTGPU_DRM_CAPSET_VIRGL,
    VIRTGPU_DRM_CAPSET_VIRGL2,
    VIRTGPU_EXECBUF_FENCE_FD_IN,
    VIRTGPU_EXECBUF_FENCE_FD_OUT,
    VIRTGPU_PARAM_3D_FEATURES,
    VIRTGPU_PARAM_CAPSET_QUERY_FIX,
    VIRTGPU_PARAM_CONTEXT_INIT,
    VIRTGPU_PARAM_CROSS_DEVICE,
    VIRTGPU_PARAM_HOST_VISIBLE,
    VIRTGPU_PARAM_RESOURCE_BLOB,
    VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS,
    VIRTGPU_WAIT_NOWAIT,
};
use super::sync_file::SyncFile;
use crate::{
    StarryError, StarryResult,
    file::{FileLike, add_file_like, get_file_like},
    pseudofs::{DeviceMmap, DeviceOps},
    sync::Mutex,
    task::AsThread,
};

pub const DRIVER_NAME: &str = "virtio_gpu";
pub const DRIVER_DATE: &str = "2026-04-19";
pub const DRIVER_DESC: &str = "StarryOS simple DRM driver";
// Linux virtio-gpu 的 `DRIVER_VERSION_MINOR = 1` 随
// `VIRTGPU_EXECBUF_FENCE_FD` 引入即已 bump（virtgpu_drm.h `.minor = 1`）。
// mesa 的 `virgl_drm_get_version`(virgl_drm_winsys.c)要求 `version_major == 0`
// 否则返回 -EINVAL → virgl winsys 创建失败；并以
// `drm_version >= VIRGL_DRM_VERSION_FENCE_FD (0,1)` 决定走 fd-based fence
// 还是 legacy fence（后者每帧创建一个 8×1 占位资源 + WAIT + GEM_CLOSE）。
// minor=1 与 Linux 稳定 ABI 对齐，让 mesa 走 fd fence 路径（见 sync_file.rs）。
pub const DRIVER_VERSION_MAJOR: i32 = 0;
pub const DRIVER_VERSION_MINOR: i32 = 1;
pub const DRIVER_VERSION_PATCHLEVEL: i32 = 0;

/// Fixed object IDs advertised by GETRESOURCES / GETCONNECTOR / GETENCODER.
const CRTC_ID: u32 = 0x10;
const ENCODER_ID: u32 = 0x20;
const CONNECTOR_ID: u32 = 0x30;
const PLANE_ID: u32 = 0x40;

/// The implicit virgl context used for all 3D commands on this card.
///
/// **Must be non-zero.** virglrenderer rejects `ctx_id == 0` at context
/// creation (`virgl_renderer_context_create_with_flags` returns EINVAL for
/// First context id.  Linux starts at 1 (virglrenderer rejects id 0).
/// Allocated per-fd via `next_ctx_id` to match
/// `atomic_inc_return(&vgdev->ctx_id_cursor)` in the Linux kernel.
const FIRST_VIRGL_CTX_ID: u32 = 1;

/// First dumb-buffer handle we hand out.
const FIRST_DUMB_HANDLE: u32 = 1;
/// First framebuffer id we hand out from `ADDFB2`.
const FIRST_FB_ID: u32 = 1;

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
];
const CRTC_PROPS: &[u32] = &[PROP_CRTC_ACTIVE, PROP_CRTC_MODE_ID];
const CONN_PROPS: &[u32] = &[PROP_CONN_CRTC_ID];

/// Supported pixel formats advertised via `GETPLANE.format_type_ptr`.
const SUPPORTED_FORMATS: &[u32] = &[DRM_FORMAT_XRGB8888, DRM_FORMAT_ARGB8888];

/// Upper bound on the pending-event queue. Matches Linux's
/// `file->event_space` of 4 KB ≈ 128 `drm_event_vblank`s.
const MAX_EVENTS: usize = 128;

/// First blob id we hand out from `CREATEPROPBLOB`.
const FIRST_BLOB_ID: u32 = 0x1000;

/// Upper bound on `CREATEPROPBLOB` payload size.
const MAX_BLOB_BYTES: usize = 64 * 1024;

/// Metadata recorded per `CREATE_DUMB` call. Each buffer owns its own
/// page-aligned [`GlobalPage`] — no shared 128 MiB pool — so we don't
/// need a large contiguous physical region up front. The `offset` is
/// what `MAP_DUMB` returns and what `mmap` looks up: a synthetic,
/// monotonically-advancing key (not a real byte offset into anything)
/// that the mmap hook uses to locate this buffer's pages.
///
/// `pages` is `Arc<GlobalPage>`: `DESTROY_DUMB` drops Card0's strong
/// ref, but the `LinearBackend` cloned into each live VMA via
/// `DeviceMmap::Physical` keeps its own strong ref. The underlying
/// pages aren't released until every user mapping is unmapped, which
/// is exactly Linux's GEM refcount contract.
///
/// # Field semantics
///
/// Only `size`, `offset`, and `pages` are **consumed** by downstream
/// operations (`ADDFB2` reads `pages`+`size`; `mmap` reads `offset`;
/// `present_fb` reads `pages`+`size`).  The fields `width`, `height`,
/// `bpp`, and `pitch` are **metadata only** — written once by
/// `CREATE_DUMB` but never read back by any ioctl handler in this
/// driver.  They exist solely so that a human examining a debug dump
/// or a future `GET_DUMB_INFO` (if added) can see what geometry the
/// buffer was allocated for.
///
/// This matters for the `PRIME_FD_TO_HANDLE` import path: the
/// [`DrmPrimeHandle`] ioctl struct carries only `{handle, flags, fd}`
/// — it does **not** convey width/height/bpp/pitch from the exporting
/// driver.  Consequently an imported `DumbBuffer` will always have
/// these four fields set to zero.  No ioctl handler depends on them,
/// so the zero values are safe.  If a future commit adds code that
/// reads `.width` / `.height` / `.bpp` / `.pitch` from an imported
/// buffer, that code must handle the zero case (e.g. by falling back
/// to `ADDFB2`-supplied geometry).
struct DumbBuffer {
    width: u32,
    height: u32,
    bpp: u32,
    pitch: u32,
    size: u64,
    /// Unique mmap-offset key for this buffer.
    offset: u64,
    /// Backing pages. Refcounted so user mappings keep them alive
    /// across `DESTROY_DUMB`.
    pages: Arc<GlobalPage>,
}

/// Per-framebuffer state retained until `RMFB`. Holds the dumb
/// buffer's backing directly so a `DESTROY_DUMB` on the source
/// handle does not invalidate the fb — Linux's GEM contract says a
/// framebuffer keeps the buffer alive for as long as the fb_id is
/// live.
struct Framebuffer {
    /// Total backing size in bytes.
    size: u64,
    /// Row stride (pitch) in bytes — from ADDFB2.pitches[0].
    stride: u32,
    /// Framebuffer width in pixels — from ADDFB2.width.
    width: u32,
    /// Framebuffer height in pixels — from ADDFB2.height.
    height: u32,
    /// Backing storage kind. Present copies guest RAM for dumb buffers
    /// (2D path) and binds the host texture as scanout for virgl 3D
    /// resources (`SET_SCANOUT`) — matching Linux, which always sets the
    /// resource itself as scanout.
    kind: FbBacking,
    /// Pending damage region in framebuffer pixels, `Some((x, y, w, h))`.
    ///
    /// Set by [`Card::handle_dirty_fb`] from `DIRTY_FB` clips — the same
    /// input Linux feeds into `drm_atomic_helper_damage_merged` — and
    /// consumed by [`Card::present_fb`]: only that rect is
    /// `TRANSFER_TO_HOST_2D`'d / flushed. `None` means full-framebuffer
    /// damage, i.e. a present from a path that carries no clip data
    /// (`SETCRTC`, `PAGE_FLIP`, atomic) degrades to uploading the whole fb —
    /// Linux also falls back to the full plane when no damage clip arrives.
    dirty: Option<(u32, u32, u32, u32)>,
}

/// Backing storage for a DRM framebuffer.
#[derive(Clone)]
enum FbBacking {
    /// Guest RAM shared with a dumb buffer. The `Arc` keeps the pages
    /// alive until both this fb and any user mappings have been dropped.
    Dumb { pages: Arc<GlobalPage> },
    /// Host-side resource. `res_handle` is the host 2D/3D resource;
    /// `is_dumb_2d` marks a guest-backed 2D resource (dumb buffer) whose
    /// pixels must be `TRANSFER_TO_HOST_2D`'d from guest RAM before flush.
    /// 3D virgl/blob resources already hold their pixels on the host, so
    /// present must NOT transfer them (Linux `virtio_gpu_plane_atomic_update`
    /// transfers the dumb/2D case only).
    Gpu3d { res_handle: u32, is_dumb_2d: bool },
}

/// StarryOS kernel-side dma-buf GEM object for DRM card0.
///
/// Wraps the physical pages backing a dumb buffer so the exported fd
/// (returned by [`Self::handle_prime_handle_to_fd`]) can be mmap'd,
/// read, or passed via SCM_RIGHTS for cross-process buffer sharing.
/// Follows the same pattern as card1.rs's `ExportedGemBuffer`.
struct DmaBufGem {
    /// Physical address range of the underlying buffer.
    range: PhysAddrRange,
    /// Backing pages shared with the source dumb buffer — keeps the
    /// allocation alive even after a `DESTROY_DUMB` on the source
    /// handle.
    pages: Arc<GlobalPage>,
    /// Total size in bytes.
    size: u64,
}

impl FileLike for DmaBufGem {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:dmabuf".into()
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
        Ok(DeviceMmap::Physical(self.range, Some(self.pages.clone())))
    }
}

impl Pollable for DmaBufGem {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

/// Last legacy `SETCRTC` binding so `GETCRTC` can report what the
/// CRTC is currently scanning out. Linux DRM keeps this state on the
/// CRTC object itself; we keep it next to the atomic state but on a
/// separate lock because legacy SETCRTC needs to validate against
/// `fbs` and we don't want to nest locks when the validation may need
/// to take `fbs` while another path holds `state`.
#[derive(Debug, Default, Clone)]
struct LegacyCrtcState {
    fb_id: u32,
    connectors: Vec<u32>,
    mode: DrmModeModeInfo,
    mode_valid: u32,
    x: u32,
    y: u32,
}

/// Current values of all atomic-tunable properties on our single-CRTC /
/// single-connector / single-plane layout. Guarded by one mutex because
/// atomic commits touch multiple fields at once and userspace expects
/// the commit to be all-or-nothing.
#[derive(Debug, Default, Clone, Copy)]
struct ModesetState {
    crtc_active: u64,
    crtc_mode_id: u32,
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
    /// The GEM handle associated with this resource (from CREATE_DUMB or
    /// allocated by RESOURCE_CREATE).
    bo_handle: u32,
    /// Resource width in pixels.
    width: u32,
    /// Resource height in pixels.
    height: u32,
    /// Row stride in bytes.
    stride: u32,
    /// Resource size in bytes.
    size: u64,
    /// Contexts this resource has already been attached to. Linux attaches a
    /// resource once per GEM open (`virtio_gpu_gem_object_open` →
    /// CTX_ATTACH_RESOURCE); mirroring that at (ctx, resource) granularity is
    /// required because weston and the app hold separate contexts that share
    /// the same scanout buffer. Previously a single `attached: bool` was
    /// written but never read as a gate, so EXECBUFFER re-attached every
    /// handle every submit (~29 synchronous RTTs/frame ≈ the fixed 2.4ms
    /// floor). Empty set = not yet attached to any context.
    attached_ctxs: BTreeSet<u32>,
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
    /// PID of the process that issued VIRTGPU_RESOURCE_CREATE for this
    /// resource — Step-0 #2 forensic (who allocates buffers per frame).
    created_pid: u32,
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
    /// Host virtio-gpu resource ID (the thing virgl commands address).
    res_handle: u32,
    /// Resource size in bytes.
    size: u64,
    /// blob_mem (0 for classic 3D resources).
    blob_mem: u32,
    /// Weak card reference so the fd can reach the resource tables on
    /// close without the card holding a strong ref back (the card lives in
    /// devfs; fds live in process fd tables).
    card: Weak<Card0>,
}

impl Drop for HostResourceDmaBuf {
    fn drop(&mut self) {
        if let Some(card) = self.card.upgrade() {
            card.release_dma_buf_ref(self.res_handle);
        }
    }
}

impl FileLike for HostResourceDmaBuf {
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

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

/// Host-resource info for a blob dma-buf imported via `PRIME_FD_TO_HANDLE`.
///
/// Linux returns the *same* GEM object on a same-device import, so
/// `RESOURCE_INFO` on the imported handle must resolve back to the
/// exporter's host resource and blob_mem — this is what makes Mesa take the
/// `maybe_untyped` path and reuse the host texture (`virgl_drm_winsys.c`).
#[derive(Clone, Copy)]
struct ImportedBlob {
    res_handle: u32,
    size: u64,
    blob_mem: u32,
}

/// Per-process virgl context state (see `process_ctxs` doc).
struct PerFdCtx {
    initialized: bool,
    /// CONTEXT_INIT parameters. Kept for per-fd semantics (a future
    /// context-info query may report them); not read today.
    #[allow(dead_code)]
    capset_id: u32,
    #[allow(dead_code)]
    num_rings: u32,
    ctx_id: u32,
}

// ==== Experiment A: present-path timing instrumentation ====
//
// Guest-side slice of the present path (see AAA_starry_note §8.4). Because
// fork `enqueue_ctrl` busy-spins in-place on QueueFull and card0 never asks
// the device to wait otherwise, the flat sum of the slots below is the
// *guest-kernel* time per present. If it is µs-scale while glmark2 on-screen
// still reports ~2 ms/frame, the floor is host-side latency (→ experiment B,
// host `-trace virtio_gpu_*`); if it is ms-scale, the floor is this kernel
// path (QueueFull spin or IOCTL handling). Reported via `warn!` (visible at
// AX_LOG=warn) every `PERF_REPORT_EVERY` ATOMIC commits.
struct PerfSlot {
    cnt: AtomicU64,
    sum_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl PerfSlot {
    const fn new() -> Self {
        Self {
            cnt: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }

    fn add(&self, d: core::time::Duration) {
        let ns = d.as_nanos() as u64;
        self.cnt.fetch_add(1, Ordering::Relaxed);
        self.sum_ns.fetch_add(ns, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }
}

static PERF_ATOMIC: PerfSlot = PerfSlot::new(); // full MODE_ATOMIC handler
static PERF_SCANOUT: PerfSlot = PerfSlot::new(); // set_scanout enqueue
static PERF_TRANSFER: PerfSlot = PerfSlot::new(); // transfer_to_host_2d enqueue
static PERF_FLUSH: PerfSlot = PerfSlot::new(); // resource_flush enqueue
static PERF_EXECBUF: PerfSlot = PerfSlot::new(); // full EXECBUFFER handler
static PERF_RESCREATE: PerfSlot = PerfSlot::new(); // full VIRTGPU_RESOURCE_CREATE (incl. sync ctx_attach)
static PERF_WAIT: PerfSlot = PerfSlot::new(); // VIRTGPU_WAIT handler
static PERF_TRANS3D: PerfSlot = PerfSlot::new(); // VIRTGPU_TRANSFER_TO_HOST (3D)
static PERF_DUMBCREATE: PerfSlot = PerfSlot::new(); // MODE_CREATE_DUMB handler
static PERF_DUMMAP: PerfSlot = PerfSlot::new(); // MODE_MAP_DUMB handler
static PERF_DUMDESTROY: PerfSlot = PerfSlot::new(); // MODE_DESTROY_DUMB handler
static PERF_GEMCLOSE: PerfSlot = PerfSlot::new(); // GEM_CLOSE handler
const PERF_REPORT_EVERY: u64 = 300;

// ---- run6 frame-gap forensics: EXECBUFFER entry-to-entry interval ----
// Intervals between consecutive EXECBUFFER ioctls (any fd, glmark2 + weston).
// Buckets in ms: 0.5, 1, 2, 3, 4, 6, 10, inf. Pure atomics — no per-frame log.
static EXECB_GAP_BUCKETS: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static LAST_EXECB_TS: AtomicU64 = AtomicU64::new(0);
const EXECB_GAP_BOUNDS_MS: [f64; 7] = [0.5, 1.0, 2.0, 3.0, 4.0, 6.0, 10.0];

fn record_execb_gap() {
    let now = monotonic_time().as_nanos() as u64;
    let last = LAST_EXECB_TS.swap(now, Ordering::Relaxed);
    if last == 0 {
        return;
    }
    let gap_ms = (now.saturating_sub(last)) as f64 / 1e6;
    let idx = match EXECB_GAP_BOUNDS_MS.iter().position(|b| gap_ms < *b) {
        Some(i) => i,
        None => 7,
    };
    EXECB_GAP_BUCKETS[idx].fetch_add(1, Ordering::Relaxed);
}

// ---- Step-0 #2 / #1-a forensic counters (reset per perf_report window) ----
// EXECBUFFER fence-mode selection Mesa actually sends. Only *counts*; full
// per-submit detail is in the first-CREATE_DETAIL_LIMIT create logs.
static EXECB_FENCE_FD_IN: AtomicU64 = AtomicU64::new(0);
static EXECB_FENCE_FD_OUT: AtomicU64 = AtomicU64::new(0);
static EXECB_SYNC_IN: AtomicU64 = AtomicU64::new(0);
static EXECB_SYNC_OUT: AtomicU64 = AtomicU64::new(0);
/// VIRTGPU_WAIT called with `VIRTGPU_WAIT_NOWAIT` (a poll, not a block).
static WAIT_NOWAIT: AtomicU64 = AtomicU64::new(0);
/// VIRTGPU_WAIT(NOWAIT) probes that returned EBUSY — the host batch was still
/// in flight. Verifies the honest-fence path is actually exercised.
static WAIT_BUSY: AtomicU64 = AtomicU64::new(0);
/// Number of VIRTGPU_RESOURCE_CREATE details logged so far (gate the log).
static CREATE_DETAIL_N: AtomicU64 = AtomicU64::new(0);
const CREATE_DETAIL_LIMIT: u64 = 200;
/// [card0:fx] tiny-resource content dump gate (per-frame 8x1 forensics).
static SMALL_RES_DUMP_N: AtomicU64 = AtomicU64::new(0);
const SMALL_RES_DUMP_LIMIT: u64 = 400;
/// Upper bound on `DIRTY_FB` clips we merge per call. clamps a garbage
/// `num_clips` so `vm_load` can't allocate unbounded kernel memory.
const MAX_DIRTY_CLIPS: u32 = 64;

// ==== ioctl-stream forensics (run6d+): where does the per-frame gap live? ====
// Every card0 ioctl arrival is recorded into a ring and the idle gap until the
// *next* ioctl is bucketed per ioctl type. Combined with the EXECB gap
// histogram this attributes the ~2-4ms guest frame silence to the ioctl (or
// userspace work) that precedes it. Pure atomics — no per-ioctl log line.
const IOCTL_SLOT_N: usize = 10;
const SLOT_EXECB: usize = 0;
const SLOT_WAIT: usize = 1;
const SLOT_RESCREATE: usize = 2;
const SLOT_CREATE_DUMB: usize = 3;
const SLOT_MAP_DUMB: usize = 4;
const SLOT_DESTROY_DUMB: usize = 5;
const SLOT_GEM_CLOSE: usize = 6;
const SLOT_ATOMIC: usize = 7;
const SLOT_TRANS3D: usize = 8;
const SLOT_OTHER: usize = 9;
const IOCTL_SLOT_NAMES: [&str; IOCTL_SLOT_N] = [
    "execbuf",
    "wait",
    "res-create",
    "create-dumb",
    "map-dumb",
    "destroy-dumb",
    "gem-close",
    "atomic",
    "trans3d",
    "other",
];

fn ioctl_slot_of(cmd: u32) -> usize {
    match cmd {
        DRM_IOCTL_VIRTGPU_EXECBUFFER => SLOT_EXECB,
        DRM_IOCTL_VIRTGPU_WAIT => SLOT_WAIT,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE => SLOT_RESCREATE,
        DRM_IOCTL_MODE_CREATE_DUMB => SLOT_CREATE_DUMB,
        DRM_IOCTL_MODE_MAP_DUMB => SLOT_MAP_DUMB,
        DRM_IOCTL_MODE_DESTROY_DUMB => SLOT_DESTROY_DUMB,
        DRM_IOCTL_GEM_CLOSE => SLOT_GEM_CLOSE,
        DRM_IOCTL_MODE_ATOMIC => SLOT_ATOMIC,
        DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST => SLOT_TRANS3D,
        _ => SLOT_OTHER,
    }
}

struct IoctlGapSlot {
    buckets: [AtomicU64; 8],
    cnt: AtomicU64,
}

impl IoctlGapSlot {
    const fn new() -> Self {
        Self {
            buckets: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            cnt: AtomicU64::new(0),
        }
    }
}

static IOCTL_GAP_AFTER: [IoctlGapSlot; IOCTL_SLOT_N] = [
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
    IoctlGapSlot::new(),
];
static LAST_IOCTL_TS: AtomicU64 = AtomicU64::new(0);
static LAST_IOCTL_SLOT: AtomicU64 = AtomicU64::new(0);

const IOCTL_RING_N: usize = 128;
// Parallel atomic arrays (a plain `static [T; N]` can't be written). Each
// entry packs (pid<<32 | cmd) so the ring stays race-safe under SMP ioctls;
// torn reads only degrade a diagnostic dump, never correctness.
static IOCTL_RING_T: [AtomicU64; IOCTL_RING_N] = [const { AtomicU64::new(0) }; IOCTL_RING_N];
static IOCTL_RING_V: [AtomicU64; IOCTL_RING_N] = [const { AtomicU64::new(0) }; IOCTL_RING_N];
static RING_HEAD: AtomicUsize = AtomicUsize::new(0);
static RING_LAST_WRITTEN: AtomicUsize = AtomicUsize::new(0);
static RING_LAST_EXECB: AtomicUsize = AtomicUsize::new(usize::MAX);
static RING_LAST_EXECB_NONW: AtomicUsize = AtomicUsize::new(usize::MAX);
static RING_DUMPED: AtomicU64 = AtomicU64::new(0);
const RING_DUMP_LIMIT: u64 = 2;

fn record_ioctl_arrival(cmd: u32, pid: u32) {
    let now = monotonic_time().as_nanos() as u64;
    // Gap since the previous ioctl is charged to that previous ioctl's slot
    // ("gap-after"): a long silence shows which ioctl boundary it follows.
    let last_ts = LAST_IOCTL_TS.swap(now, Ordering::Relaxed);
    if last_ts != 0 {
        let gap_us = now.saturating_sub(last_ts) / 1_000;
        let slot = (LAST_IOCTL_SLOT.load(Ordering::Relaxed) as usize).min(IOCTL_SLOT_N - 1);
        let idx = EXECB_GAP_BOUNDS_MS
            .iter()
            .position(|b| (gap_us as f64) / 1e3 < *b)
            .unwrap_or(7);
        let s = &IOCTL_GAP_AFTER[slot];
        s.buckets[idx].fetch_add(1, Ordering::Relaxed);
        s.cnt.fetch_add(1, Ordering::Relaxed);
    }
    LAST_IOCTL_SLOT.store(ioctl_slot_of(cmd) as u64, Ordering::Relaxed);
    let head = RING_HEAD.load(Ordering::Relaxed);
    IOCTL_RING_T[head].store(now / 1_000, Ordering::Relaxed);
    IOCTL_RING_V[head].store(((pid as u64) << 32) | cmd as u64, Ordering::Relaxed);
    RING_LAST_WRITTEN.store(head, Ordering::Relaxed);
    RING_HEAD.store((head + 1) % IOCTL_RING_N, Ordering::Relaxed);
}

// ---- sub-slot timing: where do the per-call µs go inside the hot handlers? ----
fn timed<T>(sum: &'static AtomicU64, cnt: &'static AtomicU64, f: impl FnOnce() -> T) -> T {
    let t0 = monotonic_time();
    let r = f();
    let d = monotonic_time().saturating_sub(t0).as_nanos() as u64;
    sum.fetch_add(d, Ordering::Relaxed);
    cnt.fetch_add(1, Ordering::Relaxed);
    r
}

// VIRTGPU_RESOURCE_CREATE (the per-frame 8x1 fence resource) internals.
static RC_ALLOC_SUM: AtomicU64 = AtomicU64::new(0); // guest backing page alloc + zero
static RC_ALLOC_CNT: AtomicU64 = AtomicU64::new(0);
static RC_PAGE_SUM: AtomicU64 = AtomicU64::new(0); // backing pages allocated
static RC_ENQ_SUM: AtomicU64 = AtomicU64::new(0); // create_3d + attach_backing enqueues
static RC_ENQ_CNT: AtomicU64 = AtomicU64::new(0);
static RC_CTXA_SUM: AtomicU64 = AtomicU64::new(0); // ctx_attach enqueue
static RC_CTXA_CNT: AtomicU64 = AtomicU64::new(0);
static RC_NOTIFY_SUM: AtomicU64 = AtomicU64::new(0); // ctrl_notify (kick)
static RC_NOTIFY_CNT: AtomicU64 = AtomicU64::new(0);
// GEM_CLOSE internals (per-frame fence-handle close): the host-unref enqueue
// vs the rest (O(n) gpu_resources scan + lock juggling + small-res dump).
static GC_UNREF_SUM: AtomicU64 = AtomicU64::new(0);
static GC_UNREF_CNT: AtomicU64 = AtomicU64::new(0);
// CREATE_DUMB internals (per-frame 4MB wl_drm buffer hypothesis).
static DUMB_CNT: AtomicU64 = AtomicU64::new(0);
static DUMB_SIZE_SUM: AtomicU64 = AtomicU64::new(0);
static DUMB_ALLOC_SUM: AtomicU64 = AtomicU64::new(0);
static DUMB_ZERO_SUM: AtomicU64 = AtomicU64::new(0);
static DUMB_ENQ_SUM: AtomicU64 = AtomicU64::new(0);

fn perf_measure<T>(slot: &'static PerfSlot, f: impl FnOnce() -> T) -> T {
    let t0 = monotonic_time();
    let r = f();
    slot.add(monotonic_time().saturating_sub(t0));
    r
}

fn perf_report(card: &Card0) {
    let n = PERF_ATOMIC.cnt.load(Ordering::Relaxed);
    if n == 0 || !n.is_multiple_of(PERF_REPORT_EVERY) {
        return;
    }
    macro_rules! line {
        ($name:literal, $slot:expr) => {{
            let c = $slot.cnt.load(Ordering::Relaxed);
            if c > 0 {
                let sum = $slot.sum_ns.load(Ordering::Relaxed);
                let max = $slot.max_ns.load(Ordering::Relaxed);
                warn!(
                    "  [card0:perf] {:<18} n={:>8} avg={:>8.2}us max={:>8.2}us",
                    $name,
                    c,
                    sum as f64 / c as f64 / 1e3,
                    max as f64 / 1e3,
                );
            }
        }};
    }
    warn!(
        "[card0:perf] present-path guest-kernel slice (report #{n}, at {} ATOMIC commits):",
        PERF_REPORT_EVERY
    );
    line!("atomic(handler)", PERF_ATOMIC);
    line!("scanout(enq)", PERF_SCANOUT);
    line!("transfer2d(enq)", PERF_TRANSFER);
    line!("flush(enq)", PERF_FLUSH);
    line!("execbuffer", PERF_EXECBUF);
    line!("wait", PERF_WAIT);
    line!("transfer3d", PERF_TRANS3D);
    line!("res-create", PERF_RESCREATE);
    line!("create-dumb", PERF_DUMBCREATE);
    line!("map-dumb", PERF_DUMMAP);
    line!("destroy-dumb", PERF_DUMDESTROY);
    line!("gem-close", PERF_GEMCLOSE);

    // ---- Step-0 #2 / #1-a forensic window ----
    // EXECBUFFER fence-mode distribution. Delta vs PERF_EXECBUF.cnt = plain
    // (no fence) submits.
    let execb = PERF_EXECBUF.cnt.load(Ordering::Relaxed);
    let fin = EXECB_FENCE_FD_IN.load(Ordering::Relaxed);
    let fout = EXECB_FENCE_FD_OUT.load(Ordering::Relaxed);
    let sin = EXECB_SYNC_IN.load(Ordering::Relaxed);
    let sout = EXECB_SYNC_OUT.load(Ordering::Relaxed);
    let wait_calls = PERF_WAIT.cnt.load(Ordering::Relaxed);
    let wait_nowait = WAIT_NOWAIT.load(Ordering::Relaxed);
    let wait_busy = WAIT_BUSY.load(Ordering::Relaxed);
    warn!(
        "  [card0:fx] execbuffer n={execb}: fence-fd-in={fin} fence-fd-out={fout} sync-in={sin} \
         sync-out={sout} plain={}",
        execb.saturating_sub(fin + fout + sin + sout)
    );
    warn!(
        "  [card0:fx] wait n={wait_calls} nowait={wait_nowait} busy={wait_busy} unique-handles={}",
        card.wait_handles.lock().len()
    );

    // ---- run6 frame-gap buckets (EXECBUFFER entry-to-entry, ms) ----
    let gaps = &EXECB_GAP_BUCKETS;
    let gap_total: u64 = gaps.iter().map(|b| b.load(Ordering::Relaxed)).sum();
    if gap_total > 0 {
        warn!(
            "  [card0:fx] execb-gap n={gap_total} ms:<0.5={} 0.5-1={} 1-2={} 2-3={} 3-4={} 4-6={} \
             6-10={} >10={}",
            gaps[0].load(Ordering::Relaxed),
            gaps[1].load(Ordering::Relaxed),
            gaps[2].load(Ordering::Relaxed),
            gaps[3].load(Ordering::Relaxed),
            gaps[4].load(Ordering::Relaxed),
            gaps[5].load(Ordering::Relaxed),
            gaps[6].load(Ordering::Relaxed),
            gaps[7].load(Ordering::Relaxed),
        );
    }
    // Spin before the global display lock (axdisplay `lock_display`): the
    // cross-vCPU serialization cost of every gpu3d_* entry. The delta between
    // this and the [card0:perf] ioctl slices is the in-critical-section cost
    // (enqueue + host-wait spinning).
    let (lcnt, lsum, lmax) = ax_display::gpu3d_lock_wait_stats();
    ax_display::gpu3d_lock_wait_reset();
    warn!(
        "  [card0:fx] display-lock-wait n={lcnt} avg={:.2}us max={:.2}us",
        lsum as f64 / lcnt.max(1) as f64 / 1e3,
        lmax as f64 / 1e3,
    );
    let geoms = card.rescreate_geoms.lock();
    warn!(
        "  [card0:fx] res-create unique-geoms={} (distinct w×h this window)",
        geoms.len()
    );
    drop(geoms);
    let by_pid = card.create_by_pid.lock();
    if !by_pid.is_empty() {
        let mut items: Vec<_> = by_pid
            .iter()
            .map(|(pid, (n, exe))| {
                let exe = String::from_utf8_lossy(exe);
                format!("{pid}({n},{exe})")
            })
            .collect();
        items.sort();
        warn!("  [card0:fx] res-create by pid: {}", items.join(" "));
    }
    drop(by_pid);
    let pres_pid = card.present_created_pid.lock();
    if !pres_pid.is_empty() {
        let mut items: Vec<_> = pres_pid.iter().map(|(p, c)| format!("{p}({c})")).collect();
        items.sort();
        warn!(
            "  [card0:fx] present scanout by creator-pid: {}",
            items.join(" ")
        );
    }
    drop(pres_pid);

    // ---- ioctl-stream: gap-after per ioctl type (which boundary is the
    // long guest-side silence on?) + last EXECBUFFER-bounded frame dump ----
    for (i, s) in IOCTL_GAP_AFTER.iter().enumerate() {
        let cnt = s.cnt.load(Ordering::Relaxed);
        if cnt == 0 {
            continue;
        }
        let b: Vec<u64> = s
            .buckets
            .iter()
            .map(|x| x.load(Ordering::Relaxed))
            .collect();
        warn!(
            "  [card0:fx] gap-after[{:>12}] n={} ms:<0.5={} 0.5-1={} 1-2={} 2-3={} 3-4={} 4-6={} \
             6-10={} >10={}",
            IOCTL_SLOT_NAMES[i], cnt, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]
        );
    }
    // ---- poll wake-to-return latency (run6g): do_poll waited, then the peer
    // unix-stream send woke it; the delta is the wakeup+return path cost. ----
    let pwl_cnt = crate::syscall::POLL_WAKE_LAT_CNT.load(Ordering::Relaxed);
    if pwl_cnt > 0 {
        let pwl = &crate::syscall::POLL_WAKE_LAT;
        warn!(
            "  [card0:fx] poll-wake-lat n={pwl_cnt} us:<100={} 100-500={} 500-1000={} 1-2m={} \
             2-4m={} >4m={}",
            pwl[0].load(Ordering::Relaxed),
            pwl[1].load(Ordering::Relaxed),
            pwl[2].load(Ordering::Relaxed),
            pwl[3].load(Ordering::Relaxed),
            pwl[4].load(Ordering::Relaxed),
            pwl[5].load(Ordering::Relaxed),
        );
    }
    // ---- scheduler wake->run latency (run6h): unblock -> actually running. ----
    let (wrl, wrl_cnt) = ax_task::wake_run_lat_snapshot();
    if wrl_cnt > 0 {
        warn!(
            "  [card0:fx] wake-run-lat n={wrl_cnt} us:<100={} 100-500={} 500-1000={} 1-2m={} \
             2-4m={} >4m={}",
            wrl[0], wrl[1], wrl[2], wrl[3], wrl[4], wrl[5],
        );
    }
    // ---- fence refresher wake-ups per window (gated tick: ~20/s idle,
    // ~1ms service only while a guest is poll-blocked on an out-fence). ----
    let frt = super::sync_file::REFRESH_TICKS.load(Ordering::Relaxed);
    if frt > 0 {
        warn!("  [card0:fx] fence-refresh ticks n={frt}");
    }
    if RING_DUMPED.load(Ordering::Relaxed) < RING_DUMP_LIMIT {
        let nonw = RING_LAST_EXECB_NONW.load(Ordering::Relaxed);
        let last_execb = if nonw != usize::MAX {
            nonw
        } else {
            RING_LAST_EXECB.load(Ordering::Relaxed)
        };
        let head = RING_HEAD.load(Ordering::Relaxed);
        if last_execb != usize::MAX && last_execb != head {
            RING_DUMPED.fetch_add(1, Ordering::Relaxed);
            let t0 = IOCTL_RING_T[last_execb].load(Ordering::Relaxed);
            let mut i = last_execb;
            let mut n = 0;
            warn!("  [card0:ring] ioctl sequence since last EXECBUFFER:");
            while i != head && n < IOCTL_RING_N {
                let t = IOCTL_RING_T[i].load(Ordering::Relaxed);
                let v = IOCTL_RING_V[i].load(Ordering::Relaxed);
                let pid = (v >> 32) as u32;
                let cmd = v as u32;
                warn!(
                    "  [card0:ring]   +{:>8}us pid={:<4} cmd=0x{:08x}",
                    t.saturating_sub(t0),
                    pid,
                    cmd
                );
                i = (i + 1) % IOCTL_RING_N;
                n += 1;
            }
        }
    }
    // ---- hot-handler internals: where do the per-call µs go? ----
    let rcnt = RC_ALLOC_CNT.load(Ordering::Relaxed);
    if rcnt > 0 {
        let alloc = RC_ALLOC_SUM.load(Ordering::Relaxed) as f64 / rcnt as f64 / 1e3;
        let enq = RC_ENQ_SUM.load(Ordering::Relaxed) as f64
            / RC_ENQ_CNT.load(Ordering::Relaxed).max(1) as f64
            / 1e3;
        let ctxa = RC_CTXA_SUM.load(Ordering::Relaxed) as f64
            / RC_CTXA_CNT.load(Ordering::Relaxed).max(1) as f64
            / 1e3;
        let notify = RC_NOTIFY_SUM.load(Ordering::Relaxed) as f64
            / RC_NOTIFY_CNT.load(Ordering::Relaxed).max(1) as f64
            / 1e3;
        let other = (PERF_RESCREATE.sum_ns.load(Ordering::Relaxed) as f64 / rcnt as f64 / 1e3)
            - alloc
            - enq
            - ctxa
            - notify;
        warn!(
            "  [card0:fx] res-create internals n={rcnt} alloc={alloc:.1}us enq={enq:.1}us \
             ctxa={ctxa:.1}us notify={notify:.1}us other={other:.1}us pages/call={:.2}",
            RC_PAGE_SUM.load(Ordering::Relaxed) as f64 / rcnt as f64,
        );
    }
    let gc_cnt = PERF_GEMCLOSE.cnt.load(Ordering::Relaxed);
    if gc_cnt > 0 {
        let unref = GC_UNREF_SUM.load(Ordering::Relaxed) as f64
            / GC_UNREF_CNT.load(Ordering::Relaxed).max(1) as f64
            / 1e3;
        let gc_other =
            (PERF_GEMCLOSE.sum_ns.load(Ordering::Relaxed) as f64 / gc_cnt as f64 / 1e3) - unref;
        warn!(
            "  [card0:fx] gem-close internals n={gc_cnt} unref={unref:.1}us other={gc_other:.1}us"
        );
    }
    let dcnt = DUMB_CNT.load(Ordering::Relaxed);
    if dcnt > 0 {
        warn!(
            "  [card0:fx] create-dumb n={dcnt} alloc={:.1}us zero={:.1}us enq={:.1}us \
             size-avg={:.0}KB",
            DUMB_ALLOC_SUM.load(Ordering::Relaxed) as f64 / dcnt as f64 / 1e3,
            DUMB_ZERO_SUM.load(Ordering::Relaxed) as f64 / dcnt as f64 / 1e3,
            DUMB_ENQ_SUM.load(Ordering::Relaxed) as f64 / dcnt as f64 / 1e3,
            DUMB_SIZE_SUM.load(Ordering::Relaxed) as f64 / dcnt as f64 / 1024.0,
        );
    }

    // Zero the counters so each report is its own window.
    let zero = |slot: &'static PerfSlot| {
        slot.cnt.store(0, Ordering::Relaxed);
        slot.sum_ns.store(0, Ordering::Relaxed);
        slot.max_ns.store(0, Ordering::Relaxed);
    };
    zero(&PERF_ATOMIC);
    zero(&PERF_SCANOUT);
    zero(&PERF_TRANSFER);
    zero(&PERF_FLUSH);
    zero(&PERF_EXECBUF);
    zero(&PERF_WAIT);
    zero(&PERF_TRANS3D);
    zero(&PERF_RESCREATE);
    zero(&PERF_DUMBCREATE);
    zero(&PERF_DUMMAP);
    zero(&PERF_DUMDESTROY);
    zero(&PERF_GEMCLOSE);
    EXECB_FENCE_FD_IN.store(0, Ordering::Relaxed);
    EXECB_FENCE_FD_OUT.store(0, Ordering::Relaxed);
    EXECB_SYNC_IN.store(0, Ordering::Relaxed);
    EXECB_SYNC_OUT.store(0, Ordering::Relaxed);
    WAIT_NOWAIT.store(0, Ordering::Relaxed);
    WAIT_BUSY.store(0, Ordering::Relaxed);
    for b in EXECB_GAP_BUCKETS.iter() {
        b.store(0, Ordering::Relaxed);
    }
    for s in IOCTL_GAP_AFTER.iter() {
        s.cnt.store(0, Ordering::Relaxed);
        for b in s.buckets.iter() {
            b.store(0, Ordering::Relaxed);
        }
    }
    LAST_IOCTL_TS.store(0, Ordering::Relaxed);
    LAST_IOCTL_SLOT.store(0, Ordering::Relaxed);
    RING_DUMPED.store(0, Ordering::Relaxed);
    for b in crate::syscall::POLL_WAKE_LAT.iter() {
        b.store(0, Ordering::Relaxed);
    }
    crate::syscall::POLL_WAKE_LAT_CNT.store(0, Ordering::Relaxed);
    ax_task::wake_run_lat_reset();
    super::sync_file::REFRESH_TICKS.store(0, Ordering::Relaxed);
    RC_ALLOC_SUM.store(0, Ordering::Relaxed);
    RC_ALLOC_CNT.store(0, Ordering::Relaxed);
    RC_PAGE_SUM.store(0, Ordering::Relaxed);
    RC_ENQ_SUM.store(0, Ordering::Relaxed);
    RC_ENQ_CNT.store(0, Ordering::Relaxed);
    RC_CTXA_SUM.store(0, Ordering::Relaxed);
    RC_CTXA_CNT.store(0, Ordering::Relaxed);
    RC_NOTIFY_SUM.store(0, Ordering::Relaxed);
    RC_NOTIFY_CNT.store(0, Ordering::Relaxed);
    GC_UNREF_SUM.store(0, Ordering::Relaxed);
    GC_UNREF_CNT.store(0, Ordering::Relaxed);
    DUMB_CNT.store(0, Ordering::Relaxed);
    DUMB_SIZE_SUM.store(0, Ordering::Relaxed);
    DUMB_ALLOC_SUM.store(0, Ordering::Relaxed);
    DUMB_ZERO_SUM.store(0, Ordering::Relaxed);
    DUMB_ENQ_SUM.store(0, Ordering::Relaxed);
    card.create_by_pid.lock().clear();
    card.wait_handles.lock().clear();
    card.rescreate_geoms.lock().clear();
    card.present_created_pid.lock().clear();
}

pub struct Card0 {
    /// Queue of pending DRM events waiting to be delivered via `read()`.
    events: Mutex<VecDeque<DrmEventVblank>>,
    /// Wakes up `poll`-waiters blocked on `read()` when a new event
    /// arrives.
    poll_rx: PollSet,
    /// Monotonically-increasing vblank sequence.
    sequence: AtomicU32,
    /// Current values of all atomic-tunable properties.
    state: Mutex<ModesetState>,
    /// Legacy `SETCRTC` binding readable via `GETCRTC`. Atomic commits
    /// don't update this — userspace that mixes legacy and atomic gets
    /// the well-defined "legacy state reflects the last SETCRTC"
    /// behavior libdrm expects.
    legacy_crtc: Mutex<LegacyCrtcState>,
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
    /// Next fb id to hand out.
    next_fb_id: AtomicU32,
    /// User-created `CREATEPROPBLOB` blobs keyed by their blob_id.
    /// Distinct from `system_blobs` so DESTROY_BLOB cannot remove
    /// kernel-owned blobs (e.g. `IN_FORMATS`). Stored behind `Arc`
    /// so committed modeset state (see [`Self::mode_id_blob_ref`]) can
    /// hold its own backing reference past a user `DESTROYPROPBLOB`.
    blobs: Mutex<BTreeMap<u32, Arc<Vec<u8>>>>,
    /// Strong reference to the blob backing the currently-committed
    /// `MODE_ID` property. Linux DRM pins the mode blob from the CRTC
    /// state, so a user `DESTROYPROPBLOB` on the publish handle only
    /// drops the user's reference — `GETPROPBLOB` on the same id keeps
    /// working until a later atomic commit replaces or clears
    /// `MODE_ID`. Cleared/replaced atomically with `state.crtc_mode_id`.
    mode_id_blob_ref: Mutex<Option<Arc<Vec<u8>>>>,
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
    /// Registered virtio-gpu IRQ action, when the display backend advertises one.
    irq_handle: ax_lazyinit::OnceLock<ax_runtime::hal::irq::IrqHandle>,

    // ---- 3D (virgl) resource management ----
    /// 3D resources keyed by virtio-gpu resource ID. Each resource tracks
    /// its associated GEM handle, geometry, and size for transfer validation.
    gpu_resources: Mutex<BTreeMap<u32, GpuResource>>,
    /// Imported blob dma-bufs: GEM handle (from `PRIME_FD_TO_HANDLE`) →
    /// host resource info. A same-device import is the same host resource
    /// the exporter created, so `RESOURCE_INFO` on an imported handle
    /// resolves back to the original `res_handle` + `blob_mem`.
    ///
    /// Each entry holds one reference on the host resource for the importer.
    blob_aliases: Mutex<BTreeMap<u32, ImportedBlob>>,
    /// Currently bound scanout resource + geometry, for Linux-style
    /// bind-on-change `SET_SCANOUT` in [`Card::present_fb`]. `None` means
    /// nothing bound yet — the first present forces a bind. Linux gates
    /// `SET_SCANOUT` on fb/src/modeset change (`virtio_gpu_primary_plane_update`),
    /// not on every frame.
    scanout_bind: Mutex<Option<(u32, u32, u32)>>,
    /// Number of open PRIME-export dma-buf fds per host resource
    /// (`res_handle` → open-fd count). Each fd holds one reference on the
    /// host resource — Linux GEM lifetime says a dma-buf keeps the GEM
    /// object (and thus the host resource) alive until the last fd closes.
    dma_buf_refs: Mutex<BTreeMap<u32, u32>>,
    /// Weak reference to this card, handed to exported dma-buf fds so their
    /// [`Drop`] can release the host-resource reference they hold on close.
    self_weak: Weak<Card0>,
    /// Next virtio-gpu resource ID to allocate. Starts at 1 (0 is reserved).
    next_res_handle: AtomicU32,
    /// Next virgl context ID. Linux: `atomic_inc_return(&vgdev->ctx_id_cursor)`.
    /// Each CONTEXT_INIT call gets a unique id so multiple fds/clients
    /// don't share (and corrupt) the same virgl context state.
    next_ctx_id: AtomicU32,
    /// Cached capset data keyed by (capset_id, version). GET_CAPS results
    /// are cached here so repeated queries don't round-trip to the host.
    capset_cache: Mutex<BTreeMap<(u32, u32), Vec<u8>>>,
    /// Per-process virgl context state, mirroring Linux's per-fd
    /// `struct virtio_gpu_fpriv` (`virtgpu_ioctl.c`). `card0` and
    /// `renderD128` are the *same* pseudo-device node shared by every
    /// opener, so the fd identity that Linux keys contexts on is not
    /// visible here — key by the owning process instead (one render fd
    /// per process is the norm for Mesa clients). Without this, two
    /// clients (e.g. weston compositor + glmark2) both end up in the
    /// same host virgl context: their sub-ctx ids collide (both start
    /// at 1) and cso/sampler objects created by one are looked up in
    /// the other's sub-ctx → "Illegal handle" → context in_error.
    ///
    /// Context IDs themselves are allocated from the device-wide
    /// [`Card0::next_ctx_id`] so each CONTEXT_INIT gets a unique id —
    /// same as Linux's `atomic_inc_return(&vgdev->ctx_id_cursor)`.
    process_ctxs: Mutex<BTreeMap<u32, PerFdCtx>>,

    // ---- Step-0 #2 forensic: per-window histograms (reset by perf_report) ----
    /// VIRTGPU_RESOURCE_CREATE producers: pid → (count this window, executable
    /// suffix ≤ 64B). Answers "who allocates a buffer per frame — Mesa app
    /// (glmark2) or the compositor (weston)".
    create_by_pid: Mutex<BTreeMap<u32, (u64, Vec<u8>)>>,
    /// Distinct VIRTGPU_WAIT handles observed this window. Unique-vs-total
    /// reveals whether Mesa waits on a handful of reused buffers or one per
    /// batch.
    wait_handles: Mutex<BTreeSet<u32>>,
    /// Distinct (width,height) of resources created this window. Constant
    /// geometry = a cache-missed reusable buffer; varying = per-object
    /// allocation.
    rescreate_geoms: Mutex<BTreeSet<(u32, u32)>>,
    /// PID of the process whose resource each present_fb scanout is bound to,
    /// counted per window — correlates the per-frame creates with the
    /// presented (scanout) buffer ownership.
    present_created_pid: Mutex<BTreeMap<u32, u64>>,

    /// bo_handle → fence ID of the last EXECBUFFER that referenced it.
    /// Powers the honest VIRTGPU_WAIT: each handle waits on the fence of the
    /// most recent submit (Linux per-object dma-resv, `virtio_gpu_wait_ioctl`).
    /// GEM handles are unique while alive (the whole card0 assumes bo_handle
    /// uniqueness across fds), and a stale entry for a recycled handle is
    /// harmless: fence IDs are monotonic, so an old value is already
    /// completed.
    bo_fence: Mutex<BTreeMap<u32, u64>>,
}

impl Card0 {
    pub fn new() -> Arc<Self> {
        let card = Arc::new_cyclic(|weak| Self {
            events: Mutex::new(VecDeque::with_capacity(MAX_EVENTS)),
            poll_rx: PollSet::new(),
            sequence: AtomicU32::new(0),
            state: Mutex::new(ModesetState::default()),
            legacy_crtc: Mutex::new(LegacyCrtcState::default()),
            dumbs: Mutex::new(BTreeMap::new()),
            next_dumb_handle: AtomicU32::new(FIRST_DUMB_HANDLE),
            // Start at STRIDE rather than 0 so a zero `offset` argument
            // on `mmap` is unambiguous (it means "hasn't called
            // MAP_DUMB yet").
            next_offset: AtomicU64::new(DUMB_BUFFER_OFFSET_STRIDE),
            fbs: Mutex::new(BTreeMap::new()),
            next_fb_id: AtomicU32::new(FIRST_FB_ID),
            blobs: Mutex::new(BTreeMap::new()),
            mode_id_blob_ref: Mutex::new(None),
            next_blob_id: AtomicU32::new(FIRST_BLOB_ID),
            system_blobs: Mutex::new(BTreeMap::new()),
            in_formats_blob: AtomicU32::new(0),
            system_blobs_init: Mutex::new(()),
            irq_handle: ax_lazyinit::OnceLock::new(),
            // 3D resource management
            gpu_resources: Mutex::new(BTreeMap::new()),
            blob_aliases: Mutex::new(BTreeMap::new()),
            scanout_bind: Mutex::new(None),
            dma_buf_refs: Mutex::new(BTreeMap::new()),
            next_res_handle: AtomicU32::new(1),
            next_ctx_id: AtomicU32::new(FIRST_VIRGL_CTX_ID),
            capset_cache: Mutex::new(BTreeMap::new()),
            process_ctxs: Mutex::new(BTreeMap::new()),
            create_by_pid: Mutex::new(BTreeMap::new()),
            wait_handles: Mutex::new(BTreeSet::new()),
            rescreate_geoms: Mutex::new(BTreeSet::new()),
            present_created_pid: Mutex::new(BTreeMap::new()),
            bo_fence: Mutex::new(BTreeMap::new()),
            self_weak: weak.clone(),
        });
        card.register_irq();
        card
    }

    /// PID of the process currently executing this ioctl. `None` in
    /// kernel-only context (no user thread).
    fn current_pid(&self) -> Option<u32> {
        current_may_uninit().map(|cur| u32::from(cur.as_thread().proc_data.proc.pid()))
    }

    /// Look up the virgl context assigned to the calling process, or
    /// `None` if CONTEXT_INIT was never called for it (Linux: an
    /// EXECBUFFER/RESOURCE_CREATE before `virtio_gpu_create_context`
    /// implicitly creates the context; here the implicit path is
    /// CONTEXT_INIT, which Mesa always issues first).
    fn current_ctx(&self) -> Option<u32> {
        let pid = self.current_pid()?;
        self.process_ctxs.lock().get(&pid).map(|c| c.ctx_id)
    }

    fn register_irq(self: &Arc<Self>) {
        if !ax_display::has_display() {
            return;
        }
        let Some(irq) = ax_display::framebuffer_irq_id() else {
            return;
        };

        let request = ax_runtime::hal::irq::IrqRequest::new(|_| {
            if ax_display::framebuffer_handle_irq() {
                // The IRQ may have pumped a fence completion; wake any poller
                // blocked on an out-fence whose host fence just fired (guest
                // fence waits are poll()-based; a blocked poll cannot re-check
                // the completion level by itself).
                super::sync_file::refresh_fence_waiters_from_irq();
                ax_runtime::hal::irq::IrqReturn::Handled
            } else {
                ax_runtime::hal::irq::IrqReturn::Unhandled
            }
        })
        .share_mode(ax_runtime::hal::irq::ShareMode::Shared)
        .auto_enable(ax_runtime::hal::irq::AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(irq, request) {
            Ok(handle) => {
                self.irq_handle.call_once(|| handle);
                ax_display::framebuffer_enable_irq();
                if let Some(handle) = self.irq_handle.get().copied()
                    && let Err(err) = ax_runtime::hal::irq::enable_irq(handle)
                {
                    warn!("failed to enable display irq handler for irq {irq:?}: {err:?}");
                    ax_display::framebuffer_disable_irq();
                }
            }
            Err(err) => {
                warn!("failed to register display irq handler for irq {irq:?}: {err:?}");
                ax_display::framebuffer_disable_irq();
            }
        }
    }

    /// Lazily construct the `IN_FORMATS` blob the first time a caller
    /// asks for plane properties. Holds `system_blobs_init` across the
    /// allocate-and-publish so a concurrent first-caller cannot leak
    /// a parallel copy into `system_blobs`. The blob lives there
    /// permanently — `handle_destroy_blob` refuses ids it covers.
    fn ensure_in_formats_blob(&self) -> u32 {
        let cur = self.in_formats_blob.load(Ordering::Acquire);
        if cur != 0 {
            return cur;
        }
        let _guard = self.system_blobs_init.lock();
        let cur = self.in_formats_blob.load(Ordering::Acquire);
        if cur != 0 {
            return cur;
        }
        let bytes = build_in_formats_blob();
        let id = self.next_blob_id.fetch_add(1, Ordering::Relaxed);
        self.system_blobs.lock().insert(id, Arc::new(bytes));
        self.in_formats_blob.store(id, Ordering::Release);
        id
    }
}

/// Write a kernel-owned `src` into a user buffer. Returns the number of
/// bytes the kernel tried to write (for the truncated-write `*_len =
/// len(src)` convention DRM's VERSION ioctl uses).
fn write_user_string(user_ptr: u64, user_cap: usize, src: &str) -> VfsResult<usize> {
    let n = user_cap.min(src.len());
    if n > 0 {
        vm_write_slice(user_ptr as *mut u8, &src.as_bytes()[..n])
            .map_err(|_| VfsError::BadAddress)?;
    }
    Ok(src.len())
}

/// Write up to `cap` `T`s from `src` into `user_ptr`; returns the total
/// source length.
fn report_user_array<T: Copy>(user_ptr: u64, cap: u32, src: &[T]) -> VfsResult<u32> {
    if user_ptr != 0 {
        let to_write = (cap as usize).min(src.len());
        vm_write_slice(user_ptr as *mut T, &src[..to_write]).map_err(|_| VfsError::BadAddress)?;
    }
    Ok(src.len() as u32)
}

/// Fetch a (width, height) pair from `axdisplay`. If no display device
/// was probed, returns a tiny default so `MODE_GETRESOURCES`/
/// `GETCONNECTOR` still have something coherent to report.
fn display_resolution() -> (u32, u32) {
    if ax_display::has_display() {
        let info = ax_display::framebuffer_info();
        (info.width, info.height)
    } else {
        (640, 480)
    }
}

/// VESA CVT-RBv1 (Coordinated Video Timings, Reduced Blanking — 2003)
/// constants. virtio-gpu doesn't actually drive a scanout clock but
/// userspace mode-validators reject self-inconsistent modes, so we
/// synthesize plausible values from the real resolution.
const CVT_RB_HFRONT_PORCH: u16 = 48;
const CVT_RB_HSYNC_WIDTH: u16 = 32;
const CVT_RB_HBACK_PORCH: u16 = 80;
const CVT_RB_VFRONT_PORCH: u16 = 3;
const CVT_RB_VSYNC_WIDTH: u16 = 8;
const CVT_RB_VBACK_PORCH: u16 = 6;

/// Default output refresh rate.
const DEFAULT_VREFRESH: u32 = 60;

/// Synthesized mode matching the display's current resolution.
fn current_mode() -> DrmModeModeInfo {
    let (w, h) = display_resolution();
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

    let vrefresh: u32 = DEFAULT_VREFRESH;
    let clock = ((htotal as u32) * (vtotal as u32) * vrefresh) / 1000;

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
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let evsz = core::mem::size_of::<DrmEventVblank>();
        if buf.len() < evsz {
            return Err(VfsError::InvalidInput);
        }
        let mut events = self.events.lock();
        let mut written = 0;
        while written + evsz <= buf.len() {
            let Some(ev) = events.pop_front() else {
                break;
            };
            buf[written..written + evsz].copy_from_slice(bytes_of(&ev));
            written += evsz;
        }
        if written == 0 {
            Err(VfsError::WouldBlock)
        } else {
            Ok(written)
        }
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::BadFileDescriptor)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        // [card0:ring] ioctl-stream forensics: record every arrival (type +
        // pid) so perf_report can attribute the inter-ioctl gaps.
        let pid = self.current_pid().unwrap_or(0);
        record_ioctl_arrival(cmd, pid);
        info!("[card0] ioctl cmd=0x{:08x} arg=0x{:x}", cmd, arg);
        let result = match cmd {
            DRM_IOCTL_VERSION => handle_version(arg),
            DRM_IOCTL_GET_UNIQUE => handle_get_unique(arg),
            DRM_IOCTL_SET_VERSION => handle_set_version(arg),
            DRM_IOCTL_GET_CAP => handle_get_cap(arg),
            DRM_IOCTL_SET_CLIENT_CAP => handle_set_client_cap(arg),
            DRM_IOCTL_SET_MASTER | DRM_IOCTL_DROP_MASTER => Ok(0),

            DRM_IOCTL_MODE_GETRESOURCES => handle_get_resources(arg),
            DRM_IOCTL_MODE_GETCRTC => self.handle_get_crtc(arg),
            DRM_IOCTL_MODE_SETCRTC => self.handle_set_crtc(arg),
            DRM_IOCTL_MODE_GETENCODER => handle_get_encoder(arg),
            DRM_IOCTL_MODE_GETCONNECTOR => handle_get_connector(arg),
            DRM_IOCTL_MODE_ADDFB2 => self.handle_addfb2(arg),
            DRM_IOCTL_MODE_RMFB => self.handle_rmfb(arg),
            DRM_IOCTL_MODE_CREATE_DUMB => {
                perf_measure(&PERF_DUMBCREATE, || self.handle_create_dumb(arg))
            }
            DRM_IOCTL_MODE_MAP_DUMB => perf_measure(&PERF_DUMMAP, || self.handle_map_dumb(arg)),
            DRM_IOCTL_MODE_DESTROY_DUMB => {
                perf_measure(&PERF_DUMDESTROY, || self.handle_destroy_dumb(arg))
            }
            // GEM_CLOSE is the release path Mesa uses for virgl 3D/blob
            // resources (incl. PRIME imports). Linux funnels both GEM_CLOSE
            // and DESTROY_DUMB through `drm_gem_handle_delete`; we mirror
            // that by sharing one cleanup helper.
            DRM_IOCTL_GEM_CLOSE => perf_measure(&PERF_GEMCLOSE, || self.handle_gem_close(arg)),

            DRM_IOCTL_MODE_GETPLANERESOURCES => handle_get_plane_resources(arg),
            DRM_IOCTL_MODE_GETPLANE => handle_get_plane(arg),
            DRM_IOCTL_MODE_OBJ_GETPROPERTIES => self.handle_obj_get_properties(arg),
            DRM_IOCTL_MODE_GETPROPERTY => handle_get_property(arg),
            DRM_IOCTL_MODE_PAGE_FLIP => self.handle_page_flip(arg),
            DRM_IOCTL_WAIT_VBLANK => self.handle_wait_vblank(arg),

            DRM_IOCTL_MODE_ATOMIC => perf_measure(&PERF_ATOMIC, || self.handle_atomic(arg)),
            DRM_IOCTL_MODE_CREATEPROPBLOB => self.handle_create_blob(arg),
            DRM_IOCTL_MODE_DESTROYPROPBLOB => self.handle_destroy_blob(arg),
            DRM_IOCTL_MODE_GETPROPBLOB => self.handle_get_blob(arg),

            DRM_IOCTL_GET_MAGIC => handle_get_magic(arg),
            DRM_IOCTL_AUTH_MAGIC => handle_auth_magic(arg),
            DRM_IOCTL_MODE_DIRTYFB => self.handle_dirty_fb(arg),
            DRM_IOCTL_PRIME_HANDLE_TO_FD => self.handle_prime_handle_to_fd(arg),
            DRM_IOCTL_PRIME_FD_TO_HANDLE => self.handle_prime_fd_to_handle(arg),

            // ---- virtgpu 3D ioctls ----
            DRM_IOCTL_VIRTGPU_GETPARAM => self.handle_virtgpu_getparam(arg),
            DRM_IOCTL_VIRTGPU_CONTEXT_INIT => self.handle_virtgpu_context_init(arg),
            DRM_IOCTL_VIRTGPU_GET_CAPS => self.handle_virtgpu_get_caps(arg),
            DRM_IOCTL_VIRTGPU_RESOURCE_CREATE => {
                perf_measure(&PERF_RESCREATE, || self.handle_virtgpu_resource_create(arg))
            }
            DRM_IOCTL_VIRTGPU_RESOURCE_INFO => self.handle_virtgpu_resource_info(arg),
            DRM_IOCTL_VIRTGPU_MAP => self.handle_virtgpu_map(arg),
            DRM_IOCTL_VIRTGPU_EXECBUFFER => {
                record_execb_gap();
                // Ring marker: the last ring entry written is this EXECBUFFER —
                // perf_report dumps the ioctl sequence between two submits.
                // Keep both the generic marker and a non-weston (app) marker so
                // the dump can prefer glmark2 frames over weston's.
                RING_LAST_EXECB.store(RING_LAST_WRITTEN.load(Ordering::Relaxed), Ordering::Relaxed);
                if pid != 3 {
                    RING_LAST_EXECB_NONW
                        .store(RING_LAST_WRITTEN.load(Ordering::Relaxed), Ordering::Relaxed);
                }
                perf_measure(&PERF_EXECBUF, || self.handle_virtgpu_execbuffer(arg))
            }
            DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST => {
                perf_measure(&PERF_TRANS3D, || self.handle_virtgpu_transfer_to_host(arg))
            }
            DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST => self.handle_virtgpu_transfer_from_host(arg),
            DRM_IOCTL_VIRTGPU_WAIT => perf_measure(&PERF_WAIT, || self.handle_virtgpu_wait(arg)),
            DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB => self.handle_virtgpu_resource_create_blob(arg),

            _ => {
                warn!("[card0] unsupported ioctl cmd=0x{:08x}", cmd);
                Err(VfsError::OperationNotSupported)
            }
        };
        result
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        // `offset` is the key `MAP_DUMB` handed back for a specific
        // `CREATE_DUMB`. Look up the matching buffer, return its
        // per-buffer physical range, and hand a strong ref on the
        // backing pages back through the retainer slot. The resulting
        // VMA keeps those pages alive across DESTROY_DUMB, matching
        // Linux GEM refcount semantics.
        let dumbs = self.dumbs.lock();
        let Some(b) = dumbs.values().find(|b| b.offset == offset) else {
            return DeviceMmap::None;
        };
        let range = PhysAddrRange::from_start_size(
            virt_to_phys(b.pages.start_vaddr()),
            length.min(b.pages.size() as u64) as usize,
        );
        let retain: Arc<dyn Any + Send + Sync> = b.pages.clone();
        DeviceMmap::Physical(range, Some(retain))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

impl Pollable for Card0 {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, !self.events.lock().is_empty());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            // Registration happens from DRM file poll task context.
            unsafe { self.poll_rx.register(context.waker(), IoEvents::IN) };
        }
    }

    fn unregister(&self, waker: &core::task::Waker) {
        // Poll/epoll return: drop this waiter's stale waker so flip-event
        // wake_one always reaches the compositor's current drm poll instead of
        // a leftover entry (leftovers starve the real waiter until its
        // timeout — measured ~1.3ms flip delivery on-screen).
        unsafe {
            self.poll_rx.unregister(waker);
        }
    }
}

impl Card0 {
    /// Look up the dumb buffer behind a given `fb_id` and copy its
    /// contents into the axdisplay scanout, then trigger
    /// `framebuffer_flush`. Used by `SETCRTC`, `PAGE_FLIP`, and atomic
    /// commits — every path that userspace uses to "show this buffer
    /// now" routes through here. A follow-on PR will swap the memcpy
    /// for virtio-gpu zero-copy via `set_scanout` / `transfer_to_host`.
    fn present_fb(&self, fb_id: u32) {
        // Snapshot the fb out of the registry, then drop the lock so the
        // display calls below don't run with the map locked. Pages survive
        // a concurrent DESTROY_DUMB because the fb owns its own
        // Arc<GlobalPage> clone.
        let (fb, dirty) = {
            let mut fbs = self.fbs.lock();
            let fb = match fbs.get_mut(&fb_id) {
                Some(fb) => Framebuffer {
                    size: fb.size,
                    stride: fb.stride,
                    width: fb.width,
                    height: fb.height,
                    kind: fb.kind.clone(),
                    dirty: None,
                },
                None => return,
            };
            // Consume the damage recorded by DIRTY_FB (Linux:
            // damage_merged rect is what the plane update uploads). None =
            // no clip data for this present — fall back to the whole fb.
            let dirty = fbs.get_mut(&fb_id).unwrap().dirty.take();
            (fb, dirty)
        };

        match &fb.kind {
            // Guest-RAM dumb buffer: copy pixels into the virtio-gpu
            // framebuffer and flush. This is the 2D CPU path (verified by
            // the Qt Widgets Gallery test).
            FbBacking::Dumb { pages } => {
                if !ax_display::has_display() {
                    return;
                };
                let src = pages.start_vaddr().as_usize() as *const u8;
                let info = ax_display::framebuffer_info();
                let dst = info.fb_base_vaddr as *mut u8;

                if fb.stride != 0 && info.stride != 0 && fb.stride as usize != info.stride {
                    // Stride mismatch — copy row by row to avoid diagonal tearing.
                    let dst_limit = info.fb_size / info.stride.max(1);
                    let rows = (fb.height as usize).min(dst_limit);
                    let bytes_per_row = (fb.stride as usize).min(info.stride);
                    for row in 0..rows {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                src.add(row * fb.stride as usize),
                                dst.add(row * info.stride),
                                bytes_per_row,
                            );
                        }
                    }
                } else {
                    // Strides match (or one is unknown) — flat copy.
                    let copy = (fb.size as usize).min(info.fb_size);
                    unsafe {
                        core::ptr::copy_nonoverlapping(src, dst, copy);
                    }
                }
                let _ = ax_display::framebuffer_flush();
            }
            // Host-side resource (2D dumb or 3D virgl/blob): bind it as the
            // scanout and flush — Linux's `virtio_gpu_plane_atomic_update`
            // does the same. Guest-backed 2D resources additionally need a
            // TRANSFER_TO_HOST_2D (handled below); 3D virgl/blob resources
            // already hold host-side pixels and skip the transfer.
            FbBacking::Gpu3d {
                res_handle,
                is_dumb_2d,
            } => {
                if !ax_display::has_display() {
                    return;
                };
                // Step-0 #2 forensic: who owns the presented scanout buffer?
                // Counts by the resource's creator pid → correlates the
                // per-frame creates with the scanout-side ownership.
                let created_pid = self
                    .gpu_resources
                    .lock()
                    .get(res_handle)
                    .map(|r| r.created_pid);
                if let Some(p) = created_pid {
                    let mut m = self.present_created_pid.lock();
                    *m.entry(p).or_insert(0) += 1;
                }
                // Damage region this present must upload. `dirty` comes from
                // DIRTY_FB clips; nothing recorded ⇒ whole fb (Linux
                // drm_atomic_helper_damage_merged's no-clips fallback).
                let (x, y, w, h) = dirty.unwrap_or((0, 0, fb.width, fb.height));
                let w = w.min(fb.width.saturating_sub(x));
                let h = h.min(fb.height.saturating_sub(y));

                // Guest-backed 2D (dumb): TRANSFER_TO_HOST_2D the damaged
                // region only, and BEFORE binding scanout so the first
                // scanout shows pixels — QEMU only fills the host image on
                // TRANSFER_TO_HOST_2D. Matches Linux
                // `virtio_gpu_update_dumb_bo` + queue order in
                // `virtio_gpu_primary_plane_update` (transfer → scanout →
                // flush). 3D virgl/blob resources already hold host-side
                // pixels and skip this.
                if *is_dumb_2d {
                    let _ = perf_measure(&PERF_TRANSFER, || {
                        ax_display::gpu3d_transfer_to_host_2d(*res_handle, x, y, w, h)
                    });
                }

                // SET_SCANOUT only when the bound resource or geometry
                // changed — Linux gates scanout on fb/src/modeset change,
                // not per frame. First present always binds (scanout_bind
                // starts None).
                let bind = (*res_handle, fb.width, fb.height);
                if *self.scanout_bind.lock() != Some(bind)
                    && perf_measure(&PERF_SCANOUT, || {
                        ax_display::gpu3d_set_scanout(0, *res_handle, 0, 0, fb.width, fb.height)
                    })
                    .is_ok()
                {
                    *self.scanout_bind.lock() = Some(bind);
                }

                // Flush the damaged region, not the whole fb (Linux flushes
                // the damage_merged rect).
                let _ = perf_measure(&PERF_FLUSH, || {
                    ax_display::gpu3d_resource_flush(*res_handle, x, y, w, h)
                });
            }
        }
        // Present is a fire-and-forget transaction (transfer/scanout/flush);
        // close the command window with one notify — Linux's ioctl-boundary
        // `virtio_gpu_notify()` (vq.c:551). Without this the flush would sit
        // in the avail ring until some later sync command kicks.
        ax_display::gpu3d_ctrl_notify();
    }

    fn handle_create_dumb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCreateDumb;
        let mut c: DrmModeCreateDumb = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
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
        c.size = size;
        // Each buffer gets its own page-aligned `GlobalPage`. No shared
        // pool, so we don't fail on early-boot fragmentation on arches
        // whose allocator can't satisfy one large contiguous request
        // after driver probe.
        let size_aligned = (size as usize).next_multiple_of(PAGE_SIZE_4K);
        let pages = size_aligned / PAGE_SIZE_4K;
        DUMB_CNT.fetch_add(1, Ordering::Relaxed);
        DUMB_SIZE_SUM.fetch_add(size, Ordering::Relaxed);
        let mut backing = timed(&DUMB_ALLOC_SUM, &DUMB_CNT, || {
            GlobalPage::alloc_contiguous(pages, PAGE_SIZE_4K).map_err(|_| VfsError::NoMemory)
        })?;
        // Linux DRM dumb buffers must be returned zeroed: the page
        // allocator may hand back pages that previously held kernel
        // data, and we mmap them straight into user space.
        timed(&DUMB_ZERO_SUM, &DUMB_CNT, || backing.zero());
        let pages_arc = Arc::new(backing);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);

        // --- Create a host-side 2D resource + attach guest backing ---
        // Mirrors Linux `virtio_gpu_mode_dumb_create` (virtgpu_gem.c:61-100)
        // which calls RESOURCE_CREATE_2D + ATTACH_BACKING so the host knows
        // about the guest pages.  Without this, virgl blits to the dumb
        // buffer fail (no host resource) and present_fb reads zeros.
        if ax_display::has_display() {
            let res_handle = self.next_res_handle.fetch_add(1, Ordering::Relaxed);
            let paddr = virt_to_phys(pages_arc.start_vaddr());
            timed(&DUMB_ENQ_SUM, &DUMB_CNT, || {
                let _ = ax_display::gpu3d_resource_create_2d(res_handle, c.width, c.height);
                let _ = ax_display::gpu3d_attach_backing(
                    res_handle,
                    paddr.as_usize() as u64,
                    size as u32,
                );
            });
            // Also register in gpu_resources so ADDFB2 finds it as a
            // host-backed buffer (FbBacking::Gpu3d) instead of plain Dumb.
            self.gpu_resources.lock().insert(
                res_handle,
                GpuResource {
                    bo_handle: handle,
                    width: c.width,
                    height: c.height,
                    stride: pitch,
                    size,
                    // Dumb 2D scanout buffers are presented via
                    // TRANSFER_TO_HOST_2D + FLUSH, never through a context, so
                    // reserve attach-on-first-EXECBUFFER if one ever references it.
                    attached_ctxs: BTreeSet::new(),
                    blob_mem: 0,
                    blob_flags: 0,
                    is_dumb_2d: true,
                    created_pid: self.current_pid().unwrap_or(0),
                },
            );
        }

        self.dumbs.lock().insert(
            handle,
            DumbBuffer {
                width: c.width,
                height: c.height,
                bpp: c.bpp,
                pitch: c.pitch,
                size: c.size,
                offset,
                pages: pages_arc,
            },
        );
        c.handle = handle;
        ptr.vm_write(c).map_err(|_| VfsError::BadAddress)?;

        // CREATE_DUMB is a fire-and-forget transaction (create_2d +
        // attach_backing); one boundary notify delivers it. Linux:
        // `virtio_gpu_notify()` after the kmem-based create ioctl.
        ax_display::gpu3d_ctrl_notify();

        Ok(0)
    }

    /// True when `res_handle` has no kernel-side holder left: no
    /// `gpu_resources` entry (the creating GEM handle), no `blob_aliases`
    /// entry (a same-device import), and no open export dma-buf fd.
    ///
    /// Mirrors Linux GEM object lifetime: the host resource is unref'd only
    /// once every handle/fd referring to it is gone. The `has_display()`
    /// guard keeps the actual unref callable on display-less kernels
    /// (where `ax_display` has no inited device).
    fn resource_has_no_references(&self, res_handle: u32) -> bool {
        let resources = self.gpu_resources.lock();
        let aliases = self.blob_aliases.lock();
        let dma_bufs = self.dma_buf_refs.lock();
        !resources.contains_key(&res_handle)
            && !aliases.values().any(|a| a.res_handle == res_handle)
            && dma_bufs.get(&res_handle).is_none_or(|&n| n == 0)
    }

    /// Send `RESOURCE_UNREF` to the host for `res_handle`, but only when
    /// the last kernel-side reference was just released. Without this check
    /// an exporter closing its GEM handle would free a host resource the
    /// importer still references — virglrenderer then fails the importer's
    /// next command with "Illegal resource".
    fn unref_resource_if_last(&self, res_handle: u32) {
        if !self.resource_has_no_references(res_handle) {
            return;
        }
        if ax_display::has_display() && ax_display::has_virgl() {
            info!(
                "[card0] RESOURCE_UNREF 0x{:x} (last reference released)",
                res_handle
            );
            let _ = ax_display::gpu3d_resource_unref(res_handle);
        }
    }

    /// Release the reference an exported dma-buf fd holds on `res_handle`.
    /// Invoked from [`HostResourceDmaBuf::drop`] when the last fd closes.
    fn release_dma_buf_ref(&self, res_handle: u32) {
        let last = {
            let mut refs = self.dma_buf_refs.lock();
            match refs.get_mut(&res_handle) {
                Some(n) => {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        refs.remove(&res_handle);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            }
        };
        if last {
            self.unref_resource_if_last(res_handle);
        }
    }

    /// Shared cleanup for both DESTROY_DUMB and GEM_CLOSE. Linux funnels
    /// both through the same `drm_gem_handle_delete`, so mirroring that
    /// here keeps the two release paths consistent.
    ///
    /// The handle may be an exporter's creating handle (a `gpu_resources`
    /// entry), a same-device import alias (a `blob_aliases` entry), or a
    /// dumb-buffer handle (`dumbs`). Each of these holders keeps the host
    /// resource alive: the host-side `RESOURCE_UNREF` is only sent once the
    /// *last* reference is released (see [`Self::unref_resource_if_last`]).
    fn destroy_handle(&self, handle: u32) {
        // Silently accept unknown handles — userspace sometimes
        // destroys the same handle twice on cleanup. The `Arc` on
        // `pages` means the backing memory only goes away after both
        // this remove drops Card0's ref AND every live mapping
        // releases its retainer.
        // [card0:fx] temporary forensic: dump contents of tiny (<64B)
        // resources at destroy time. The per-frame 8x1 (bind=0x20000) is
        // created on every EGL swap and unref'd ~1s later; its 8 bytes tell
        // whether Mesa writes it per frame (counter/timestamp) or once
        // (flag/placeholder). Removed together with the res-create warn.
        if let Some(dumb) = self.dumbs.lock().get(&handle) {
            if dumb.size <= 64 {
                let p = dumb.pages.start_vaddr().as_ptr() as *const u8;
                let mut bytes = [0u8; 8];
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b = unsafe { core::ptr::read_volatile(p.add(i)) };
                }
                let u64le = u64::from_le_bytes(bytes);
                let n = SMALL_RES_DUMP_N.fetch_add(1, Ordering::Relaxed);
                if n < SMALL_RES_DUMP_LIMIT {
                    warn!(
                        "[card0:fx] small-res-destroy #{} handle={} size={} \
                         bytes={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} u64le=0x{:x}",
                        n,
                        handle,
                        dumb.size,
                        bytes[0],
                        bytes[1],
                        bytes[2],
                        bytes[3],
                        bytes[4],
                        bytes[5],
                        bytes[6],
                        bytes[7],
                        u64le
                    );
                }
            }
        }
        self.dumbs.lock().remove(&handle);
        // Imported blob alias handles are destroyed the same way.
        let released_alias = self.blob_aliases.lock().remove(&handle);
        // Remove any gpu_resources entry whose bo_handle matches this
        // handle (created by handle_create_dumb for host 2D backing, or by
        // RESOURCE_CREATE / RESOURCE_CREATE_BLOB for 3D resources).
        let removed_resources: Vec<u32> = {
            let mut resources = self.gpu_resources.lock();
            let to_remove: Vec<u32> = resources
                .iter()
                .filter(|(_, r)| r.bo_handle == handle)
                .map(|(&rh, _)| rh)
                .collect();
            for rh in &to_remove {
                resources.remove(rh);
            }
            to_remove
        };
        // Release the references this handle held. A resource only reaches
        // the host `RESOURCE_UNREF` when every holder (this entry, all
        // import aliases, all open export dma-buf fds) is gone.
        timed(&GC_UNREF_SUM, &GC_UNREF_CNT, || {
            for rh in &removed_resources {
                self.unref_resource_if_last(*rh);
            }
            if let Some(alias) = released_alias {
                self.unref_resource_if_last(alias.res_handle);
            }
        });
    }

    /// DESTROY_DUMB: dumb-buffer specific release path. Linux falls through
    /// to the same `drm_gem_handle_delete` as GEM_CLOSE; StarryOS shares
    /// one cleanup helper for both.
    fn handle_destroy_dumb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDestroyDumb;
        let d: DrmModeDestroyDumb = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        self.destroy_handle(d.handle);
        // DESTROY_DUMB may have enqueued a fire-and-forget RESOURCE_UNREF;
        // one boundary notify delivers it — Linux `virtio_gpu_notify()`.
        ax_display::gpu3d_ctrl_notify();
        Ok(0)
    }

    /// GEM_CLOSE: the only release channel Mesa uses to destroy virgl
    /// 3D/blob resources (including PRIME imports).
    fn handle_gem_close(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmGemClose;
        let c: DrmGemClose = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        self.destroy_handle(c.handle);
        // GEM_CLOSE may have enqueued a fire-and-forget RESOURCE_UNREF;
        // one boundary notify delivers it — Linux `virtio_gpu_notify()`.
        ax_display::gpu3d_ctrl_notify();
        Ok(0)
    }

    fn handle_map_dumb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeMapDumb;
        let mut m: DrmModeMapDumb = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        let offset = self
            .dumbs
            .lock()
            .get(&m.handle)
            .map(|b| b.offset)
            .ok_or(VfsError::InvalidInput)?;
        m.offset = offset;
        ptr.vm_write(m).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }
}

fn handle_version(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmVersion;
    let mut v: DrmVersion = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    v.version_major = DRIVER_VERSION_MAJOR;
    v.version_minor = DRIVER_VERSION_MINOR;
    v.version_patchlevel = DRIVER_VERSION_PATCHLEVEL;
    v.name_len = write_user_string(v.name, v.name_len, DRIVER_NAME)?;
    v.date_len = write_user_string(v.date, v.date_len, DRIVER_DATE)?;
    v.desc_len = write_user_string(v.desc, v.desc_len, DRIVER_DESC)?;
    ptr.vm_write(v).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_unique(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmUnique;
    let mut u: DrmUnique = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    let unique: String = format!("{}:0", DRIVER_NAME);
    u.unique_len = write_user_string(u.unique, u.unique_len, &unique)?;
    ptr.vm_write(u).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_set_version(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmSetVersion;
    let mut sv: DrmSetVersion = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    if sv.drm_di_major < 0 {
        sv.drm_di_major = 1;
    }
    if sv.drm_di_minor < 0 {
        sv.drm_di_minor = 4;
    }
    sv.drm_dd_major = DRIVER_VERSION_MAJOR;
    sv.drm_dd_minor = DRIVER_VERSION_MINOR;
    ptr.vm_write(sv).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_cap(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmGetCap;
    let mut cap: DrmGetCap = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    // Unknown caps return value=0 rather than EINVAL.
    cap.value = match cap.capability {
        DRM_CAP_DUMB_BUFFER => 1,
        DRM_CAP_TIMESTAMP_MONOTONIC => 1,
        DRM_CAP_CRTC_IN_VBLANK_EVENT => 1,
        DRM_CAP_ADDFB2_MODIFIERS => 1,
        DRM_CAP_PRIME => DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT,
        _ => 0,
    };
    ptr.vm_write(cap).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_set_client_cap(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *const DrmSetClientCap;
    let _scc: DrmSetClientCap = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_magic(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmAuth;
    let magic = DrmAuth { magic: 1 };
    ptr.vm_write(magic).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_auth_magic(_arg: usize) -> VfsResult<usize> {
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
    fn handle_prime_handle_to_fd(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmPrimeHandle;
        let mut req: DrmPrimeHandle = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        let dma_buf: Arc<dyn FileLike> = {
            // 3D resource (blob or classic) → export the host resource.
            let resources = self.gpu_resources.lock();
            let host_res = resources
                .iter()
                .find(|(_, r)| r.bo_handle == req.handle)
                .map(|(&rh, r)| (rh, r.size, r.blob_mem));
            drop(resources);

            if let Some((res_handle, size, blob_mem)) = host_res {
                // Each exported dma-buf fd holds one reference on the host
                // resource, keeping it alive until the last fd closes
                // (Linux: `dma_buf` pins the GEM object via its file ref).
                *self.dma_buf_refs.lock().entry(res_handle).or_insert(0) += 1;
                Arc::new(HostResourceDmaBuf {
                    res_handle,
                    size,
                    blob_mem,
                    card: self.self_weak.clone(),
                })
            } else {
                // 2D dumb buffer → export guest RAM (original path).
                let dumbs = self.dumbs.lock();
                let buf = dumbs.get(&req.handle).ok_or(VfsError::InvalidInput)?;

                // Convert the dumb buffer's virtual address to a physical address
                // range that the mmap machinery can map into user space.
                // `PhysAddrRange::from_start_size(virt_to_phys(...), size)` builds
                // `{ start = pa, end = pa + size }` — the standard idiom for
                // constructing a range from a base + length.
                let range = PhysAddrRange::from_start_size(
                    virt_to_phys(buf.pages.start_vaddr()),
                    buf.size as usize,
                );
                Arc::new(DmaBufGem {
                    range,
                    pages: buf.pages.clone(),
                    size: buf.size,
                })
            }
        };

        let cloexec = req.flags & O_CLOEXEC != 0;
        let fd = add_file_like(dma_buf, cloexec).map_err(|_| VfsError::NoMemory)?;
        req.fd = fd;

        ptr.vm_write(req).map_err(|_| VfsError::BadAddress)?;
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
    fn handle_prime_fd_to_handle(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmPrimeHandle;
        let mut req: DrmPrimeHandle = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        let file = crate::file::get_file_like(req.fd).map_err(|_| VfsError::BadFileDescriptor)?;

        // Imported *host* 3D resource (blob dma-buf): same-device import is
        // the same host resource — record an alias so RESOURCE_INFO on the
        // new handle resolves back to the exporter's res_handle + blob_mem.
        if let Some(dma) = file.as_any().downcast_ref::<HostResourceDmaBuf>() {
            let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
            self.blob_aliases.lock().insert(
                handle,
                ImportedBlob {
                    res_handle: dma.res_handle,
                    size: dma.size,
                    blob_mem: dma.blob_mem,
                },
            );
            // Linux: `virtio_gpu_gem_object_open()` attaches the imported
            // resource to this fd's context at gem-handle creation time.
            // Without this, the importer's EXECBUFFER referencing the
            // resource fails in vrend with "Illegal resource" (the
            // resource is only attached to the exporter's context), the
            // sampler view / draw is discarded, and the compositor never
            // composites the imported buffer.
            if let Some(ctx_id) = self.current_ctx() {
                // Attach-once per ctx (Linux `virtio_gpu_gem_object_open`
                // dedups in the importer servo): if this ctx already holds
                // the resource, skip the redundant CTX_ATTACH_RESOURCE.
                let already_attached = self
                    .gpu_resources
                    .lock()
                    .get(&dma.res_handle)
                    .is_some_and(|r| r.attached_ctxs.contains(&ctx_id));
                if !already_attached {
                    let ok = ax_display::gpu3d_ctx_attach_resource(ctx_id, dma.res_handle).is_ok();
                    // Record so subsequent imports / EXECBUFFERs skip this
                    // ctx and don't re-attach every submit.
                    if ok && let Some(res) = self.gpu_resources.lock().get_mut(&dma.res_handle) {
                        res.attached_ctxs.insert(ctx_id);
                    }
                }
            }
            // The import's ctx_attach is fire-and-forget; one boundary notify
            // delivers it — Linux `virtio_gpu_notify()` after gem_object_open.
            ax_display::gpu3d_ctrl_notify();
            req.handle = handle;
            ptr.vm_write(req).map_err(|_| VfsError::BadAddress)?;
            return Ok(0);
        }

        // Guest-RAM dma-buf → register in dumbs (original path).
        let dma_buf: &DmaBufGem = file
            .as_any()
            .downcast_ref::<DmaBufGem>()
            .ok_or(VfsError::InvalidInput)?;

        let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        self.dumbs.lock().insert(
            handle,
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
                width: 0,
                height: 0,
                bpp: 0,
                pitch: 0,
                size: dma_buf.size,
                offset,
                pages: dma_buf.pages.clone(),
            },
        );
        req.handle = handle;

        ptr.vm_write(req).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }
}

fn handle_get_resources(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeCardRes;
    let mut r: DrmModeCardRes = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

    let (w, h) = display_resolution();
    r.min_width = w;
    r.max_width = w;
    r.min_height = h;
    r.max_height = h;

    r.count_fbs = 0;
    r.count_crtcs = report_user_array(r.crtc_id_ptr, r.count_crtcs, &[CRTC_ID])?;
    r.count_encoders = report_user_array(r.encoder_id_ptr, r.count_encoders, &[ENCODER_ID])?;
    r.count_connectors =
        report_user_array(r.connector_id_ptr, r.count_connectors, &[CONNECTOR_ID])?;

    ptr.vm_write(r).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

impl Card0 {
    fn handle_get_crtc(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtc;
        let mut c: DrmModeCrtc = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if c.crtc_id != CRTC_ID {
            return Err(VfsError::InvalidInput);
        }
        let legacy = self.legacy_crtc.lock().clone();
        c.gamma_size = 0;
        if legacy.fb_id != 0 {
            // Report the bound state from the last successful SETCRTC.
            c.x = legacy.x;
            c.y = legacy.y;
            c.fb_id = legacy.fb_id;
            c.mode_valid = legacy.mode_valid;
            c.mode = if legacy.mode_valid != 0 {
                legacy.mode
            } else {
                DrmModeModeInfo::default()
            };
            c.count_connectors =
                report_user_array(c.set_connectors_ptr, c.count_connectors, &legacy.connectors)?;
        } else {
            // Unbound CRTC: no fb, no connectors, advertise the current
            // synthetic mode so probes still see a coherent mode.
            c.x = 0;
            c.y = 0;
            c.fb_id = 0;
            c.mode_valid = 1;
            c.mode = current_mode();
            let empty: &[u32] = &[];
            c.count_connectors =
                report_user_array(c.set_connectors_ptr, c.count_connectors, empty)?;
        }
        ptr.vm_write(c).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_set_crtc(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtc;
        let c: DrmModeCrtc = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if c.crtc_id != CRTC_ID {
            return Err(VfsError::InvalidInput);
        }

        // fb_id == 0 with no connectors is the libdrm "disable CRTC"
        // idiom. Anything else must pass full validation.
        if c.fb_id == 0 && c.count_connectors == 0 {
            *self.legacy_crtc.lock() = LegacyCrtcState::default();
            return Ok(0);
        }

        // Validate the fb exists. Snapshot under the lock so a racing
        // RMFB can't pull the rug between validation and present.
        if c.fb_id == 0 || !self.fbs.lock().contains_key(&c.fb_id) {
            return Err(VfsError::InvalidInput);
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
            c.set_connectors_ptr as *const u32,
            c.count_connectors as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
        for &id in &connectors {
            if id != CONNECTOR_ID {
                return Err(VfsError::InvalidInput);
            }
        }

        // Validation passed — commit state, then push pixels.
        *self.legacy_crtc.lock() = LegacyCrtcState {
            fb_id: c.fb_id,
            connectors,
            mode: c.mode,
            mode_valid: c.mode_valid,
            x: c.x,
            y: c.y,
        };
        self.present_fb(c.fb_id);
        Ok(0)
    }
}

fn handle_get_encoder(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetEncoder;
    let mut e: DrmModeGetEncoder = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    if e.encoder_id != ENCODER_ID {
        return Err(VfsError::InvalidInput);
    }
    e.encoder_type = DRM_MODE_ENCODER_VIRTUAL;
    e.crtc_id = CRTC_ID;
    e.possible_crtcs = 1;
    e.possible_clones = 0;
    ptr.vm_write(e).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_connector(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetConnector;
    let mut c: DrmModeGetConnector = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    if c.connector_id != CONNECTOR_ID {
        return Err(VfsError::InvalidInput);
    }
    c.encoder_id = ENCODER_ID;
    c.connector_type = DRM_MODE_CONNECTOR_VIRTUAL;
    c.connector_type_id = 1;
    c.connection = DRM_MODE_CONNECTED;
    let (w, h) = display_resolution();
    c.mm_width = w;
    c.mm_height = h;
    c.subpixel = 0;

    c.count_encoders = report_user_array(c.encoders_ptr, c.count_encoders, &[ENCODER_ID])?;

    if c.modes_ptr != 0 && c.count_modes > 0 {
        let p = c.modes_ptr as *mut DrmModeModeInfo;
        p.vm_write(current_mode())
            .map_err(|_| VfsError::BadAddress)?;
    }
    c.count_modes = 1;
    c.count_props = 0;

    ptr.vm_write(c).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

impl Card0 {
    fn handle_addfb2(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeFbCmd2;
        let mut f: DrmModeFbCmd2 = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
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
        let (kind, size) = {
            let resources = self.gpu_resources.lock();
            let host_res = resources
                .iter()
                .find(|(_, r)| r.bo_handle == handle)
                .map(|(&rh, r)| (rh, r.size, r.is_dumb_2d));
            if let Some((res_handle, res_size, is_dumb_2d)) = host_res {
                (
                    FbBacking::Gpu3d {
                        res_handle,
                        is_dumb_2d,
                    },
                    res_size,
                )
            } else {
                drop(resources);
                let dumbs = self.dumbs.lock();
                let Some(b) = dumbs.get(&handle) else {
                    return Err(VfsError::InvalidInput);
                };
                (
                    FbBacking::Dumb {
                        pages: b.pages.clone(),
                    },
                    b.size,
                )
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

        let bpp = match fb_pixel_format {
            DRM_FORMAT_XRGB8888 | DRM_FORMAT_ARGB8888 => 32u32,
            _ => {
                warn!("ADDFB2: unsupported pixel_format {:#x}", fb_pixel_format);
                return Err(VfsError::InvalidInput);
            }
        };
        let visible_bytes = fb_width * (bpp / 8);
        if fb_stride < visible_bytes {
            warn!(
                "ADDFB2: stride {} < visible bytes {} ({}bpp, {}px)",
                fb_stride, visible_bytes, bpp, fb_width
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
        self.fbs.lock().insert(
            fb_id,
            Framebuffer {
                size,
                stride: fb_stride,
                width: fb_width,
                height: fb_height,
                kind,
                dirty: None,
            },
        );
        f.fb_id = fb_id;
        ptr.vm_write(f).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_rmfb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const u32;
        let fb_id: u32 = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        self.fbs.lock().remove(&fb_id);
        // If the removed fb was the one bound by legacy SETCRTC, clear
        // the binding so GETCRTC stops reporting a stale fb_id.
        {
            let mut legacy = self.legacy_crtc.lock();
            if legacy.fb_id == fb_id {
                *legacy = LegacyCrtcState::default();
            }
        }
        Ok(0)
    }
}

// ======== M4b: planes, properties, page flip, vblank ========

fn handle_get_plane_resources(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetPlaneRes;
    let mut r: DrmModeGetPlaneRes = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    let planes: &[u32] = &[PLANE_ID];
    r.count_planes = report_user_array(r.plane_id_ptr, r.count_planes, planes)?;
    ptr.vm_write(r).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_get_plane(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetPlane;
    let mut p: DrmModeGetPlane = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    if p.plane_id != PLANE_ID {
        return Err(VfsError::InvalidInput);
    }
    p.crtc_id = CRTC_ID;
    p.fb_id = 0;
    p.possible_crtcs = 1;
    p.gamma_size = 0;
    p.count_format_types =
        report_user_array(p.format_type_ptr, p.count_format_types, SUPPORTED_FORMATS)?;
    ptr.vm_write(p).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

impl Card0 {
    fn handle_obj_get_properties(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeObjGetProperties;
        let mut q: DrmModeObjGetProperties = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        let state = *self.state.lock();
        let (prop_ids, prop_vals): (&[u32], Vec<u64>) = match (q.obj_type, q.obj_id) {
            (DRM_MODE_OBJECT_PLANE, PLANE_ID) => {
                let blob_id = self.ensure_in_formats_blob() as u64;
                (PLANE_PROPS, plane_prop_values(&state, blob_id))
            }
            (DRM_MODE_OBJECT_CRTC, CRTC_ID) => (CRTC_PROPS, crtc_prop_values(&state)),
            (DRM_MODE_OBJECT_CONNECTOR, CONNECTOR_ID) => (CONN_PROPS, conn_prop_values(&state)),
            _ => return Err(VfsError::NotFound),
        };
        report_user_array(q.props_ptr, q.count_props, prop_ids)?;
        report_user_array(q.prop_values_ptr, q.count_props, &prop_vals)?;
        q.count_props = prop_ids.len() as u32;
        ptr.vm_write(q).map_err(|_| VfsError::BadAddress)?;
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
    ]
}

/// Construct the `IN_FORMATS` blob payload advertising every
/// `SUPPORTED_FORMATS` × `DRM_FORMAT_MOD_LINEAR` pair.
fn build_in_formats_blob() -> Vec<u8> {
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
    let n_formats = SUPPORTED_FORMATS.len() as u32;
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
    for fmt in SUPPORTED_FORMATS {
        buf.extend_from_slice(&fmt.to_le_bytes());
    }
    buf.extend_from_slice(bytes_of(&me));
    buf
}

fn crtc_prop_values(s: &ModesetState) -> Vec<u64> {
    vec![s.crtc_active, s.crtc_mode_id as u64]
}

fn conn_prop_values(s: &ModesetState) -> Vec<u64> {
    vec![s.conn_crtc_id as u64]
}

/// `GETPROPERTY` — describe a single property by id.
fn handle_get_property(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *mut DrmModeGetProperty;
    let mut g: DrmModeGetProperty = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    let meta = property_meta(g.prop_id).ok_or(VfsError::NotFound)?;

    g.flags = meta.flags;
    g.name = [0; DRM_PROP_NAME_LEN];
    let nb = meta.name.as_bytes();
    let n = nb.len().min(DRM_PROP_NAME_LEN - 1);
    g.name[..n].copy_from_slice(&nb[..n]);

    match meta.kind {
        PropKind::Enum(enums) => {
            g.count_values = enums.len() as u32;
            g.count_enum_blobs = report_user_array(g.enum_blob_ptr, g.count_enum_blobs, enums)?;
        }
        PropKind::RangeU64 { min, max } => {
            let limits = [min, max];
            g.count_values = report_user_array(g.values_ptr, g.count_values, &limits)?;
            g.count_enum_blobs = 0;
        }
        PropKind::Object | PropKind::Blob => {
            g.count_values = 0;
            g.count_enum_blobs = 0;
        }
    }
    ptr.vm_write(g).map_err(|_| VfsError::BadAddress)?;
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
    Object,
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
            kind: PropKind::Object,
        },
        PROP_PLANE_CRTC_ID => PropMeta {
            name: "CRTC_ID",
            flags: DRM_MODE_PROP_OBJECT | atomic,
            kind: PropKind::Object,
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
            kind: PropKind::Object,
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
    fn handle_dirty_fb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDirtyFB;
        let dirty: DrmModeDirtyFB = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        // Record the damaged region on the fb so the next present uploads
        // only that rect. Linux feeds these clips into
        // `drm_atomic_helper_damage_merged` and the plane update transfers +
        // flushes just the merged rect; `None` (no clips) = whole fb.
        let mut fbs = self.fbs.lock();
        if !fbs.contains_key(&dirty.fb_id) {
            return Err(VfsError::InvalidInput);
        }
        let (fb_w, fb_h) = {
            let fb = fbs.get(&dirty.fb_id).unwrap();
            (fb.width, fb.height)
        };
        let full = (0u32, 0u32, fb_w, fb_h);

        if dirty.num_clips == 0 || dirty.clips_ptr == 0 {
            // No clips — the caller marks the whole fb changed.
            fbs.get_mut(&dirty.fb_id).unwrap().dirty = Some(full);
        } else {
            // Merge all clip rects into one damage union, clamping each to
            // the fb bounds (Linux clips via drm_rect_intersect).
            let n = dirty.num_clips.min(MAX_DIRTY_CLIPS) as usize;
            let clips: Vec<DrmClipRect> = vm_load(dirty.clips_ptr as *const DrmClipRect, n)
                .map_err(|_| VfsError::BadAddress)?;
            let mut merged: Option<(u32, u32, u32, u32)> = None;
            for c in clips {
                // drm_clip_rect bounds are inclusive: x2/y2 name the last
                // damaged pixel, so build an exclusive w/h from them.
                let max_x = fb_w.min(u16::MAX as u32);
                let max_y = fb_h.min(u16::MAX as u32);
                let x1 = u32::from(c.x1).min(max_x);
                let y1 = u32::from(c.y1).min(max_y);
                let x2 = (u32::from(c.x2).saturating_add(1)).min(fb_w);
                let y2 = (u32::from(c.y2).saturating_add(1)).min(fb_h);
                if x2 <= x1 || y2 <= y1 {
                    continue;
                }
                let r = (x1, y1, x2 - x1, y2 - y1);
                merged = Some(match merged {
                    None => r,
                    Some((mx, my, mw, mh)) => {
                        let nx1 = mx.min(r.0);
                        let ny1 = my.min(r.1);
                        let nw = (mx + mw).max(r.0 + r.2) - nx1;
                        let nh = (my + mh).max(r.1 + r.3) - ny1;
                        (nx1, ny1, nw, nh)
                    }
                });
            }
            fbs.get_mut(&dirty.fb_id).unwrap().dirty = merged.or(Some(full));
        }
        drop(fbs);
        self.present_fb(dirty.fb_id);
        Ok(0)
    }

    fn handle_page_flip(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeCrtcPageFlip;
        let f: DrmModeCrtcPageFlip = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if f.crtc_id != CRTC_ID || !self.fbs.lock().contains_key(&f.fb_id) {
            return Err(VfsError::InvalidInput);
        }
        self.present_fb(f.fb_id);
        if f.flags & DRM_MODE_PAGE_FLIP_EVENT != 0 {
            self.queue_flip_event(f.user_data);
        }
        Ok(0)
    }

    /// Enqueue a `drm_event_vblank` for the next `read()`, wake pollers.
    /// Shared by legacy PAGE_FLIP and atomic commits.
    fn queue_flip_event(&self, user_data: u64) {
        let seq = self
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let now = monotonic_time();
        let ev = DrmEventVblank {
            base: DrmEvent {
                event_type: DRM_EVENT_FLIP_COMPLETE,
                length: core::mem::size_of::<DrmEventVblank>() as u32,
            },
            user_data,
            tv_sec: now.as_secs() as u32,
            tv_usec: now.subsec_micros(),
            sequence: seq,
            crtc_id: CRTC_ID,
        };
        let enqueued = {
            let mut queue = self.events.lock();
            if queue.len() >= MAX_EVENTS {
                false
            } else {
                queue.push_back(ev);
                true
            }
        };
        if enqueued {
            // DRM event is queued before waking readers.
            unsafe { self.poll_rx.wake(IoEvents::IN) };
        }
    }

    /// `WAIT_VBLANK` — user asks to block until a given vblank sequence.
    /// We don't have a real vblank source, so just bump the sequence and
    /// return immediately with the current timestamp.
    fn handle_wait_vblank(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmWaitVblank;
        let request: DrmWaitVblank = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        let is_relative = request.rep_type & crate::pseudofs::dev::drm::DRM_VBLANK_RELATIVE != 0;
        let current = self.sequence.load(Ordering::Acquire);
        let target = if is_relative {
            current.wrapping_add(request.sequence)
        } else {
            request.sequence
        };
        let raw_wait = target.wrapping_sub(current);
        let wait_count = if raw_wait == 0 || raw_wait >= i32::MAX as u32 {
            1
        } else {
            raw_wait
        };

        const FRAME_PERIOD_NS: u64 = 1_000_000_000 / 60;
        let delay =
            core::time::Duration::from_nanos(FRAME_PERIOD_NS.saturating_mul(wait_count as u64));
        ax_task::sleep(delay);
        self.sequence.fetch_add(wait_count, Ordering::AcqRel);

        let now = monotonic_time();
        let reply = DrmWaitVblank {
            rep_type: 0,
            sequence: self.sequence.load(Ordering::Acquire),
            tv_sec: now.as_secs() as i64,
            tv_usec: now.subsec_micros() as i64,
        };
        ptr.vm_write(reply).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    // ======== M4c: atomic commit + blob properties ========

    fn handle_atomic(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeAtomic;
        let a: DrmModeAtomic = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        let known = DRM_MODE_ATOMIC_TEST_ONLY
            | DRM_MODE_ATOMIC_NONBLOCK
            | DRM_MODE_ATOMIC_ALLOW_MODESET
            | DRM_MODE_PAGE_FLIP_EVENT;
        if a.flags & !known != 0 {
            return Err(VfsError::InvalidInput);
        }

        let n = a.count_objs as usize;
        let objs: Vec<u32> =
            vm_load(a.objs_ptr as *const u32, n).map_err(|_| VfsError::BadAddress)?;
        let counts: Vec<u32> =
            vm_load(a.count_props_ptr as *const u32, n).map_err(|_| VfsError::BadAddress)?;
        let total_props: usize = counts.iter().map(|c| *c as usize).sum();
        let props: Vec<u32> =
            vm_load(a.props_ptr as *const u32, total_props).map_err(|_| VfsError::BadAddress)?;
        let values: Vec<u64> = vm_load(a.prop_values_ptr as *const u64, total_props)
            .map_err(|_| VfsError::BadAddress)?;

        let mut state = self.state.lock();
        let mut proposed = *state;
        // Outer Option: "the commit assigned MODE_ID at least once".
        // Inner Option: the resolved Arc (None means clearing MODE_ID to 0).
        // Only published into `mode_id_blob_ref` after the whole batch
        // validates so a TEST_ONLY commit or a later property error
        // leaves the committed mode blob ref untouched.
        let mut new_mode_blob: Option<Option<Arc<Vec<u8>>>> = None;
        let mut idx = 0;
        for (obj_i, &obj_id) in objs.iter().enumerate() {
            let obj_type = object_type_of(obj_id).ok_or(VfsError::NotFound)?;
            for _ in 0..counts[obj_i] {
                let prop_id = props[idx];
                let value = values[idx];
                idx += 1;
                if !self.apply_prop(
                    obj_type,
                    obj_id,
                    prop_id,
                    value,
                    &mut proposed,
                    &mut new_mode_blob,
                )? {
                    return Err(VfsError::InvalidInput);
                }
            }
        }

        if a.flags & DRM_MODE_ATOMIC_TEST_ONLY != 0 {
            return Ok(0);
        }

        let current_fb = proposed.plane_fb_id;
        *state = proposed;
        drop(state);
        if let Some(new_ref) = new_mode_blob {
            *self.mode_id_blob_ref.lock() = new_ref;
        }
        if current_fb != 0 {
            self.present_fb(current_fb);
            perf_report(self);
        }
        if a.flags & DRM_MODE_PAGE_FLIP_EVENT != 0 {
            self.queue_flip_event(a.user_data);
        }
        Ok(0)
    }

    /// Apply one `(prop_id, value)` tuple onto `s`. Returns `Ok(true)`
    /// if the tuple is valid for the given object type, `Ok(false)` if
    /// the property isn't one the object exposes.
    fn apply_prop(
        &self,
        obj_type: u32,
        _obj_id: u32,
        prop_id: u32,
        value: u64,
        s: &mut ModesetState,
        new_mode_blob: &mut Option<Option<Arc<Vec<u8>>>>,
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
            (DRM_MODE_OBJECT_CRTC, PROP_CRTC_ACTIVE) => {
                if value > 1 {
                    return Err(VfsError::InvalidInput);
                }
                s.crtc_active = value;
            }
            (DRM_MODE_OBJECT_CRTC, PROP_CRTC_MODE_ID) => {
                let blob = value as u32;
                let arc = if blob == 0 {
                    None
                } else {
                    // Resolve the Arc backing in priority order:
                    //   1. user-publish table — the normal case.
                    //   2. the existing `mode_id_blob_ref` if the
                    //      requested id matches the currently-committed
                    //      MODE_ID — keeps a re-commit of the same id
                    //      working even after the user destroyed their
                    //      publish handle.
                    let arc = self.blobs.lock().get(&blob).cloned().or_else(|| {
                        if s.crtc_mode_id == blob {
                            self.mode_id_blob_ref.lock().clone()
                        } else {
                            None
                        }
                    });
                    Some(arc.ok_or(VfsError::InvalidInput)?)
                };
                s.crtc_mode_id = blob;
                *new_mode_blob = Some(arc);
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

    fn handle_create_blob(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCreateBlob;
        let mut c: DrmModeCreateBlob = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if c.length == 0 || c.length as usize > MAX_BLOB_BYTES {
            return Err(VfsError::InvalidInput);
        }
        let bytes: Vec<u8> =
            vm_load(c.data as *const u8, c.length as usize).map_err(|_| VfsError::BadAddress)?;
        let id = self.next_blob_id.fetch_add(1, Ordering::Relaxed);
        self.blobs.lock().insert(id, Arc::new(bytes));
        c.blob_id = id;
        ptr.vm_write(c).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_destroy_blob(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDestroyBlob;
        let d: DrmModeDestroyBlob = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        // System (kernel-owned) blobs are not user-destroyable. Linux's
        // DRM rejects ENOTSUPP for this; we map it to PermissionDenied
        // (EPERM) since VfsError lacks a finer-grained variant.
        if self.system_blobs.lock().contains_key(&d.blob_id) {
            return Err(VfsError::PermissionDenied);
        }
        // Drop the user-publish reference. If `mode_id_blob_ref` still
        // holds the same Arc (i.e. an atomic commit pinned this blob as
        // the CRTC's `MODE_ID`), the blob data stays alive and
        // `GETPROPBLOB` keeps succeeding via the committed-state lookup
        // below until a later atomic commit replaces `MODE_ID`.
        self.blobs
            .lock()
            .remove(&d.blob_id)
            .ok_or(VfsError::NotFound)?;
        Ok(0)
    }

    fn handle_get_blob(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeGetBlob;
        let mut g: DrmModeGetBlob = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        // Clone the Arc backing out of the lock — `vm_write_slice` can
        // page-fault and sleep, and we don't want to hold the blob
        // map locked across that. Lookup order:
        //   1. user-publish table (`blobs`)
        //   2. committed `MODE_ID` ref, only when the requested id
        //      matches `state.crtc_mode_id` — this is the lifeline that
        //      keeps a user-destroyed-but-still-committed mode blob
        //      visible.
        //   3. system blobs (kernel-owned, e.g. `IN_FORMATS`).
        let bytes = if let Some(b) = self.blobs.lock().get(&g.blob_id).cloned() {
            b
        } else if g.blob_id == self.state.lock().crtc_mode_id
            && let Some(b) = self.mode_id_blob_ref.lock().clone()
        {
            b
        } else if let Some(b) = self.system_blobs.lock().get(&g.blob_id).cloned() {
            b
        } else {
            return Err(VfsError::NotFound);
        };
        if g.data != 0 && g.length > 0 {
            let n = (g.length as usize).min(bytes.len());
            vm_write_slice(g.data as *mut u8, &bytes[..n]).map_err(|_| VfsError::BadAddress)?;
        }
        g.length = bytes.len() as u32;
        ptr.vm_write(g).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    // ======== virtgpu ioctl handlers ========
    //
    // These implement the 11 virtgpu private ioctls that Mesa's virgl
    // driver needs to submit 3D rendering commands. The handlers forward
    // to the ax_display 3D API which reaches the virtio-gpu driver.
    //
    // Security: Each handler validates input from userspace before use,
    // matching Linux kernel behavior (bounds checks, EINVAL for invalid
    // params, EEXIST for duplicate context init, etc.).

    /// VIRTGPU_GETPARAM — queries driver parameters.
    ///
    /// Linux: `virtgpu_getparam_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Mesa queries all parameters during initialization. Known parameters
    /// return their values; unknown parameters return `-EINVAL` (matching
    /// Linux kernel behavior — Mesa handles this gracefully).
    fn handle_virtgpu_getparam(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuGetparam;
        let g: DrmVirtgpuGetparam = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        let has_virgl = ax_display::has_virgl();

        let value = match g.param {
            VIRTGPU_PARAM_3D_FEATURES => {
                // Must return 1 for Mesa to use virgl path.
                if has_virgl { 1 } else { 0 }
            }
            VIRTGPU_PARAM_CAPSET_QUERY_FIX => {
                // Linux 内核总是返回 1，不管 has_virgl_3d。
                // 这影响 GET_CAPS 的行为（Mesa 用它决定查询顺序）。
                1
            }
            VIRTGPU_PARAM_RESOURCE_BLOB => {
                // Report the *actual* negotiated feature (Linux:
                // `has_resource_blob ? 1 : 0`). Without RESOURCE_BLOB the
                // device doesn't support blobs and Mesa must use the classic
                // resource path — reporting 1 here would make Mesa create
                // blobs that fail.
                if ax_display::has_resource_blob() {
                    1
                } else {
                    0
                }
            }
            VIRTGPU_PARAM_HOST_VISIBLE => {
                // Host-visible memory requires blob resources. Match the
                // device's actual capability so Mesa's `supports_coherent`
                // tracks reality.
                if ax_display::has_resource_blob() {
                    1
                } else {
                    0
                }
            }
            VIRTGPU_PARAM_CROSS_DEVICE => {
                // Cross-device sharing not supported yet.
                0
            }
            VIRTGPU_PARAM_CONTEXT_INIT => {
                // Must return 1 for Mesa to use CONTEXT_INIT.
                if has_virgl { 1 } else { 0 }
            }
            VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS => {
                // Bitmask of supported capset IDs.
                // Bit 0 = reserved, bit 1 = VIRGL, bit 2 = VIRGL2.
                if has_virgl {
                    (1 << VIRTGPU_DRM_CAPSET_VIRGL) | (1 << VIRTGPU_DRM_CAPSET_VIRGL2)
                } else {
                    0
                }
            }
            _ => {
                // Unknown parameter — match Linux kernel: return -EINVAL.
                // Mesa handles this gracefully (value stays 0).
                return Err(VfsError::InvalidInput);
            }
        };

        // Linux `virtio_gpu_getparam_ioctl`: `copy_to_user((void __user *)
        // param->value, &value, sizeof(value))` — 结果写入用户指针指向的 u64,
        // 而不是写回 struct 字段。之前 `g.value = value; vm_write(g)` 把值写进
        // struct 的 value 字段(覆盖了指针),mesa 读的是指针指向的本地变量,
        // 导致所有 GETPARAM 都读到 0 → 3D_FEATURES=0 → virgl winsys 创建失败。
        vm_write_slice(g.value as *mut u64, &[value]).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// VIRTGPU_CONTEXT_INIT — initializes a rendering context on this fd.
    ///
    /// Linux: `virtgpu_context_init_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// **Critical**: This is a pure-input ioctl with no output fields.
    /// The context is implicitly bound to the file descriptor. Each fd
    /// can only call CONTEXT_INIT once (repeated calls return -EEXIST).
    ///
    /// Mesa calls this with num_params=1 and a single parameter:
    ///   { param=VIRTGPU_CONTEXT_PARAM_CAPSET_ID, value=VIRGL2(2) or VIRGL1(1) }
    fn handle_virtgpu_context_init(&self, arg: usize) -> VfsResult<usize> {
        let init: DrmVirtgpuContextInit = (arg as *const DrmVirtgpuContextInit)
            .vm_read()
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: if (!vgdev->has_context_init || !vgdev->has_virgl_3d)
        //           return -EINVAL;
        if !ax_display::has_virgl() {
            return Err(VfsError::InvalidInput);
        }

        // `card0`/`renderD128` share one Card0 for every opener, so the
        // per-fd "one CONTEXT_INIT per fd" rule becomes per-process.
        let pid = self.current_pid().ok_or(VfsError::InvalidInput)?;

        // Linux kernel: each fd can only call CONTEXT_INIT once.
        if self
            .process_ctxs
            .lock()
            .get(&pid)
            .is_some_and(|c| c.initialized)
        {
            return Err(VfsError::AlreadyExists);
        }

        // Linux kernel limits num_params to 3.
        if init.num_params > 3 {
            return Err(VfsError::InvalidInput);
        }

        // Read the parameter array from userspace.
        let mut capset_id: u32 = 0;
        let mut num_rings: u32 = 1; // Linux default

        if init.num_params > 0 && init.ctx_set_params != 0 {
            let params_ptr = init.ctx_set_params as *const DrmVirtgpuContextSetParam;
            for i in 0..init.num_params as usize {
                let param: DrmVirtgpuContextSetParam = unsafe { params_ptr.add(i) }
                    .vm_read()
                    .map_err(|_| VfsError::BadAddress)?;
                match param.param {
                    VIRTGPU_CONTEXT_PARAM_CAPSET_ID => {
                        capset_id = param.value as u32;
                        // Linux: if (value > MAX_CAPSET_ID) return -EINVAL;
                        // MAX_CAPSET_ID in Linux v6.1 is 6 (VIRTGPU_DRM_CAPSET_DRM)
                        if capset_id > VIRTGPU_DRM_CAPSET_DRM {
                            return Err(VfsError::InvalidInput);
                        }
                        // Linux: if ((vgdev->capset_id_mask & (1ULL << value)) == 0)
                        //           return -EINVAL;
                        // 我们支持 VIRGL(1) 和 VIRGL2(2)
                        if capset_id != VIRTGPU_DRM_CAPSET_VIRGL
                            && capset_id != VIRTGPU_DRM_CAPSET_VIRGL2
                        {
                            warn!("[card0] CONTEXT_INIT: unsupported capset_id={capset_id}");
                            return Err(VfsError::InvalidInput);
                        }
                    }
                    VIRTGPU_CONTEXT_PARAM_NUM_RINGS => {
                        num_rings = param.value as u32;
                        // Sanity check: limit rings.
                        if num_rings == 0 || num_rings > 64 {
                            return Err(VfsError::InvalidInput);
                        }
                    }
                    VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK => {
                        // Accept but ignore — we don't support polling yet.
                        let _ = param.value;
                    }
                    _ => {
                        // Unknown parameter — Linux returns -EINVAL.
                        return Err(VfsError::InvalidInput);
                    }
                }
            }
        }

        // Create a unique context on the host, mirroring Linux's
        // `atomic_inc_return(&vgdev->ctx_id_cursor)`. Each fd (and each
        // re-open after close) gets its own virgl context so rendering
        // state from one client cannot corrupt another.
        let ctx_id = self.next_ctx_id.fetch_add(1, Ordering::Relaxed);
        if ax_display::has_virgl() {
            // Name encodes the unique `ctx_id`, not the capset — every
            // client uses VIRGL2, so a capset-keyed name would give every
            // context the same label and make host-side error logs ("starry-
            // ctx-2") indistinguishable across clients.
            let ctx_name = format!("starry-ctx-{ctx_id}");
            ax_display::gpu3d_ctx_create(ctx_id, &ctx_name, capset_id).map_err(map_gpu3d_err)?;
        }

        self.process_ctxs.lock().insert(
            pid,
            PerFdCtx {
                initialized: true,
                capset_id,
                num_rings,
                ctx_id,
            },
        );

        info!("[card0] CONTEXT_INIT: capset_id={capset_id}, num_rings={num_rings}");
        Ok(0)
    }

    /// VIRTGPU_GET_CAPS — retrieves capability set data.
    ///
    /// Linux: `virtgpu_get_caps_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Mesa first tries cap_set_id=2 (VIRGL2), then falls back to 1 (VIRGL).
    /// The `addr` field is a user-space buffer pointer; `size` is both input
    /// (buffer capacity) and output (actual data size).
    fn handle_virtgpu_get_caps(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuGetCaps;
        let mut g: DrmVirtgpuGetCaps = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        // Validate capset ID.
        if g.cap_set_id == 0 || g.cap_set_id > VIRTGPU_DRM_CAPSET_VIRGL2 {
            return Err(VfsError::InvalidInput);
        }

        // Check cache first.
        let cache_key = (g.cap_set_id, g.cap_set_ver);
        let cached = self.capset_cache.lock().get(&cache_key).cloned();

        let cap_data = if let Some(data) = cached {
            data
        } else if ax_display::has_virgl() {
            // Linux: query GET_CAPSET_INFO for max_size, then use max_size
            // (NOT the user's g.size) to retrieve the full capset from the
            // host. Using a truncated g.size would give Mesa incomplete data
            // and cause it to enable unsupported GL features (e.g. SET_TESS_STATE).
            // Index mapping: 0=VIRGL(id=1), 1=VIRGL2(id=2).
            let capset_index = g.cap_set_id - 1;
            let max_size = ax_display::gpu3d_capset_info(capset_index)
                .map(|info| info.max_size)
                .unwrap_or(g.size);
            let query_size = max_size.max(g.size);
            let data = ax_display::gpu3d_capset(g.cap_set_id, g.cap_set_ver, query_size)
                .map_err(map_gpu3d_err)?;
            // Cache the result.
            self.capset_cache.lock().insert(cache_key, data.clone());
            data
        } else {
            return Err(VfsError::InvalidInput);
        };

        // Write capset data to user buffer.
        let write_size = (g.size as usize).min(cap_data.len());
        if g.addr != 0 && write_size > 0 {
            vm_write_slice(g.addr as *mut u8, &cap_data[..write_size])
                .map_err(|_| VfsError::BadAddress)?;
        }
        // Update size to actual data size (Linux does this too).
        g.size = cap_data.len() as u32;
        ptr.vm_write(g).map_err(|_| VfsError::BadAddress)?;

        Ok(0)
    }

    /// VIRTGPU_RESOURCE_CREATE — creates a 3D resource.
    ///
    /// Linux: `virtgpu_resource_create_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Creates a 3D resource on the host and optionally associates it with
    /// an existing GEM handle. Returns the virtio-gpu resource ID in
    /// `res_handle` (NOT the GEM handle — they are different!).
    fn handle_virtgpu_resource_create(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuResourceCreate;
        let mut r: DrmVirtgpuResourceCreate = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        // ---- Step-0 #2 forensic: who creates a per-frame buffer, and is it
        // a reusable shape? Aggregates go to perf_report's per-window table;
        // the first CREATE_DETAIL_LIMIT calls also log full detail so a human
        // sees the exact Mesa geometry/flag mix. This is pure measurement —
        // no effect on the resource path below. ----
        let create_pid = self.current_pid().unwrap_or(0);
        let exe = current_may_uninit()
            .map(|cur| cur.as_thread().proc_data.exe_path.read().clone())
            .unwrap_or_default();
        {
            let mut by_pid = self.create_by_pid.lock();
            match by_pid.get_mut(&create_pid) {
                Some((n, _)) => *n += 1,
                None => {
                    let tail: Vec<u8> = exe
                        .as_bytes()
                        .iter()
                        .rev()
                        .take(64)
                        .copied()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    by_pid.insert(create_pid, (1, tail));
                }
            }
        }
        self.rescreate_geoms.lock().insert((r.width, r.height));
        let detail_n = CREATE_DETAIL_N.fetch_add(1, Ordering::Relaxed);
        if detail_n < CREATE_DETAIL_LIMIT {
            warn!(
                "[card0:create3d] #{detail_n} pid={create_pid} exe={exe} target=0x{:x} fmt=0x{:x} \
                 bind=0x{:x} {}x{} d={} arr={} size={}",
                r.target, r.format, r.bind, r.width, r.height, r.depth, r.array_size, r.size
            );
        }

        // Linux: if (vgdev->has_virgl_3d) virtio_gpu_create_context(dev, file);
        // 我们在 CONTEXT_INIT 时已经创建了上下文。

        // Linux: 总是分配新的 GEM 对象，不复用 bo_handle。
        // bo_handle 字段在 Linux 中是输出，不是输入。
        // 如果用户设置了 bo_handle，Linux 会忽略它并分配新的。

        // Allocate a new virtio-gpu resource ID.
        let res_handle = self.next_res_handle.fetch_add(1, Ordering::Relaxed);

        // Allocate a backing buffer (dumb buffer) for this resource.
        let size = if r.size > 0 {
            r.size as u64
        } else {
            // Linux: if (params.size == 0) params.size = PAGE_SIZE;
            PAGE_SIZE_4K as u64
        };

        if size > DUMB_BUFFER_MAX_SIZE as u64 {
            return Err(VfsError::InvalidInput);
        }

        // Zero-initialize the buffer.
        let pages = timed(&RC_ALLOC_SUM, &RC_ALLOC_CNT, || {
            let pages =
                GlobalPage::alloc_contiguous((size as usize).div_ceil(PAGE_SIZE_4K), PAGE_SIZE_4K)
                    .map_err(|_| VfsError::NoMemory)?;
            unsafe {
                core::ptr::write_bytes(pages.start_vaddr().as_ptr() as *mut u8, 0, size as usize);
            }
            Ok::<_, VfsError>(pages)
        })?;
        RC_PAGE_SUM.fetch_add(
            (size as usize).div_ceil(PAGE_SIZE_4K) as u64,
            Ordering::Relaxed,
        );

        // Allocate a new GEM handle.
        let bo_handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);

        // Physical address of the backing pages, needed for ATTACH_BACKING.
        // Computed before `pages` moves into the Arc below.
        let backing_paddr = virt_to_phys(pages.start_vaddr());
        let backing_size = pages.size();

        self.dumbs.lock().insert(
            bo_handle,
            DumbBuffer {
                width: r.width,
                height: r.height,
                bpp: 32,
                pitch: r.stride,
                size,
                offset,
                pages: Arc::new(pages),
            },
        );

        // Create the resource on the host via the display driver.
        if ax_display::has_virgl() {
            let ctx_id = self.current_ctx().ok_or(VfsError::InvalidInput)?;
            timed(&RC_ENQ_SUM, &RC_ENQ_CNT, || {
                ax_display::gpu3d_resource_create(
                    ctx_id,
                    res_handle,
                    r.target,
                    r.format,
                    r.bind,
                    r.width,
                    r.height,
                    r.depth,
                    r.array_size,
                    r.last_level,
                    r.nr_samples,
                    r.flags,
                )
                .map_err(map_gpu3d_err)?;

                // Linux: `virtio_gpu_object_create()` (virtgpu_object.c) virgl
                // branch calls `virtio_gpu_object_attach()` right after
                // `virtio_gpu_cmd_resource_create_3d()` — the backing pages are
                // what the host reads/writes for TRANSFER3D.  Mesa 25.x encoded
                // transfers never use the TRANSFER_TO_HOST ioctl: the data lives
                // in these guest pages and vrend reads it via res->iov.  Without
                // the backing, TRANSFER3D fails check_transfer_iovec with
                // "Illegal resource" and the whole context goes into in_error.
                ax_display::gpu3d_attach_backing(
                    res_handle,
                    backing_paddr.as_usize() as u64,
                    backing_size as u32,
                )
                .map_err(map_gpu3d_err)?;
                Ok::<_, VfsError>(())
            })?;
        }

        // Track the resource locally.
        self.gpu_resources.lock().insert(
            res_handle,
            GpuResource {
                bo_handle,
                width: r.width,
                height: r.height,
                stride: r.stride,
                size,
                attached_ctxs: BTreeSet::new(),
                // Classic (non-blob) resources have no blob_mem/flags.
                blob_mem: 0,
                blob_flags: 0,
                is_dumb_2d: false,
                created_pid: create_pid,
            },
        );

        // Attach the resource to this fd's context immediately. Linux does
        // this in `virtio_gpu_gem_object_open` (CTX_ATTACH_RESOURCE on every
        // GEM open). Without it, TRANSFER_TO/FROM_HOST_3D (which Mesa calls
        // directly, not via EXECBUFFER) hits "Illegal resource" because the
        // host never learned the resource belongs to the context.
        let ctx_id = self.current_ctx().unwrap_or(0);
        if ax_display::has_virgl()
            && ctx_id != 0
            && timed(&RC_CTXA_SUM, &RC_CTXA_CNT, || {
                ax_display::gpu3d_ctx_attach_resource(ctx_id, res_handle).is_ok()
            })
            && let Some(res) = self.gpu_resources.lock().get_mut(&res_handle)
        {
            res.attached_ctxs.insert(ctx_id);
            info!("[card0] CTX_ATTACH res={:#x} ctx={} ok", res_handle, ctx_id);
        }

        // Linux: rc->res_handle = qobj->hw_res_handle;
        //        rc->bo_handle = handle;
        r.bo_handle = bo_handle;
        r.res_handle = res_handle;
        r.size = size as u32;
        ptr.vm_write(r).map_err(|_| VfsError::BadAddress)?;

        // RESOURCE_CREATE is a multi-command ioctl (create_3d + attach_backing
        // + ctx_attach, all fire-and-forget). One notify at the boundary
        // delivers the whole transaction — Linux `virtio_gpu_notify()`.
        timed(&RC_NOTIFY_SUM, &RC_NOTIFY_CNT, || {
            ax_display::gpu3d_ctrl_notify()
        });

        Ok(0)
    }

    /// VIRTGPU_RESOURCE_INFO — queries resource information.
    ///
    /// Linux: `virtgpu_resource_info_ioctl()` in `virtgpu_ioctl.c`
    fn handle_virtgpu_resource_info(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuResourceInfo;
        let mut info: DrmVirtgpuResourceInfo = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        // Linux: gobj = drm_gem_object_lookup(file, ri->bo_handle);
        //        if (gobj == NULL) return -ENOENT;
        // 1) Imported blob dma-buf: resolves to the *same* host resource the
        //    exporter created (Linux same-device import returns the same GEM
        //    object). Returning the real blob_mem is what makes Mesa take the
        //    maybe_untyped path and reuse the host texture.
        if let Some(imp) = self.blob_aliases.lock().get(&info.bo_handle).copied() {
            info.res_handle = imp.res_handle;
            info.size = imp.size as u32;
            info.blob_mem = imp.blob_mem;
            ptr.vm_write(info).map_err(|_| VfsError::BadAddress)?;
            return Ok(0);
        }

        // 2) Regular 3D resource (blob or classic) looked up by bo_handle.
        let resources = self.gpu_resources.lock();
        let res = resources.values().find(|r| r.bo_handle == info.bo_handle);
        let Some(res) = res else {
            return Err(VfsError::NotFound); // ENOENT
        };

        info.size = res.size as u32;
        // Report the resource's real blob_mem instead of hardcoding 0 — this
        // is the fix that lets Mesa's importer reuse the host texture.
        info.blob_mem = res.blob_mem;
        // Find the res_handle for this bo_handle.
        if let Some((&rh, _)) = resources
            .iter()
            .find(|(_, r)| r.bo_handle == info.bo_handle)
        {
            info.res_handle = rh;
        }
        drop(resources);

        ptr.vm_write(info).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// VIRTGPU_MAP — maps a GEM handle to an mmap offset.
    ///
    /// Linux: `virtgpu_map_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Returns an offset that can be used with the mmap system call.
    /// This reuses the same offset mechanism as MAP_DUMB.
    fn handle_virtgpu_map(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuMap;
        let mut m: DrmVirtgpuMap = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        // Look up the dumb buffer by handle to get its offset.
        let dumbs = self.dumbs.lock();
        let buf = dumbs.get(&m.handle).ok_or(VfsError::InvalidInput)?;
        m.offset = buf.offset;
        drop(dumbs);

        ptr.vm_write(m).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// VIRTGPU_EXECBUFFER — submits a virgl command buffer.
    ///
    /// Linux: `virtgpu_execbuffer_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// This is the core ioctl: Mesa submits VIRGL_CCMD_* command streams
    /// through this. The command buffer is read from userspace, along with
    /// an array of GEM handles that the commands reference.
    ///
    /// **Critical**: There is NO ctx_id field. The context is implicitly
    /// bound to the file descriptor.
    fn handle_virtgpu_execbuffer(&self, arg: usize) -> VfsResult<usize> {
        let mut eb: DrmVirtgpuExecbuffer = (arg as *const DrmVirtgpuExecbuffer)
            .vm_read()
            .map_err(|_| VfsError::BadAddress)?;

        // ---- Step-0 #1-a forensic: which fence mode does Mesa select?
        // (legacy fence-fd vs modern syncobj vs none). Per-window totals and
        // delta vs PERF_EXECBUF.cnt show how many submits run sync-object-free.
        if (eb.flags & VIRTGPU_EXECBUF_FENCE_FD_IN) != 0 {
            EXECB_FENCE_FD_IN.fetch_add(1, Ordering::Relaxed);
        }
        if (eb.flags & VIRTGPU_EXECBUF_FENCE_FD_OUT) != 0 {
            EXECB_FENCE_FD_OUT.fetch_add(1, Ordering::Relaxed);
        }
        if eb.num_in_syncobjs > 0 {
            EXECB_SYNC_IN.fetch_add(1, Ordering::Relaxed);
        }
        if eb.num_out_syncobjs > 0 {
            EXECB_SYNC_OUT.fetch_add(1, Ordering::Relaxed);
        }

        // Linux: if (vgdev->has_virgl_3d == false) return -ENOSYS;
        if !ax_display::has_virgl() {
            return Err(VfsError::Unsupported);
        }

        // Validate that context has been initialized for this process
        // (Linux: `virtio_gpu_create_context` on first execbuffer).
        let ctx_id = self.current_ctx().ok_or(VfsError::InvalidInput)?;

        // Linux: if ((exbuf->flags & ~VIRTGPU_EXECBUF_FLAGS)) return -EINVAL;
        // VIRTGPU_EXECBUF_FLAGS = FENCE_FD_IN | FENCE_FD_OUT | RING_IDX
        let valid_flags = VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_FENCE_FD_OUT | 0x04;
        if (eb.flags & !valid_flags) != 0 {
            return Err(VfsError::InvalidInput);
        }

        // Validate command buffer size.
        if eb.size == 0 || eb.size > 64 * 1024 * 1024 {
            return Err(VfsError::InvalidInput);
        }

        // Resolve the in-fence before touching the command buffer. Linux
        // borrows the guest's fd (mesa closes it after the ioctl returns) and
        // blocks the batch on the imported fence; only a sync_file fd created
        // by this driver can carry a fence here — anything else is -EINVAL
        // (mesa prints a debug line and continues, it does not crash).
        let in_fence = if (eb.flags & VIRTGPU_EXECBUF_FENCE_FD_IN) != 0 {
            if eb.fence_fd < 0 {
                return Err(VfsError::InvalidInput);
            }
            let file = get_file_like(eb.fence_fd).map_err(|_| VfsError::InvalidInput)?;
            let fence = file
                .downcast_arc::<SyncFile>()
                .ok()
                .ok_or(VfsError::InvalidInput)?;
            Some(fence)
        } else {
            None
        };

        // Validate bo_handles array.
        if eb.num_bo_handles > 256 {
            return Err(VfsError::InvalidInput);
        }

        // Read command buffer from userspace.
        let cmd_buf = if eb.command != 0 && eb.size > 0 {
            vm_load(eb.command as *const u8, eb.size as usize).map_err(|_| VfsError::BadAddress)?
        } else {
            Vec::new()
        };

        // Read and validate bo_handles array from userspace.
        // Each handle is a u32, stored at a u64 pointer.
        // Hoisted outside the block: VIRTGPU_WAIT maps each handle to the
        // fence of the last EXECBUFFER that referenced it (Linux per-object
        // dma-resv), so the handles must outlive the attach block below.
        let mut submit_bo_handles = Vec::new();
        if eb.num_bo_handles > 0 && eb.bo_handles != 0 {
            let handles_ptr = eb.bo_handles as *const u32;
            let mut handles = vec![0u32; eb.num_bo_handles as usize];
            for (i, handle) in handles.iter_mut().enumerate() {
                *handle = unsafe { handles_ptr.add(i) }
                    .vm_read()
                    .map_err(|_| VfsError::BadAddress)?;
            }
            submit_bo_handles.extend_from_slice(&handles);

            // Attach resources to this context on first use only. Linux does
            // this once per GEM open (`virtio_gpu_gem_object_open` →
            // CTX_ATTACH_RESOURCE); we mirror that at (ctx, resource)
            // granularity because weston and the app each hold a context that
            // shares the same buffers. Re-attaching every handle on every
            // EXECBUFFER was ~29 synchronous RTTs per frame (~2.4ms) — the
            // fixed floor that the async rewrite could not move. Imported
            // blob handles (from PRIME_FD_TO_HANDLE) resolve through
            // `blob_aliases` to the host resource the exporter created.
            let resources = self.gpu_resources.lock();
            let aliases = self.blob_aliases.lock();
            let mut to_attach = Vec::new();
            for &h in &handles {
                let rh = resources
                    .iter()
                    .find(|(_, r)| r.bo_handle == h)
                    .map(|(&rh, _)| rh)
                    .or_else(|| aliases.get(&h).map(|imp| imp.res_handle));
                if let Some(rh) = rh
                    && ax_display::has_virgl()
                    && !resources
                        .get(&rh)
                        .is_some_and(|r| r.attached_ctxs.contains(&ctx_id))
                {
                    to_attach.push(rh);
                }
            }
            drop(aliases);
            drop(resources);

            for rh in to_attach {
                if ax_display::gpu3d_ctx_attach_resource(ctx_id, rh).is_ok()
                    && let Some(res) = self.gpu_resources.lock().get_mut(&rh)
                {
                    res.attached_ctxs.insert(ctx_id);
                }
            }
        }

        // Enforce the in-fence dependency: the batch must not reach the host
        // until the imported fence signals (Linux waits the in-fence inside
        // `virtgpu_execbuffer_ioctl` before `virtio_gpu_execbuffer`).
        if let Some(in_fence) = &in_fence
            && in_fence.wait_signaled(None).is_err()
        {
            // Unreachable today (unbounded wait), but never convert a failed
            // dependency into a silently unordered submit.
            return Err(VfsError::InvalidInput);
        }
        // Submit the command buffer to the host.
        let mut out_fence_fd: i32 = -1;
        if ax_display::has_virgl() {
            let fence_id = ax_display::gpu3d_submit_cmd(ctx_id, &cmd_buf).map_err(map_gpu3d_err)?;
            // Record handle → fence so VIRTGPU_WAIT can honestly wait on the
            // last batch that referenced each handle (Linux per-object
            // dma-resv; `virtio_gpu_wait_ioctl`). Handles never submitted
            // (dumb buffers, created-but-idle GEMs) stay out of the map and
            // report idle in WAIT.
            let mut bo_fence = self.bo_fence.lock();
            for h in &submit_bo_handles {
                let e = bo_fence.entry(*h).or_insert(0);
                *e = (*e).max(fence_id);
            }
            drop(bo_fence);

            // Linux: `VIRTGPU_EXECBUF_FENCE_FD_OUT` wraps the submit fence in
            // a sync_file and returns the fd (`virtgpu_execbuffer_ioctl`). The
            // fence starts unsignaled because the submit is fire-and-forget;
            // it signals when the host pops the fenced command. fd exhaustion
            // must fail the ioctl — a success return with an invalid fd would
            // make mesa's fence_create dup -1 and risk a NULL fence.
            if (eb.flags & VIRTGPU_EXECBUF_FENCE_FD_OUT) != 0 {
                let sync_file = Arc::new(SyncFile::new(fence_id));
                sync_file.register();
                out_fence_fd = add_file_like(sync_file, true).map_err(|_| VfsError::NoMemory)?;
            }
        }
        eb.fence_fd = out_fence_fd;
        (arg as *mut DrmVirtgpuExecbuffer)
            .vm_write(eb)
            .map_err(|_| VfsError::BadAddress)?;

        // EXECBUFFER is a fire-and-forget transaction (optional ctx_attach +
        // submit_3d); one boundary notify delivers it — Linux
        // `virtio_gpu_notify()`.
        ax_display::gpu3d_ctrl_notify();

        Ok(0)
    }

    /// VIRTGPU_TRANSFER_TO_HOST — transfers data from guest to host.
    ///
    /// Linux: `virtgpu_transfer_from_host_ioctl()` in `virtgpu_ioctl.c`
    /// (Note: Linux naming is confusing — "from_host" means "from guest
    /// memory to host" in the virtio-gpu spec.)
    fn handle_virtgpu_transfer_to_host(&self, arg: usize) -> VfsResult<usize> {
        let t: DrmVirtgpu3dTransferToHost = (arg as *const DrmVirtgpu3dTransferToHost)
            .vm_read()
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: if (vgdev->has_virgl_3d == false) return -ENOSYS;
        if !ax_display::has_virgl() {
            return Err(VfsError::Unsupported);
        }

        let ctx_id = self.current_ctx().ok_or(VfsError::InvalidInput)?;

        // Linux: objs = virtio_gpu_array_from_handles(file, &args->bo_handle, 1);
        //        if (objs == NULL) return -ENOENT;
        // Validate the GEM handle exists.
        let resources = self.gpu_resources.lock();
        let res = resources.values().find(|r| r.bo_handle == t.bo_handle);
        let Some(_res) = res else {
            return Err(VfsError::NotFound); // ENOENT
        };
        drop(resources);

        // Forward to the display driver.
        if ax_display::has_virgl() {
            let box_ = ax_display::TransferBox {
                x: t.box_.x,
                y: t.box_.y,
                z: t.box_.z,
                w: t.box_.w,
                h: t.box_.h,
                d: t.box_.d,
            };
            // Find the res_handle for this bo_handle.
            let resources = self.gpu_resources.lock();
            let res_handle = resources
                .iter()
                .find(|(_, r)| r.bo_handle == t.bo_handle)
                .map(|(&rh, _)| rh)
                .unwrap_or(0);
            drop(resources);

            ax_display::gpu3d_transfer_to_host(
                ctx_id,
                res_handle,
                box_,
                t.offset as u64,
                t.level,
                t.stride,
                t.layer_stride,
            )
            .map_err(map_gpu3d_err)?;
        }

        Ok(0)
    }

    /// VIRTGPU_TRANSFER_FROM_HOST — transfers data from host to guest.
    ///
    /// Linux: `virtgpu_transfer_to_host_ioctl()` in `virtgpu_ioctl.c`
    fn handle_virtgpu_transfer_from_host(&self, arg: usize) -> VfsResult<usize> {
        let t: DrmVirtgpu3dTransferFromHost = (arg as *const DrmVirtgpu3dTransferFromHost)
            .vm_read()
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: if (vgdev->has_virgl_3d == false) return -ENOSYS;
        if !ax_display::has_virgl() {
            return Err(VfsError::Unsupported);
        }

        let ctx_id = self.current_ctx().ok_or(VfsError::InvalidInput)?;

        // Linux: objs = virtio_gpu_array_from_handles(file, &args->bo_handle, 1);
        //        if (objs == NULL) return -ENOENT;
        // Validate the GEM handle exists.
        let resources = self.gpu_resources.lock();
        let res = resources.values().find(|r| r.bo_handle == t.bo_handle);
        let Some(_res) = res else {
            return Err(VfsError::NotFound); // ENOENT
        };
        drop(resources);

        // Forward to the display driver.
        if ax_display::has_virgl() {
            let box_ = ax_display::TransferBox {
                x: t.box_.x,
                y: t.box_.y,
                z: t.box_.z,
                w: t.box_.w,
                h: t.box_.h,
                d: t.box_.d,
            };
            let resources = self.gpu_resources.lock();
            let res_handle = resources
                .iter()
                .find(|(_, r)| r.bo_handle == t.bo_handle)
                .map(|(&rh, _)| rh)
                .unwrap_or(0);
            drop(resources);

            ax_display::gpu3d_transfer_from_host(
                ctx_id,
                res_handle,
                box_,
                t.offset as u64,
                t.level,
                t.stride,
                t.layer_stride,
            )
            .map_err(map_gpu3d_err)?;
        }

        Ok(0)
    }

    /// VIRTGPU_WAIT — waits for a resource to become idle.
    ///
    /// Linux: `virtgpu_wait_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Honest wait: blocks until the last EXECBUFFER referencing the handle
    /// has completed on the host. The completion signal is the fenced submit's
    /// used-pop — the virgl fence firing after the host executed the batch.
    /// NOWAIT returns EBUSY instead of blocking (`dma_resv_test_signaled`).
    fn handle_virtgpu_wait(&self, arg: usize) -> VfsResult<usize> {
        let w: DrmVirtgpu3dWait = (arg as *const DrmVirtgpu3dWait)
            .vm_read()
            .map_err(|_| VfsError::BadAddress)?;

        // ---- Step-0 #1-a forensic: NOWAIT probe vs blocking wait, and which
        // handles Mesa actually waits on (one per batch vs a small cache). ----
        if (w.flags & VIRTGPU_WAIT_NOWAIT) != 0 {
            WAIT_NOWAIT.fetch_add(1, Ordering::Relaxed);
        }
        self.wait_handles.lock().insert(w.handle);

        // Linux: handle=0 is invalid.
        if w.handle == 0 {
            return Err(VfsError::InvalidInput);
        }

        // Linux: gobj = drm_gem_object_lookup(file, handle);
        //        if (gobj == NULL) return -ENOENT;
        // Check if the handle exists in dumbs, gpu_resources, or as an
        // imported blob alias.
        let dumbs = self.dumbs.lock();
        let has_dumb = dumbs.contains_key(&w.handle);
        drop(dumbs);

        let resources = self.gpu_resources.lock();
        let has_res = resources.values().any(|r| r.bo_handle == w.handle);
        drop(resources);

        let aliases = self.blob_aliases.lock();
        let has_alias = aliases.contains_key(&w.handle);
        drop(aliases);

        if !has_dumb && !has_res && !has_alias {
            return Err(VfsError::NotFound); // ENOENT
        }

        // Linux: dma_resv_wait_timeout(gobj->resv, ..., nowait ? 0 : timeout);
        // The per-object dma-resv is our handle → last-submit fence map.
        // A handle with no recorded submit has no pending fence and is
        // immediately idle (dma_resv_wait_timeout returns 0 with no fences).
        let last_fence = self.bo_fence.lock().get(&w.handle).copied().unwrap_or(0);
        if last_fence != 0 {
            if (w.flags & VIRTGPU_WAIT_NOWAIT) != 0 {
                // Linux: dma_resv_test_signaled → -EBUSY while busy.
                if !ax_display::gpu3d_fence_completed(last_fence).map_err(map_gpu3d_err)? {
                    WAIT_BUSY.fetch_add(1, Ordering::Relaxed);
                    return Err(VfsError::ResourceBusy); // EBUSY
                }
            } else {
                // Linux: blocking dma_resv_wait_timeout (default 15s timeout);
                // we wait until the host pops the fenced submit, i.e. the
                // virgl fence fired.
                ax_display::gpu3d_wait_fence(last_fence).map_err(map_gpu3d_err)?;
            }
        }
        Ok(0)
    }

    /// VIRTGPU_RESOURCE_CREATE_BLOB — creates a blob resource.
    ///
    /// Linux: `virtgpu_resource_create_blob_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Mesa calls this when both RESOURCE_BLOB and HOST_VISIBLE are
    /// reported as supported. The blob path is used for coherent memory
    /// allocation (shared between guest and host).
    fn handle_virtgpu_resource_create_blob(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuResourceCreateBlob;
        let mut b: DrmVirtgpuResourceCreateBlob =
            ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

        // Linux: if (!vgdev->has_resource_blob) return -EINVAL;
        if !ax_display::has_virgl() {
            return Err(VfsError::InvalidInput);
        }

        // Linux: if (rc_blob->blob_flags & ~VIRTGPU_BLOB_FLAG_USE_MASK)
        //           return -EINVAL;
        // Note: VIRTGPU_BLOB_FLAG_USE_* 是 0x0001, 0x0002, 0x0004
        // VIRTGPU_BLOB_FLAG_USE_MASK = 0x0007
        if (b.blob_flags & !0x0007) != 0 {
            return Err(VfsError::InvalidInput);
        }

        // Validate blob memory type and determine blob category.
        let host3d_blob = match b.blob_mem {
            VIRTGPU_BLOB_MEM_GUEST => false,
            VIRTGPU_BLOB_MEM_HOST3D_GUEST | VIRTGPU_BLOB_MEM_HOST3D => true,
            _ => {
                // Linux: default: return -EINVAL;
                return Err(VfsError::InvalidInput);
            }
        };

        // Linux: if (*host3d_blob) {
        //           if (!vgdev->has_virgl_3d) return -EINVAL;
        //           if (rc_blob->cmd_size % 4 != 0) return -EINVAL;
        if host3d_blob {
            if !ax_display::has_virgl() {
                return Err(VfsError::InvalidInput);
            }
            // cmd_size 必须 4 字节对齐
            if !b.cmd_size.is_multiple_of(4) {
                return Err(VfsError::InvalidInput);
            }
        } else {
            // Linux: if (rc_blob->blob_id != 0) return -EINVAL;
            //        if (rc_blob->cmd_size != 0) return -EINVAL;
            if b.blob_id != 0 {
                return Err(VfsError::InvalidInput);
            }
            if b.cmd_size != 0 {
                return Err(VfsError::InvalidInput);
            }
        }

        // Validate size.
        if b.size == 0 || b.size > 256 * 1024 * 1024 {
            return Err(VfsError::InvalidInput);
        }

        // Allocate a guest shadow buffer so VIRTGPU_MAP/mmap on the blob's
        // GEM handle keeps working. For HOST3D the real backing lives on
        // the host and we deliberately do NOT send these pages to the
        // device (nr_entries=0): QEMU and virglrenderer reject HOST3D blobs
        // that carry an iov. The shadow is only for mmap compatibility —
        // the present path is zero-copy on the host, no CPU readback.
        let alloc_size = (b.size as usize).div_ceil(PAGE_SIZE_4K) * PAGE_SIZE_4K;
        let pages = GlobalPage::alloc_contiguous(alloc_size / PAGE_SIZE_4K, PAGE_SIZE_4K)
            .map_err(|_| VfsError::NoMemory)?;

        // Zero-initialize.
        unsafe {
            core::ptr::write_bytes(pages.start_vaddr().as_ptr() as *mut u8, 0, alloc_size);
        }

        // Allocate a GEM handle.
        let bo_handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);

        self.dumbs.lock().insert(
            bo_handle,
            DumbBuffer {
                width: 0, // Blobs don't have geometry.
                height: 0,
                bpp: 0,
                pitch: 0,
                size: b.size,
                offset,
                pages: Arc::new(pages),
            },
        );

        // Allocate a virtio-gpu resource ID.
        let res_handle = self.next_res_handle.fetch_add(1, Ordering::Relaxed);

        // Linux: if (rc_blob->cmd_size) {
        //           buf = memdup_user(...);
        //           virtio_gpu_cmd_submit(vgdev, buf, rc_blob->cmd_size,
        //                                 vfpriv->ctx_id, NULL, NULL);
        //        }
        // followed by virtio_gpu_cmd_resource_create_blob(...). The display
        // driver submits the virgl cmd *before* RESOURCE_CREATE_BLOB, which
        // is the Linux order (both commands execute in sequence on the same
        // virtqueue).
        let cmd_buf = if b.cmd_size > 0 && b.cmd != 0 {
            vm_load(b.cmd as *const u8, b.cmd_size as usize).map_err(|_| VfsError::BadAddress)?
        } else {
            Vec::new()
        };

        if ax_display::has_virgl() {
            let ctx_id = self.current_ctx().ok_or(VfsError::InvalidInput)?;
            ax_display::gpu3d_resource_create_blob(
                ctx_id,
                res_handle,
                b.blob_mem,
                b.blob_flags,
                b.size,
                b.blob_id,
                &cmd_buf,
            )
            .map_err(map_gpu3d_err)?;
        }

        // Track the resource. blob_mem/blob_flags feed RESOURCE_INFO so
        // Mesa's importer can take the untyped (reuse-host-texture) path.
        self.gpu_resources.lock().insert(
            res_handle,
            GpuResource {
                bo_handle,
                width: 0,
                height: 0,
                stride: 0,
                size: b.size,
                attached_ctxs: BTreeSet::new(),
                blob_mem: b.blob_mem,
                blob_flags: b.blob_flags,
                is_dumb_2d: false,
                created_pid: self.current_pid().unwrap_or(0),
            },
        );

        // Linux: rc_blob->res_handle = bo->hw_res_handle;
        //        rc_blob->bo_handle = handle;
        b.bo_handle = bo_handle;
        b.res_handle = res_handle;
        ptr.vm_write(b).map_err(|_| VfsError::BadAddress)?;

        Ok(0)
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
fn map_gpu3d_err(err: ax_display::DisplayError) -> VfsError {
    match err {
        ax_display::DisplayError::NotSupported => VfsError::Unsupported,
        ax_display::DisplayError::NotAvailable => VfsError::WouldBlock,
        ax_display::DisplayError::Gpu3dError(kind) => match kind {
            ax_display::Gpu3dErrorKind::InvalidParam => VfsError::InvalidInput,
            ax_display::Gpu3dErrorKind::NotReady => VfsError::WouldBlock,
            _ => VfsError::Io,
        },
        _ => VfsError::Io,
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
    let _ = (b.size, b.offset, &b.pages);
};
