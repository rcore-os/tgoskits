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
//!   - All dumb buffers share the axdisplay scanout framebuffer. Each
//!     `CREATE_DUMB` records the requested geometry; `MAP_DUMB` returns
//!     offset 0; `Card0::mmap` returns the entire scanout region. This
//!     is fine for the F+G+H+I scope (single primary FB at a time);
//!     per-buffer `GlobalPage` allocation, PRIME export, and virtio-gpu
//!     zero-copy land in a follow-on PR.
//!   - Property validation is permissive: value ranges aren't rigorously
//!     enforced (tests drive sensible values). Atomic rejects only
//!     unknown `(obj, prop)` pairs and obviously-bad object/blob refs.
//!   - `WAIT_VBLANK` returns immediately with a bumped sequence number;
//!     there's no real vblank source to wait on.
//!   - Mode list: one mode matching axdisplay's resolution at a
//!     synthesized 60 Hz.

use alloc::{
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicU32, Ordering},
    task::Context,
};

use ax_hal::{mem::virt_to_phys, time::monotonic_time};
use ax_memory_addr::{PhysAddrRange, VirtAddr};
use ax_sync::Mutex;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use bytemuck::bytes_of;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use super::drm::{
    DRM_CAP_ADDFB2_MODIFIERS, DRM_CAP_CRTC_IN_VBLANK_EVENT, DRM_CAP_DUMB_BUFFER,
    DRM_CAP_TIMESTAMP_MONOTONIC, DRM_EVENT_FLIP_COMPLETE, DRM_FORMAT_ARGB8888,
    DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_XRGB8888, DRM_IOCTL_DROP_MASTER,
    DRM_IOCTL_GET_CAP, DRM_IOCTL_GET_UNIQUE, DRM_IOCTL_MODE_ADDFB2, DRM_IOCTL_MODE_ATOMIC,
    DRM_IOCTL_MODE_CREATE_DUMB, DRM_IOCTL_MODE_CREATEPROPBLOB, DRM_IOCTL_MODE_DESTROY_DUMB,
    DRM_IOCTL_MODE_DESTROYPROPBLOB, DRM_IOCTL_MODE_GETCONNECTOR, DRM_IOCTL_MODE_GETCRTC,
    DRM_IOCTL_MODE_GETENCODER, DRM_IOCTL_MODE_GETPLANE, DRM_IOCTL_MODE_GETPLANERESOURCES,
    DRM_IOCTL_MODE_GETPROPBLOB, DRM_IOCTL_MODE_GETPROPERTY, DRM_IOCTL_MODE_GETRESOURCES,
    DRM_IOCTL_MODE_MAP_DUMB, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, DRM_IOCTL_MODE_PAGE_FLIP,
    DRM_IOCTL_MODE_RMFB, DRM_IOCTL_MODE_SETCRTC, DRM_IOCTL_SET_CLIENT_CAP, DRM_IOCTL_SET_MASTER,
    DRM_IOCTL_SET_VERSION, DRM_IOCTL_VERSION, DRM_IOCTL_WAIT_VBLANK, DRM_MODE_ATOMIC_ALLOW_MODESET,
    DRM_MODE_ATOMIC_NONBLOCK, DRM_MODE_ATOMIC_TEST_ONLY, DRM_MODE_CONNECTED,
    DRM_MODE_CONNECTOR_VIRTUAL, DRM_MODE_ENCODER_VIRTUAL, DRM_MODE_FB_MODIFIERS,
    DRM_MODE_OBJECT_CONNECTOR, DRM_MODE_OBJECT_CRTC, DRM_MODE_OBJECT_PLANE,
    DRM_MODE_PAGE_FLIP_EVENT, DRM_MODE_PROP_ATOMIC, DRM_MODE_PROP_BLOB, DRM_MODE_PROP_ENUM,
    DRM_MODE_PROP_IMMUTABLE, DRM_MODE_PROP_OBJECT, DRM_MODE_PROP_RANGE, DRM_PLANE_TYPE_PRIMARY,
    DRM_PROP_NAME_LEN, DrmEvent, DrmEventVblank, DrmGetCap, DrmModeAtomic, DrmModeCardRes,
    DrmModeCreateBlob, DrmModeCreateDumb, DrmModeCrtc, DrmModeCrtcPageFlip, DrmModeDestroyBlob,
    DrmModeDestroyDumb, DrmModeFbCmd2, DrmModeGetBlob, DrmModeGetConnector, DrmModeGetEncoder,
    DrmModeGetPlane, DrmModeGetPlaneRes, DrmModeGetProperty, DrmModeMapDumb, DrmModeModeInfo,
    DrmModeObjGetProperties, DrmModePropertyEnum, DrmSetClientCap, DrmSetVersion, DrmUnique,
    DrmVersion, DrmWaitVblank,
};
use crate::pseudofs::{DeviceMmap, DeviceOps};

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

/// Metadata recorded per `CREATE_DUMB` call. In this PR every dumb
/// buffer is logically a window onto the shared axdisplay scanout
/// framebuffer; the fields here exist so user-visible `MAP_DUMB`
/// replies and `present_fb` lookups can return coherent values.
struct DumbBuffer {
    width: u32,
    height: u32,
    bpp: u32,
    pitch: u32,
    size: u64,
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
    /// `CREATE_DUMB`-allocated buffer metadata keyed by handle.
    dumbs: Mutex<BTreeMap<u32, DumbBuffer>>,
    /// Next dumb handle to hand out.
    next_dumb_handle: AtomicU32,
    /// `ADDFB2`-registered framebuffer ids, mapped to the dumb handle
    /// they were built over. Cleared on `RMFB`.
    fbs: Mutex<BTreeMap<u32, u32>>,
    /// Next fb id to hand out.
    next_fb_id: AtomicU32,
    /// User-created `CREATEPROPBLOB` blobs keyed by their blob_id.
    /// Distinct from `system_blobs` so DESTROY_BLOB cannot remove
    /// kernel-owned blobs (e.g. `IN_FORMATS`).
    blobs: Mutex<BTreeMap<u32, Vec<u8>>>,
    /// Next blob id to hand out.
    next_blob_id: AtomicU32,
    /// Kernel-owned immutable blobs (e.g. plane `IN_FORMATS`) keyed by
    /// blob_id. Read-only after publish; never freed; DESTROY_BLOB
    /// refuses to remove ids in this table.
    system_blobs: Mutex<BTreeMap<u32, Vec<u8>>>,
    /// Cached blob_id for the `IN_FORMATS` property. Allocated once
    /// under `system_blobs_init` so concurrent first-callers cannot
    /// each leak their own copy into `system_blobs`.
    in_formats_blob: AtomicU32,
    /// Serializes the lazy initialization of `in_formats_blob` so
    /// only one allocation lands in `system_blobs`.
    system_blobs_init: Mutex<()>,
}

impl Card0 {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(VecDeque::with_capacity(MAX_EVENTS)),
            poll_rx: PollSet::new(),
            sequence: AtomicU32::new(0),
            state: Mutex::new(ModesetState::default()),
            dumbs: Mutex::new(BTreeMap::new()),
            next_dumb_handle: AtomicU32::new(FIRST_DUMB_HANDLE),
            fbs: Mutex::new(BTreeMap::new()),
            next_fb_id: AtomicU32::new(FIRST_FB_ID),
            blobs: Mutex::new(BTreeMap::new()),
            next_blob_id: AtomicU32::new(FIRST_BLOB_ID),
            system_blobs: Mutex::new(BTreeMap::new()),
            in_formats_blob: AtomicU32::new(0),
            system_blobs_init: Mutex::new(()),
        })
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
        self.system_blobs.lock().insert(id, bytes);
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
            DRM_IOCTL_MODE_GETCONNECTOR => self.handle_get_connector(arg),
            DRM_IOCTL_MODE_ADDFB2 => self.handle_addfb2(arg),
            DRM_IOCTL_MODE_RMFB => self.handle_rmfb(arg),
            DRM_IOCTL_MODE_CREATE_DUMB => self.handle_create_dumb(arg),
            DRM_IOCTL_MODE_MAP_DUMB => self.handle_map_dumb(arg),
            DRM_IOCTL_MODE_DESTROY_DUMB => self.handle_destroy_dumb(arg),

            DRM_IOCTL_MODE_GETPLANERESOURCES => handle_get_plane_resources(arg),
            DRM_IOCTL_MODE_GETPLANE => self.handle_get_plane(arg),
            DRM_IOCTL_MODE_OBJ_GETPROPERTIES => self.handle_obj_get_properties(arg),
            DRM_IOCTL_MODE_GETPROPERTY => handle_get_property(arg),
            DRM_IOCTL_MODE_PAGE_FLIP => self.handle_page_flip(arg),
            DRM_IOCTL_WAIT_VBLANK => self.handle_wait_vblank(arg),

            DRM_IOCTL_MODE_ATOMIC => self.handle_atomic(arg),
            DRM_IOCTL_MODE_CREATEPROPBLOB => self.handle_create_blob(arg),
            DRM_IOCTL_MODE_DESTROYPROPBLOB => self.handle_destroy_blob(arg),
            DRM_IOCTL_MODE_GETPROPBLOB => self.handle_get_blob(arg),

            _ => Err(VfsError::OperationNotSupported),
        }
    }

    fn mmap(&self, _offset: u64) -> DeviceMmap {
        // Map the whole axdisplay scanout region. All dumb buffers
        // share this backing in the F+G+H+I scope.
        if !ax_display::has_display() {
            return DeviceMmap::None;
        }
        let info = ax_display::framebuffer_info();
        DeviceMmap::Physical(PhysAddrRange::from_start_size(
            virt_to_phys(VirtAddr::from(info.fb_base_vaddr)),
            info.fb_size,
        ))
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
            self.poll_rx.register(context.waker());
        }
    }
}

impl Card0 {
    /// Push pixels for the given fb id to the scanout. In the F+G+H+I
    /// scope every dumb buffer is the scanout, so all "show this
    /// buffer" really does is kick `framebuffer_flush`. A future PR
    /// makes this per-buffer with virtio-gpu zero-copy.
    fn present_fb(&self, fb_id: u32) {
        if !self.fbs.lock().contains_key(&fb_id) {
            return;
        }
        let _ = ax_display::framebuffer_flush();
    }

    fn handle_create_dumb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCreateDumb;
        let mut c: DrmModeCreateDumb = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        // libdrm/Mesa always pass bpp ∈ {8, 16, 24, 32}. Reject anything
        // else: bpp == 0, non-byte-multiple, or absurdly large bpp would
        // overflow `pitch` below or surface as garbage in users' surfaces.
        if c.width == 0
            || c.height == 0
            || c.bpp == 0
            || c.bpp > 64
            || !c.bpp.is_multiple_of(8)
            || c.flags != 0
        {
            return Err(VfsError::InvalidInput);
        }
        // Cap dimensions to avoid u32 overflow in `pitch` and u64 overflow
        // in `size`. 1<<15 per axis is enough for any user-mode test.
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
        // Refuse multi-GiB dumb allocations — these tests don't need them
        // and reasonable users won't either.
        if size > 256 * 1024 * 1024 {
            return Err(VfsError::InvalidInput);
        }
        c.pitch = pitch;
        c.size = size;
        let handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        self.dumbs.lock().insert(
            handle,
            DumbBuffer {
                width: c.width,
                height: c.height,
                bpp: c.bpp,
                pitch: c.pitch,
                size: c.size,
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
        // destroys the same handle twice on cleanup.
        self.dumbs.lock().remove(&d.handle);
        Ok(0)
    }

    fn handle_map_dumb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeMapDumb;
        let mut m: DrmModeMapDumb = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if !self.dumbs.lock().contains_key(&m.handle) {
            return Err(VfsError::InvalidInput);
        }
        // All dumb buffers share offset 0 (the start of the scanout).
        // L will replace this with a per-buffer monotonic key.
        m.offset = 0;
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
        _ => 0,
    };
    ptr.vm_write(cap).map_err(|_| VfsError::BadAddress)?;
    Ok(0)
}

fn handle_set_client_cap(arg: usize) -> VfsResult<usize> {
    let ptr = arg as *const DrmSetClientCap;
    let _scc: DrmSetClientCap = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
    // Accept any cap. libdrm's older codepaths treat EINVAL on
    // SET_CLIENT_CAP as "card too old, give up".
    Ok(0)
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
    /// `GETCRTC` — return the CRTC's current modeset, reflecting the last
    /// `SETCRTC` or atomic commit. `fb_id` mirrors the plane's bound fb so
    /// `drmModeGetCrtc()` retrieves a coherent post-commit view. With one
    /// connector wired to one CRTC, `count_connectors` is 1 whenever the
    /// CRTC is active, and `set_connectors_ptr` (if non-NULL) is filled
    /// with the bound connector id truncated to the user-provided count.
    fn handle_get_crtc(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtc;
        let mut c: DrmModeCrtc = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if c.crtc_id != CRTC_ID {
            return Err(VfsError::InvalidInput);
        }
        let state = *self.state.lock();
        c.x = 0;
        c.y = 0;
        c.fb_id = state.plane_fb_id;
        c.gamma_size = 0;
        c.mode_valid = if state.crtc_active != 0 { 1 } else { 0 };
        c.mode = current_mode();
        let bound: &[u32] = if state.conn_crtc_id == CRTC_ID {
            &[CONNECTOR_ID]
        } else {
            &[]
        };
        c.count_connectors = report_user_array(c.set_connectors_ptr, c.count_connectors, bound)?;
        ptr.vm_write(c).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// `SETCRTC` — legacy modeset entry point. Mirror the post-commit
    /// state into `ModesetState` so `GETCRTC`, `OBJ_GETPROPERTIES`, and
    /// connector queries all see the same configuration. A zero `fb_id`
    /// is a disable request. Non-zero `fb_id` must reference a
    /// framebuffer created via `ADDFB2`. The supplied connector list
    /// (`set_connectors_ptr` / `count_connectors`) is validated against
    /// the fixed connector id; any unknown id is rejected with `EINVAL`
    /// to match the Linux DRM contract.
    fn handle_set_crtc(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeCrtc;
        let c: DrmModeCrtc = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if c.crtc_id != CRTC_ID {
            return Err(VfsError::InvalidInput);
        }
        if c.fb_id != 0 && !self.fbs.lock().contains_key(&c.fb_id) {
            return Err(VfsError::InvalidInput);
        }
        // Validate the connector list. A disable (`fb_id == 0`) is allowed
        // to pass an empty list; an enable must reference exactly the one
        // connector we expose. Linux's DRM core rejects unknown ids with
        // EINVAL — accepting them silently would let userspace corrupt
        // the modeset surface seen by GETCRTC / GETCONNECTOR.
        if c.count_connectors > 0 {
            if c.set_connectors_ptr == 0 {
                return Err(VfsError::InvalidInput);
            }
            // Cap the array read to keep a wild count from blowing up
            // the kernel allocator. One connector is all this driver
            // ever exposes; anything beyond that is by definition bogus.
            if c.count_connectors > 16 {
                return Err(VfsError::InvalidInput);
            }
            let ids: Vec<u32> = vm_load(
                c.set_connectors_ptr as *const u32,
                c.count_connectors as usize,
            )
            .map_err(|_| VfsError::BadAddress)?;
            for id in &ids {
                if *id != CONNECTOR_ID {
                    return Err(VfsError::InvalidInput);
                }
            }
        }
        let connector_bound = c.fb_id != 0 && c.count_connectors > 0;
        {
            let mut state = self.state.lock();
            if c.fb_id == 0 {
                state.crtc_active = 0;
                state.conn_crtc_id = 0;
                state.plane_fb_id = 0;
                state.plane_crtc_id = 0;
            } else {
                state.crtc_active = 1;
                state.conn_crtc_id = if connector_bound { CRTC_ID } else { 0 };
                state.plane_fb_id = c.fb_id;
                state.plane_crtc_id = CRTC_ID;
            }
        }
        if c.fb_id != 0 {
            self.present_fb(c.fb_id);
        }
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

impl Card0 {
    /// `GETCONNECTOR` — describe the connector. Returns encoder, mode, and
    /// the `CRTC_ID` property (same set as `OBJ_GETPROPERTIES` for this
    /// connector) so libdrm's `drmModeGetConnector()` sees a property
    /// surface consistent with the atomic property enumeration.
    fn handle_get_connector(&self, arg: usize) -> VfsResult<usize> {
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

        let state = *self.state.lock();
        let prop_vals = conn_prop_values(&state);
        report_user_array(c.props_ptr, c.count_props, CONN_PROPS)?;
        report_user_array(c.prop_values_ptr, c.count_props, &prop_vals)?;
        c.count_props = CONN_PROPS.len() as u32;

        ptr.vm_write(c).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }
}

impl Card0 {
    fn handle_addfb2(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeFbCmd2;
        let mut f: DrmModeFbCmd2 = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        let handle = f.handles[0];
        if !self.dumbs.lock().contains_key(&handle) {
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
        self.fbs.lock().insert(fb_id, handle);
        f.fb_id = fb_id;
        ptr.vm_write(f).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    fn handle_rmfb(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const u32;
        let fb_id: u32 = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        self.fbs.lock().remove(&fb_id);
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

impl Card0 {
    /// `GETPLANE` — report the current bind state of the (single) plane.
    /// `crtc_id` and `fb_id` reflect the post-commit `ModesetState`
    /// produced by the last `SETCRTC` / atomic commit / `PAGE_FLIP`,
    /// matching what `OBJ_GETPROPERTIES` exposes. A plane that is not
    /// bound to any CRTC reports both fields as 0.
    fn handle_get_plane(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeGetPlane;
        let mut p: DrmModeGetPlane = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if p.plane_id != PLANE_ID {
            return Err(VfsError::InvalidInput);
        }
        let state = *self.state.lock();
        p.crtc_id = state.plane_crtc_id;
        p.fb_id = state.plane_fb_id;
        p.possible_crtcs = 1;
        p.gamma_size = 0;
        p.count_format_types =
            report_user_array(p.format_type_ptr, p.count_format_types, SUPPORTED_FORMATS)?;
        ptr.vm_write(p).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }
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
    fn handle_page_flip(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *const DrmModeCrtcPageFlip;
        let f: DrmModeCrtcPageFlip = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        if f.crtc_id != CRTC_ID || !self.fbs.lock().contains_key(&f.fb_id) {
            return Err(VfsError::InvalidInput);
        }
        // Mirror the new fb into modeset state so a subsequent
        // GETCRTC / OBJ_GETPROPERTIES sees the post-flip fb_id rather
        // than the value left over from the previous SETCRTC or
        // atomic commit.
        self.state.lock().plane_fb_id = f.fb_id;
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
            self.poll_rx.wake();
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
        let mut idx = 0;
        for (obj_i, &obj_id) in objs.iter().enumerate() {
            let obj_type = object_type_of(obj_id).ok_or(VfsError::NotFound)?;
            for _ in 0..counts[obj_i] {
                let prop_id = props[idx];
                let value = values[idx];
                idx += 1;
                if !self.apply_prop(obj_type, obj_id, prop_id, value, &mut proposed)? {
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
                if blob != 0 && !self.blobs.lock().contains_key(&blob) {
                    return Err(VfsError::InvalidInput);
                }
                s.crtc_mode_id = blob;
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
        self.blobs.lock().insert(id, bytes);
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
        self.blobs
            .lock()
            .remove(&d.blob_id)
            .ok_or(VfsError::NotFound)?;
        Ok(0)
    }

    fn handle_get_blob(&self, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmModeGetBlob;
        let mut g: DrmModeGetBlob = ptr.vm_read().map_err(|_| VfsError::BadAddress)?;
        // Clone the blob bytes out of the lock — `vm_write_slice` can
        // page-fault and sleep, and we don't want to hold the blob
        // map locked across that. Search user blobs first, then
        // system blobs.
        let bytes = {
            if let Some(b) = self.blobs.lock().get(&g.blob_id) {
                b.clone()
            } else if let Some(b) = self.system_blobs.lock().get(&g.blob_id) {
                b.clone()
            } else {
                return Err(VfsError::NotFound);
            }
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

// Acknowledge dead fields to silence lint warnings — these are
// recorded but not directly read. The whole struct is meaningful.
#[allow(dead_code)]
const _DUMB_BUFFER_FIELDS_USED: fn(&DumbBuffer) = |b| {
    let _ = (b.width, b.height, b.bpp, b.pitch, b.size);
};
