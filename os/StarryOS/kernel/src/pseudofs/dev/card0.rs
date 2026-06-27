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
//!     `present_fb` memcpies the committed buffer into the axdisplay
//!     scanout framebuffer and kicks `framebuffer_flush`. PRIME export
//!     and virtio-gpu zero-copy resource plumbing land in follow-on
//!     PRs.
//!   - Property validation is permissive: value ranges aren't rigorously
//!     enforced (tests drive sensible values). Atomic rejects only
//!     unknown `(obj, prop)` pairs and obviously-bad object/blob refs.
//!   - `WAIT_VBLANK` returns immediately with a bumped sequence number;
//!     there's no real vblank source to wait on.
//!   - Mode list: one mode matching axdisplay's resolution at a
//!     synthesized 60 Hz.

use alloc::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    task::Context,
};

use ax_alloc::GlobalPage;
use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddrRange};
use ax_runtime::hal::{mem::virt_to_phys, time::monotonic_time};
use ax_sync::Mutex;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use bytemuck::bytes_of;
use linux_raw_sys::general::O_CLOEXEC;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use super::drm::{
    DRM_CAP_ADDFB2_MODIFIERS, DRM_CAP_CRTC_IN_VBLANK_EVENT, DRM_CAP_DUMB_BUFFER, DRM_CAP_PRIME,
    DRM_CAP_TIMESTAMP_MONOTONIC, DRM_EVENT_FLIP_COMPLETE, DRM_FORMAT_ARGB8888,
    DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_XRGB8888, DRM_IOCTL_AUTH_MAGIC,
    DRM_IOCTL_DROP_MASTER, DRM_IOCTL_GET_CAP, DRM_IOCTL_GET_MAGIC, DRM_IOCTL_GET_UNIQUE,
    DRM_IOCTL_MODE_ADDFB2, DRM_IOCTL_MODE_ATOMIC, DRM_IOCTL_MODE_CREATE_DUMB,
    DRM_IOCTL_MODE_CREATEPROPBLOB, DRM_IOCTL_MODE_DESTROY_DUMB, DRM_IOCTL_MODE_DESTROYPROPBLOB,
    DRM_IOCTL_MODE_DIRTYFB, DRM_IOCTL_MODE_GETCONNECTOR, DRM_IOCTL_MODE_GETCRTC,
    DRM_IOCTL_MODE_GETENCODER, DRM_IOCTL_MODE_GETPLANE, DRM_IOCTL_MODE_GETPLANERESOURCES,
    DRM_IOCTL_MODE_GETPROPBLOB, DRM_IOCTL_MODE_GETPROPERTY, DRM_IOCTL_MODE_GETRESOURCES,
    DRM_IOCTL_MODE_MAP_DUMB, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, DRM_IOCTL_MODE_PAGE_FLIP,
    DRM_IOCTL_MODE_RMFB, DRM_IOCTL_MODE_SETCRTC, DRM_IOCTL_PRIME_FD_TO_HANDLE,
    DRM_IOCTL_PRIME_HANDLE_TO_FD, DRM_IOCTL_SET_CLIENT_CAP, DRM_IOCTL_SET_MASTER,
    DRM_IOCTL_SET_VERSION, DRM_IOCTL_VERSION, DRM_IOCTL_WAIT_VBLANK, DRM_MODE_ATOMIC_ALLOW_MODESET,
    DRM_MODE_ATOMIC_NONBLOCK, DRM_MODE_ATOMIC_TEST_ONLY, DRM_MODE_CONNECTED,
    DRM_MODE_CONNECTOR_VIRTUAL, DRM_MODE_ENCODER_VIRTUAL, DRM_MODE_FB_MODIFIERS,
    DRM_MODE_OBJECT_CONNECTOR, DRM_MODE_OBJECT_CRTC, DRM_MODE_OBJECT_PLANE,
    DRM_MODE_PAGE_FLIP_EVENT, DRM_MODE_PROP_ATOMIC, DRM_MODE_PROP_BLOB, DRM_MODE_PROP_ENUM,
    DRM_MODE_PROP_IMMUTABLE, DRM_MODE_PROP_OBJECT, DRM_MODE_PROP_RANGE, DRM_PLANE_TYPE_PRIMARY,
    DRM_PRIME_CAP_EXPORT, DRM_PRIME_CAP_IMPORT, DRM_PROP_NAME_LEN, DrmAuth, DrmEvent,
    DrmEventVblank, DrmGetCap, DrmModeAtomic, DrmModeCardRes, DrmModeCreateBlob, DrmModeCreateDumb,
    DrmModeCrtc, DrmModeCrtcPageFlip, DrmModeDestroyBlob, DrmModeDestroyDumb, DrmModeDirtyFB,
    DrmModeFbCmd2, DrmModeGetBlob, DrmModeGetConnector, DrmModeGetEncoder, DrmModeGetPlane,
    DrmModeGetPlaneRes, DrmModeGetProperty, DrmModeMapDumb, DrmModeModeInfo,
    DrmModeObjGetProperties, DrmModePropertyEnum, DrmPrimeHandle, DrmSetClientCap, DrmSetVersion,
    DrmUnique, DrmVersion, DrmWaitVblank,
};
use crate::{
    file::{FileLike, add_file_like},
    pseudofs::{DeviceMmap, DeviceOps},
};

pub const DRIVER_NAME: &str = "starry-simpledrm";
pub const DRIVER_DATE: &str = "2026-04-19";
pub const DRIVER_DESC: &str = "StarryOS simple DRM driver";
pub const DRIVER_VERSION_MAJOR: i32 = 1;
pub const DRIVER_VERSION_MINOR: i32 = 0;
pub const DRIVER_VERSION_PATCHLEVEL: i32 = 0;

/// Fixed object IDs advertised by GETRESOURCES / GETCONNECTOR / GETENCODER.
const CRTC_ID: u32 = 0x10;
const ENCODER_ID: u32 = 0x20;
const CONNECTOR_ID: u32 = 0x30;
const PLANE_ID: u32 = 0x40;

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
    /// Row stride (pitch) in bytes from CREATE_DUMB.
    stride: u32,
    /// Backing pages. Shared with the (now possibly removed) dumb
    /// buffer; refcount keeps them alive until both this fb and any
    /// user mappings have been dropped.
    pages: Arc<GlobalPage>,
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

    fn device_mmap(&self, offset: u64, length: u64) -> AxResult<DeviceMmap> {
        // Validate that the requested sub-range fits within the buffer.
        // `checked_add` guards against a wrapping length that would
        // bypass the > self.size check.
        let end = offset.checked_add(length).ok_or(AxError::InvalidInput)?;
        if end > self.size {
            return Err(AxError::InvalidInput);
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
    irq_handle: spin::Once<ax_runtime::hal::irq::IrqHandle>,
}

impl Card0 {
    pub fn new() -> Arc<Self> {
        let card = Arc::new(Self {
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
            irq_handle: spin::Once::new(),
        });
        card.register_irq();
        card
    }

    fn register_irq(self: &Arc<Self>) {
        if !ax_display::has_display() {
            return;
        }
        let Some(irq) = ax_display::framebuffer_irq_num() else {
            return;
        };

        let data = NonNull::from(self.as_ref()).cast();
        let request = ax_runtime::hal::irq::IrqRequest::new(card0_irq_handler, data)
            .share_mode(ax_runtime::hal::irq::ShareMode::Shared)
            .auto_enable(ax_runtime::hal::irq::AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(irq, request) {
            Ok(handle) => {
                self.irq_handle.call_once(|| handle);
                ax_display::framebuffer_enable_irq();
                if let Some(handle) = self.irq_handle.get().copied()
                    && let Err(err) = ax_runtime::hal::irq::enable_irq(handle)
                {
                    warn!("failed to enable display irq handler for irq {irq}: {err:?}");
                    ax_display::framebuffer_disable_irq();
                }
            }
            Err(err) => {
                warn!("failed to register display irq handler for irq {irq}: {err:?}");
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
        match cmd {
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
            DRM_IOCTL_MODE_CREATE_DUMB => self.handle_create_dumb(arg),
            DRM_IOCTL_MODE_MAP_DUMB => self.handle_map_dumb(arg),
            DRM_IOCTL_MODE_DESTROY_DUMB => self.handle_destroy_dumb(arg),

            DRM_IOCTL_MODE_GETPLANERESOURCES => handle_get_plane_resources(arg),
            DRM_IOCTL_MODE_GETPLANE => handle_get_plane(arg),
            DRM_IOCTL_MODE_OBJ_GETPROPERTIES => self.handle_obj_get_properties(arg),
            DRM_IOCTL_MODE_GETPROPERTY => handle_get_property(arg),
            DRM_IOCTL_MODE_PAGE_FLIP => self.handle_page_flip(arg),
            DRM_IOCTL_WAIT_VBLANK => self.handle_wait_vblank(arg),

            DRM_IOCTL_MODE_ATOMIC => self.handle_atomic(arg),
            DRM_IOCTL_MODE_CREATEPROPBLOB => self.handle_create_blob(arg),
            DRM_IOCTL_MODE_DESTROYPROPBLOB => self.handle_destroy_blob(arg),
            DRM_IOCTL_MODE_GETPROPBLOB => self.handle_get_blob(arg),

            DRM_IOCTL_GET_MAGIC => handle_get_magic(arg),
            DRM_IOCTL_AUTH_MAGIC => handle_auth_magic(arg),
            DRM_IOCTL_MODE_DIRTYFB => self.handle_dirty_fb(arg),
            DRM_IOCTL_PRIME_HANDLE_TO_FD => self.handle_prime_handle_to_fd(arg),
            DRM_IOCTL_PRIME_FD_TO_HANDLE => self.handle_prime_fd_to_handle(arg),

            _ => Err(VfsError::OperationNotSupported),
        }
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

unsafe fn card0_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    if ax_display::framebuffer_handle_irq() {
        ax_runtime::hal::irq::IrqReturn::Handled
    } else {
        ax_runtime::hal::irq::IrqReturn::Unhandled
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
}

impl Card0 {
    /// Look up the dumb buffer behind a given `fb_id` and copy its
    /// contents into the axdisplay scanout, then trigger
    /// `framebuffer_flush`. Used by `SETCRTC`, `PAGE_FLIP`, and atomic
    /// commits — every path that userspace uses to "show this buffer
    /// now" routes through here. A follow-on PR will swap the memcpy
    /// for virtio-gpu zero-copy via `set_scanout` / `transfer_to_host`.
    fn present_fb(&self, fb_id: u32) {
        // Snapshot the backing pages out of the fb registry, then drop
        // the lock so `framebuffer_flush` doesn't run with the
        // map locked. Pages survive a concurrent DESTROY_DUMB because
        // the fb owns its own Arc<GlobalPage> clone.
        let (pages, size, src_stride) = match self.fbs.lock().get(&fb_id) {
            Some(fb) => (fb.pages.clone(), fb.size, fb.stride),
            None => return,
        };
        if !ax_display::has_display() {
            return;
        };
        let info = ax_display::framebuffer_info();
        let src = pages.start_vaddr().as_usize() as *const u8;
        let dst = info.fb_base_vaddr as *mut u8;

        if src_stride != 0
            && info.stride != 0
            && src_stride as usize != info.stride
        {
            // Stride mismatch — copy row by row to avoid diagonal tearing.
            let rows = (size as usize) / src_stride as usize;
            let dst_limit = info.fb_size / info.stride.max(1);
            let rows = rows.min(dst_limit);
            let bytes_per_row = (src_stride as usize).min(info.stride);
            for row in 0..rows {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src.add(row * src_stride as usize),
                        dst.add(row * info.stride),
                        bytes_per_row,
                    );
                }
            }
        } else {
            // Strides match (or unknown) — flat copy.
            let copy = (size as usize).min(info.fb_size);
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst, copy);
            }
        }
        let _ = ax_display::framebuffer_flush();
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
        let mut backing =
            GlobalPage::alloc_contiguous(pages, PAGE_SIZE_4K).map_err(|_| VfsError::NoMemory)?;
        // Linux DRM dumb buffers must be returned zeroed: the page
        // allocator may hand back pages that previously held kernel
        // data, and we mmap them straight into user space.
        backing.zero();
        let pages_arc = Arc::new(backing);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);

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
        Ok(0)
    }

    fn handle_destroy_dumb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeDestroyDumb;
        let d: DrmModeDestroyDumb = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        // Silently accept unknown handles — userspace sometimes
        // destroys the same handle twice on cleanup. The `Arc` on
        // `pages` means the backing memory only goes away after both
        // this remove drops Card0's ref AND every live mapping
        // releases its retainer.
        self.dumbs.lock().remove(&d.handle);
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
    /// Looks up the dumb buffer backing `req.handle`, wraps its physical
    /// pages in a [`DmaBufGem`], and registers it in the calling process's
    /// fd table.  The returned fd can be passed across processes via
    /// `SCM_RIGHTS` or used directly with `mmap`/`read`.
    fn handle_prime_handle_to_fd(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmPrimeHandle;
        let mut req: DrmPrimeHandle = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;

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
        let dma_buf = Arc::new(DmaBufGem {
            range,
            pages: buf.pages.clone(),
            size: buf.size,
        });

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
        // Resolve and clone-retain the dumb's backing under the dumbs
        // lock so a concurrent DESTROY_DUMB can't race the fb's
        // initial Arc bump.
        let (pages, size, stride) = {
            let dumbs = self.dumbs.lock();
            let Some(b) = dumbs.get(&handle) else {
                return Err(VfsError::InvalidInput);
            };
            (b.pages.clone(), b.size, b.pitch)
        };
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
        self.fbs.lock().insert(fb_id, Framebuffer { size, stride, pages });
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
        if !self.fbs.lock().contains_key(&dirty.fb_id) {
            return Err(VfsError::InvalidInput);
        }
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
