//! DRM ioctl decoding helpers and userspace struct definitions.
//!
//! See Linux's `include/uapi/drm/drm.h` for the canonical definitions —
//! everything here is layout-compatible with that header.  This file
//! intentionally covers only the subset `card0.rs` implements today; the
//! full DRM ioctl set has ~100 commands, and we add them incrementally.

use core::ffi::c_int;

use bytemuck::{AnyBitPattern, NoUninit};

// ---- ioctl-number encoding ----
//
// The kernel uses a 32-bit packed layout:
//   bits 31..30 : direction   (NONE=0, WRITE=1, READ=2, READ|WRITE=3)
//   bits 29..16 : struct size (14 bits)
//   bits 15..8  : type (a.k.a. "magic" / subsystem tag)
//   bits  7..0  : command number
//
// DRM uses type 'd' (0x64) for all its commands.

const IOC_READ: u32 = 2;
const IOC_WRITE: u32 = 1;

const fn ioc(dir: u32, ty: u8, nr: u8, size: u16) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | (nr as u32)
}
#[inline]
const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ioc(
        IOC_READ | IOC_WRITE,
        ty,
        nr,
        core::mem::size_of::<T>() as u16,
    )
}
#[inline]
const fn io(ty: u8, nr: u8) -> u32 {
    ioc(0, ty, nr, 0)
}

pub const DRM_TYPE: u8 = b'd';

pub const DRM_IOCTL_VERSION: u32 = iowr::<DrmVersion>(DRM_TYPE, 0x00);
pub const DRM_IOCTL_GET_UNIQUE: u32 = iowr::<DrmUnique>(DRM_TYPE, 0x01);
pub const DRM_IOCTL_SET_VERSION: u32 = iowr::<DrmSetVersion>(DRM_TYPE, 0x07);
pub const DRM_IOCTL_GET_CAP: u32 = iowr::<DrmGetCap>(DRM_TYPE, 0x0c);
pub const DRM_IOCTL_SET_CLIENT_CAP: u32 = ioc(
    IOC_WRITE,
    DRM_TYPE,
    0x0d,
    core::mem::size_of::<DrmSetClientCap>() as u16,
);
pub const DRM_IOCTL_SET_MASTER: u32 = io(DRM_TYPE, 0x1e);
pub const DRM_IOCTL_DROP_MASTER: u32 = io(DRM_TYPE, 0x1f);

// ---- DRM_IOCTL_VERSION ----
//
// Userspace allocates `name`/`date`/`desc` buffers, sets `*_len` to their
// capacity, and calls the ioctl.  The kernel fills the buffers (truncated
// to the provided capacities) and updates `*_len` to the amount written
// (not counting the nul terminator, per Linux convention).

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVersion {
    pub version_major: c_int,
    pub version_minor: c_int,
    pub version_patchlevel: c_int,
    /// `_pad` — field missing on 32-bit. On 64-bit the compiler inserts
    /// padding naturally before the u64 fields.
    pub name_len: usize,
    pub name: u64,
    pub date_len: usize,
    pub date: u64,
    pub desc_len: usize,
    pub desc: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmUnique {
    pub unique_len: usize,
    pub unique: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmSetVersion {
    pub drm_di_major: c_int,
    pub drm_di_minor: c_int,
    pub drm_dd_major: c_int,
    pub drm_dd_minor: c_int,
}

// ---- DRM_IOCTL_GET_CAP ----
//
// A single `(cap_id, value)` query.  Userspace sets `cap_id`; kernel
// writes back `value`.

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmGetCap {
    pub capability: u64,
    pub value: u64,
}

/// DRM capability IDs (`DRM_CAP_*`).  Only the ones we report are listed.
pub const DRM_CAP_DUMB_BUFFER: u64 = 0x1;
pub const DRM_CAP_TIMESTAMP_MONOTONIC: u64 = 0x6;
pub const DRM_CAP_CRTC_IN_VBLANK_EVENT: u64 = 0x12;
/// Reported by Linux DRM drivers that honor the `modifier[]` array in
/// `drm_mode_fb_cmd2` under `DRM_MODE_FB_MODIFIERS`. weston's drm-backend
/// checks this cap before switching to modifier-aware buffer allocation
/// via GBM. We accept the cap because our ADDFB2 path reads the
/// `modifier[]` array and validates every entry against the set we
/// advertise in the plane's `IN_FORMATS` blob.
pub const DRM_CAP_ADDFB2_MODIFIERS: u64 = 0x10;

/// `DRM_MODE_FB_MODIFIERS` — caller is providing `modifier[]` entries.
/// Without this flag the `modifier[]` array in `drm_mode_fb_cmd2` is
/// ignored and assumed implicit-linear.
pub const DRM_MODE_FB_MODIFIERS: u32 = 0x2;

/// `DRM_FORMAT_MOD_INVALID` — sentinel used in `IN_FORMATS` by
/// pre-modifier drivers. userspace interprets it as "driver doesn't
/// care; implicit modifier".
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
/// `DRM_FORMAT_MOD_LINEAR` — the plain row-major layout. The only
/// modifier we advertise; virtio-gpu resources are always linear.
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

// ---- DRM_IOCTL_SET_CLIENT_CAP ----
//
// Userspace asks the kernel to enable a per-client behavior (e.g.
// UNIVERSAL_PLANES, ATOMIC).  The kernel either accepts (returns 0) or
// refuses (returns EOPNOTSUPP / EINVAL).  All the caps we currently
// support are accept-and-ignore (the behaviors they gate aren't in the
// fast path yet).

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmSetClientCap {
    pub capability: u64,
    pub value: u64,
}

// ======== modesetting ioctls ========
//
// All `MODE_*` commands live at nr ≥ 0xA0.

pub const DRM_IOCTL_MODE_GETRESOURCES: u32 = iowr::<DrmModeCardRes>(DRM_TYPE, 0xA0);
pub const DRM_IOCTL_MODE_GETCRTC: u32 = iowr::<DrmModeCrtc>(DRM_TYPE, 0xA1);
pub const DRM_IOCTL_MODE_SETCRTC: u32 = iowr::<DrmModeCrtc>(DRM_TYPE, 0xA2);
pub const DRM_IOCTL_MODE_GETENCODER: u32 = iowr::<DrmModeGetEncoder>(DRM_TYPE, 0xA6);
pub const DRM_IOCTL_MODE_GETCONNECTOR: u32 = iowr::<DrmModeGetConnector>(DRM_TYPE, 0xA7);
pub const DRM_IOCTL_MODE_GETPROPERTY: u32 = iowr::<DrmModeGetProperty>(DRM_TYPE, 0xAA);
pub const DRM_IOCTL_MODE_RMFB: u32 = iowr::<u32>(DRM_TYPE, 0xAF);
pub const DRM_IOCTL_MODE_PAGE_FLIP: u32 = iowr::<DrmModeCrtcPageFlip>(DRM_TYPE, 0xB0);
pub const DRM_IOCTL_MODE_CREATE_DUMB: u32 = iowr::<DrmModeCreateDumb>(DRM_TYPE, 0xB2);
pub const DRM_IOCTL_MODE_MAP_DUMB: u32 = iowr::<DrmModeMapDumb>(DRM_TYPE, 0xB3);
pub const DRM_IOCTL_MODE_DESTROY_DUMB: u32 = iowr::<DrmModeDestroyDumb>(DRM_TYPE, 0xB4);
pub const DRM_IOCTL_MODE_GETPLANERESOURCES: u32 = iowr::<DrmModeGetPlaneRes>(DRM_TYPE, 0xB5);
pub const DRM_IOCTL_MODE_GETPLANE: u32 = iowr::<DrmModeGetPlane>(DRM_TYPE, 0xB6);
pub const DRM_IOCTL_MODE_ADDFB2: u32 = iowr::<DrmModeFbCmd2>(DRM_TYPE, 0xB8);
pub const DRM_IOCTL_MODE_OBJ_GETPROPERTIES: u32 = iowr::<DrmModeObjGetProperties>(DRM_TYPE, 0xB9);
pub const DRM_IOCTL_MODE_ATOMIC: u32 = iowr::<DrmModeAtomic>(DRM_TYPE, 0xBC);
pub const DRM_IOCTL_MODE_CREATEPROPBLOB: u32 = iowr::<DrmModeCreateBlob>(DRM_TYPE, 0xBD);
pub const DRM_IOCTL_MODE_DESTROYPROPBLOB: u32 = iowr::<DrmModeDestroyBlob>(DRM_TYPE, 0xBE);
pub const DRM_IOCTL_MODE_GETPROPBLOB: u32 = iowr::<DrmModeGetBlob>(DRM_TYPE, 0xAC);
// WAIT_VBLANK is a union of request/reply, size = 24 bytes on 64-bit.
pub const DRM_IOCTL_WAIT_VBLANK: u32 = ioc(
    IOC_READ | IOC_WRITE,
    DRM_TYPE,
    0x3A,
    core::mem::size_of::<DrmWaitVblank>() as u16,
);

/// 32 bytes — Linux's `DRM_DISPLAY_MODE_LEN`.
pub const DRM_MODE_NAME_LEN: usize = 32;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeCardRes {
    /// user ptr to array of u32 fb ids
    pub fb_id_ptr: u64,
    /// user ptr to array of u32 crtc ids
    pub crtc_id_ptr: u64,
    /// user ptr to array of u32 connector ids
    pub connector_id_ptr: u64,
    /// user ptr to array of u32 encoder ids
    pub encoder_id_ptr: u64,
    pub count_fbs: u32,
    pub count_crtcs: u32,
    pub count_connectors: u32,
    pub count_encoders: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeModeInfo {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub kind: u32,
    pub name: [u8; DRM_MODE_NAME_LEN],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeCrtc {
    /// user ptr to set-of connector ids (on SETCRTC)
    pub set_connectors_ptr: u64,
    pub count_connectors: u32,
    pub crtc_id: u32,
    pub fb_id: u32,
    pub x: u32,
    pub y: u32,
    pub gamma_size: u32,
    pub mode_valid: u32,
    pub mode: DrmModeModeInfo,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeGetEncoder {
    pub encoder_id: u32,
    pub encoder_type: u32,
    pub crtc_id: u32,
    pub possible_crtcs: u32,
    pub possible_clones: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeGetConnector {
    pub encoders_ptr: u64,
    pub modes_ptr: u64,
    pub props_ptr: u64,
    pub prop_values_ptr: u64,
    pub count_modes: u32,
    pub count_props: u32,
    pub count_encoders: u32,
    pub encoder_id: u32,
    pub connector_id: u32,
    pub connector_type: u32,
    pub connector_type_id: u32,
    pub connection: u32,
    pub mm_width: u32,
    pub mm_height: u32,
    pub subpixel: u32,
    pub pad: u32,
}

/// Linux's `DRM_MODE_CONNECTED`.
pub const DRM_MODE_CONNECTED: u32 = 1;
/// `DRM_MODE_CONNECTOR_VIRTUAL` — we advertise a single virtual connector
/// since we're not on real hardware.
pub const DRM_MODE_CONNECTOR_VIRTUAL: u32 = 15;
/// Encoder type VIRTUAL.
pub const DRM_MODE_ENCODER_VIRTUAL: u32 = 5;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeFbCmd2 {
    pub fb_id: u32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub flags: u32,
    pub handles: [u32; 4],
    pub pitches: [u32; 4],
    pub offsets: [u32; 4],
    pub modifier: [u64; 4],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeCreateDumb {
    pub height: u32,
    pub width: u32,
    pub bpp: u32,
    pub flags: u32,
    pub handle: u32,
    pub pitch: u32,
    pub size: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeMapDumb {
    pub handle: u32,
    pub pad: u32,
    pub offset: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeDestroyDumb {
    pub handle: u32,
}

/// XRGB8888 — four bytes per pixel, little-endian, X/R/G/B in low-to-high.
pub const DRM_FORMAT_XRGB8888: u32 =
    (b'X' as u32) | ((b'R' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);
/// ARGB8888 — same layout but with meaningful alpha.
pub const DRM_FORMAT_ARGB8888: u32 =
    (b'A' as u32) | ((b'R' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);

// ======== M4b: planes, properties, page flip, vblank ========

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeGetPlaneRes {
    /// user ptr to array of u32 plane ids
    pub plane_id_ptr: u64,
    pub count_planes: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeGetPlane {
    pub plane_id: u32,
    pub crtc_id: u32,
    pub fb_id: u32,
    pub possible_crtcs: u32,
    pub gamma_size: u32,
    pub count_format_types: u32,
    /// user ptr to u32 array of supported formats
    pub format_type_ptr: u64,
}

/// `DRM_MODE_OBJECT_*` — type tags for `OBJ_GETPROPERTIES` and atomic
/// commits.  Values match Linux's uapi exactly; weston/modetest pattern-
/// match on them.
pub const DRM_MODE_OBJECT_CRTC: u32 = 0xcccc_cccc;
pub const DRM_MODE_OBJECT_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const DRM_MODE_OBJECT_PLANE: u32 = 0xeeee_eeee;

pub const DRM_PLANE_TYPE_PRIMARY: u64 = 1;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeObjGetProperties {
    /// user ptr to u32 array of property ids
    pub props_ptr: u64,
    /// user ptr to u64 array of property values (parallel to props_ptr)
    pub prop_values_ptr: u64,
    pub count_props: u32,
    pub obj_id: u32,
    pub obj_type: u32,
}

/// Property-flag bits (`DRM_MODE_PROP_*`).  Only the values we actually
/// tag properties with.
pub const DRM_MODE_PROP_RANGE: u32 = 1 << 1;
pub const DRM_MODE_PROP_IMMUTABLE: u32 = 1 << 2;
pub const DRM_MODE_PROP_ENUM: u32 = 1 << 3;
pub const DRM_MODE_PROP_BLOB: u32 = 1 << 4;
pub const DRM_MODE_PROP_OBJECT: u32 = 1 << 6;
pub const DRM_MODE_PROP_ATOMIC: u32 = 0x8000_0000;

/// `DRM_PROP_NAME_LEN` from Linux uapi.
pub const DRM_PROP_NAME_LEN: usize = 32;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModePropertyEnum {
    pub value: u64,
    pub name: [u8; DRM_PROP_NAME_LEN],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeGetProperty {
    /// user ptr to u64 array of range limits (RANGE props) or enum values
    pub values_ptr: u64,
    /// user ptr to array of `DrmModePropertyEnum` (ENUM/BITMASK props)
    pub enum_blob_ptr: u64,
    pub prop_id: u32,
    pub flags: u32,
    pub name: [u8; DRM_PROP_NAME_LEN],
    pub count_values: u32,
    pub count_enum_blobs: u32,
}

// ---- page flip ----

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeCrtcPageFlip {
    pub crtc_id: u32,
    pub fb_id: u32,
    pub flags: u32,
    pub reserved: u32,
    pub user_data: u64,
}

pub const DRM_MODE_PAGE_FLIP_EVENT: u32 = 0x01;

// ---- wait vblank ----
//
// Userspace hands us a `union drm_wait_vblank` — request on input, reply
// on output.  Request and reply are the same size (24 bytes on 64-bit);
// we just overlay the reply when writing back.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmWaitVblank {
    pub rep_type: u32,
    pub sequence: u32,
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `drm_wait_vblank.type` high bits — bit 0 distinguishes relative
/// (count vblanks from now) vs absolute (wait until the counter hits a
/// specific target). Linux `include/uapi/drm/drm.h` defines:
/// `_DRM_VBLANK_ABSOLUTE = 0`, `_DRM_VBLANK_RELATIVE = 1`. The low bits
/// of `type` are a CRTC index (unused here — we have one CRTC).
pub const DRM_VBLANK_RELATIVE: u32 = 0x1;

// ---- event delivery ----
//
// Page-flip completion events are delivered by reading the DRM fd.  Each
// event begins with a `drm_event` header (type + total length); the
// concrete payload type tells userspace what struct to expect.  We only
// ever emit `drm_event_vblank`.

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmEvent {
    pub event_type: u32,
    pub length: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmEventVblank {
    pub base: DrmEvent,
    pub user_data: u64,
    pub tv_sec: u32,
    pub tv_usec: u32,
    pub sequence: u32,
    pub crtc_id: u32,
}

pub const DRM_EVENT_FLIP_COMPLETE: u32 = 0x02;

// ======== M4c: atomic KMS + property blobs ========

/// `DRM_IOCTL_MODE_ATOMIC` payload.  Userspace batches up
/// `(object_id, prop_id, value)` tuples across multiple KMS objects; the
/// kernel validates them all, optionally applies the commit, and either
/// succeeds or rolls back atomically.
///
/// Arrays are "flat" — `objs_ptr` has `count_objs` entries, and the
/// `props_ptr` / `prop_values_ptr` arrays together have
/// `sum(count_props_ptr[0..count_objs])` entries, consumed in order.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeAtomic {
    pub flags: u32,
    pub count_objs: u32,
    pub objs_ptr: u64,
    pub count_props_ptr: u64,
    pub props_ptr: u64,
    pub prop_values_ptr: u64,
    pub reserved: u64,
    pub user_data: u64,
}

/// Atomic-ioctl flag bits (`DRM_MODE_ATOMIC_*`).  The page-flip bits
/// share numbering with `DRM_MODE_PAGE_FLIP_*` because an atomic commit
/// that moves FB_ID on a plane IS a page flip.
pub const DRM_MODE_ATOMIC_TEST_ONLY: u32 = 0x0100;
pub const DRM_MODE_ATOMIC_NONBLOCK: u32 = 0x0200;
pub const DRM_MODE_ATOMIC_ALLOW_MODESET: u32 = 0x0400;

// ---- blob properties ----
//
// Userspace allocates a kernel-side blob with `CREATEPROPBLOB`, gets
// back a u32 blob_id, then hands that id to whatever consumer wants it
// (e.g. the CRTC's MODE_ID property in an atomic commit).
// `GETPROPBLOB` reads the stored bytes back.

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeCreateBlob {
    /// user ptr to the source bytes
    pub data: u64,
    pub length: u32,
    /// kernel writes the allocated blob id here
    pub blob_id: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeDestroyBlob {
    pub blob_id: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmModeGetBlob {
    pub blob_id: u32,
    pub length: u32,
    /// user ptr the kernel writes the blob bytes to (truncated to `length`)
    pub data: u64,
}
