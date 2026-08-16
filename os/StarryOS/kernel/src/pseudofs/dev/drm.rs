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

// ---- ioctl-number decoding ----
//
// The inverse of the encode helpers above: pull the command number and
// struct size back out of a packed ioctl request. card1 (the RKNPU
// companion DRM node) dispatches on these because its driver-specific
// commands aren't known at compile time the way card0's fixed set is.

/// Lowest driver-specific DRM command number (`DRM_COMMAND_BASE`). Core
/// DRM commands live below this; modeset commands at or above 0xA0.
#[allow(dead_code)]
pub const DRM_COMMAND_BASE: u32 = 0x40;
/// One past the highest driver-specific DRM command number
/// (`DRM_COMMAND_END`).
#[allow(dead_code)]
pub const DRM_COMMAND_END: u32 = 0xA0;

/// Extracts the command number (bits 7..0) from a packed ioctl request.
#[cfg(feature = "rknpu")]
#[inline]
pub const fn ioctl_nr(cmd: u32) -> u32 {
    cmd & 0xff
}

/// Extracts the struct size (bits 29..16) from a packed ioctl request.
#[cfg(feature = "rknpu")]
#[inline]
pub const fn io_size(cmd: u32) -> u32 {
    (cmd >> 16) & ((1 << 14) - 1)
}

/// Returns `true` if `nr` falls in the driver-specific command range
/// (`DRM_COMMAND_BASE..DRM_COMMAND_END`).
#[cfg(feature = "rknpu")]
#[inline]
pub fn is_driver_ioctl(nr: u32) -> bool {
    (DRM_COMMAND_BASE..DRM_COMMAND_END).contains(&nr)
}

pub const DRM_TYPE: u8 = b'd';

pub const DRM_IOCTL_VERSION: u32 = iowr::<DrmVersion>(DRM_TYPE, 0x00);
pub const DRM_IOCTL_GET_UNIQUE: u32 = iowr::<DrmUnique>(DRM_TYPE, 0x01);
pub const DRM_IOCTL_SET_VERSION: u32 = iowr::<DrmSetVersion>(DRM_TYPE, 0x07);
pub const DRM_IOCTL_GET_CAP: u32 = iowr::<DrmGetCap>(DRM_TYPE, 0x0c);
/// GEM_CLOSE — release a GEM handle (pure input, WRITE direction).
/// Layout: `struct drm_gem_close { u32 handle; u32 pad; }` (8 bytes).
pub const DRM_IOCTL_GEM_CLOSE: u32 = ioc(
    IOC_WRITE,
    DRM_TYPE,
    0x09,
    core::mem::size_of::<DrmGemClose>() as u16,
);
pub const DRM_IOCTL_SET_CLIENT_CAP: u32 = ioc(
    IOC_WRITE,
    DRM_TYPE,
    0x0d,
    core::mem::size_of::<DrmSetClientCap>() as u16,
);
pub const DRM_IOCTL_SET_MASTER: u32 = io(DRM_TYPE, 0x1e);
pub const DRM_IOCTL_DROP_MASTER: u32 = io(DRM_TYPE, 0x1f);
pub const DRM_IOCTL_GET_MAGIC: u32 = iowr::<DrmAuth>(DRM_TYPE, 0x02);
pub const DRM_IOCTL_AUTH_MAGIC: u32 = iowr::<DrmAuth>(DRM_TYPE, 0x03);
pub const DRM_IOCTL_MODE_DIRTYFB: u32 = iowr::<DrmModeDirtyFB>(DRM_TYPE, 0xB1);
pub const DRM_IOCTL_PRIME_HANDLE_TO_FD: u32 = iowr::<DrmPrimeHandle>(DRM_TYPE, 0x2d);
pub const DRM_IOCTL_PRIME_FD_TO_HANDLE: u32 = iowr::<DrmPrimeHandle>(DRM_TYPE, 0x2e);

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmAuth {
    pub magic: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeDirtyFB {
    pub fb_id: u32,
    pub flags: u32,
    pub color: u32,
    pub num_clips: u32,
    pub clips_ptr: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmPrimeHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
}

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
pub const DRM_CAP_PRIME: u64 = 0x5;
/// DRM PRIME capability bits for `DRM_CAP_PRIME` value — bitmask, not a bool.
pub const DRM_PRIME_CAP_IMPORT: u64 = 0x1;
pub const DRM_PRIME_CAP_EXPORT: u64 = 0x2;
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

/// `struct drm_gem_close` — GEM_CLOSE ioctl payload. Unlike
/// `DrmModeDestroyDumb`, this carries an explicit 8-byte layout
/// (`handle` + `pad`) so the packed ioctl number decodes with size 8,
/// matching `DRM_IOCTL_GEM_CLOSE = 0x40086409` as sent by Mesa/libdrm.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmGemClose {
    pub handle: u32,
    pub pad: u32,
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

// ======== virtgpu ioctls ========
//
// Private ioctls for virtio-gpu 3D (virgl) support. These live at
// DRM_COMMAND_BASE + N and are dispatched through the same ioctl match
// as core DRM commands.
//
// Reference: Linux v6.1 include/uapi/drm/virtgpu_drm.h
// Mesa: src/gallium/winsys/virgl/drm/virgl_drm_winsys.c

// ---- virtgpu ioctl numbers ----
// DRM ioctl encoding: DRM_IOWR(DRM_COMMAND_BASE + N, struct)
// where DRM_TYPE = b'd' (0x64)

pub const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr::<DrmVirtgpuMap>(DRM_TYPE, 0x41);
pub const DRM_IOCTL_VIRTGPU_EXECBUFFER: u32 = iowr::<DrmVirtgpuExecbuffer>(DRM_TYPE, 0x42);
pub const DRM_IOCTL_VIRTGPU_GETPARAM: u32 = iowr::<DrmVirtgpuGetparam>(DRM_TYPE, 0x43);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr::<DrmVirtgpuResourceCreate>(DRM_TYPE, 0x44);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 = iowr::<DrmVirtgpuResourceInfo>(DRM_TYPE, 0x45);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST: u32 =
    iowr::<DrmVirtgpu3dTransferFromHost>(DRM_TYPE, 0x46);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u32 =
    iowr::<DrmVirtgpu3dTransferToHost>(DRM_TYPE, 0x47);
pub const DRM_IOCTL_VIRTGPU_WAIT: u32 = iowr::<DrmVirtgpu3dWait>(DRM_TYPE, 0x48);
pub const DRM_IOCTL_VIRTGPU_GET_CAPS: u32 = iowr::<DrmVirtgpuGetCaps>(DRM_TYPE, 0x49);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB: u32 =
    iowr::<DrmVirtgpuResourceCreateBlob>(DRM_TYPE, 0x4a);
pub const DRM_IOCTL_VIRTGPU_CONTEXT_INIT: u32 = iowr::<DrmVirtgpuContextInit>(DRM_TYPE, 0x4b);

// ---- GETPARAM parameter IDs (VIRTGPU_PARAM_*) ----

/// Whether the device supports 3D features (virgl).
pub const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
/// Whether capset query fix is supported.
pub const VIRTGPU_PARAM_CAPSET_QUERY_FIX: u64 = 2;
/// Whether RESOURCE_CREATE_BLOB is supported.
pub const VIRTGPU_PARAM_RESOURCE_BLOB: u64 = 3;
/// Whether host-visible blob resources can be mmap'd.
pub const VIRTGPU_PARAM_HOST_VISIBLE: u64 = 4;
/// Whether cross-device resource sharing is supported.
pub const VIRTGPU_PARAM_CROSS_DEVICE: u64 = 5;
/// Whether CONTEXT_INIT is supported.
pub const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;
/// Bitmask of supported capset IDs.
pub const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;
/// User-space debug naming (v6.12+).
#[allow(dead_code)]
pub const VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME: u64 = 8;
/// Blob alignment requirement (v6.15+).
#[allow(dead_code)]
pub const VIRTGPU_PARAM_BLOB_ALIGNMENT: u64 = 9;

// ---- EXECBUFFER flags (VIRTGPU_EXECBUF_*) ----

/// fence_fd is an input dependency.
pub const VIRTGPU_EXECBUF_FENCE_FD_IN: u32 = 0x01;
/// fence_fd is an output completion signal.
pub const VIRTGPU_EXECBUF_FENCE_FD_OUT: u32 = 0x02;
/// Use ring_idx field.
#[allow(dead_code)]
pub const VIRTGPU_EXECBUF_RING_IDX: u32 = 0x04;
/// syncobj reset (v6.6+).
#[allow(dead_code)]
pub const VIRTGPU_EXECBUF_SYNCOBJ_RESET: u32 = 0x01;

// ---- WAIT flags ----

/// Non-blocking wait.
#[allow(dead_code)]
pub const VIRTGPU_WAIT_NOWAIT: u32 = 1;

// ---- CONTEXT parameters (VIRTGPU_CONTEXT_PARAM_*) ----

/// Capset ID for the context.
pub const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
/// Number of command rings.
pub const VIRTGPU_CONTEXT_PARAM_NUM_RINGS: u64 = 0x0002;
/// Poll rings mask.
pub const VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 0x0003;
/// Debug name (v6.12+).
#[allow(dead_code)]
pub const VIRTGPU_CONTEXT_PARAM_DEBUG_NAME: u64 = 0x0004;

// ---- CAPSET IDs (VIRTGPU_DRM_CAPSET_*) ----

/// Virgl capset v1.
pub const VIRTGPU_DRM_CAPSET_VIRGL: u32 = 1;
/// Virgl capset v2 (Mesa prefers this).
pub const VIRTGPU_DRM_CAPSET_VIRGL2: u32 = 2;
/// gfxstream Vulkan capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_GFXSTREAM_VULKAN: u32 = 3;
/// Venus (Vulkan) capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_VENUS: u32 = 4;
/// Cross-domain capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_CROSS_DOMAIN: u32 = 5;
/// DRM capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_DRM: u32 = 6;

// ---- BLOB memory types (VIRTGPU_BLOB_MEM_*) ----

/// Guest memory blob.
pub const VIRTGPU_BLOB_MEM_GUEST: u32 = 0x0001;
/// Host 3D memory blob (Mesa uses this).
pub const VIRTGPU_BLOB_MEM_HOST3D: u32 = 0x0002;
/// Host 3D + guest memory blob.
pub const VIRTGPU_BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;

// ---- BLOB flags (VIRTGPU_BLOB_FLAG_*) ----

/// Blob can be mmap'd (Mesa uses this).
#[allow(dead_code)]
pub const VIRTGPU_BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;
/// Blob can be shared.
#[allow(dead_code)]
pub const VIRTGPU_BLOB_FLAG_USE_SHAREABLE: u32 = 0x0002;
/// Blob can be used across devices.
#[allow(dead_code)]
pub const VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 0x0004;
/// Hint: defer mapping.
#[allow(dead_code)]
pub const DRM_VIRTGPU_BLOB_FLAG_HINT_DEFER_MAPPING: u32 = 0x0001;

// ---- Event codes ----

/// Fence signaled event.
#[allow(dead_code)]
pub const VIRTGPU_EVENT_FENCE_SIGNALED: u32 = 0x90000000;

// ---- virtgpu structs ----

/// Maps a GEM handle to an mmap offset.
/// Linux: `struct drm_virtgpu_map` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuMap {
    /// Output: offset for mmap system call.
    pub offset: u64,
    /// Input: GEM handle.
    pub handle: u32,
    pub pad: u32,
}

/// Submits a virgl command buffer to the host.
/// Linux: `struct drm_virtgpu_execbuffer` (v6.1: 40 bytes, v6.6+: 64 bytes)
///
/// We use the v6.1 layout (40 bytes) for maximum compatibility. Mesa virgl
/// does not use the syncobj fields added in v6.6+.
///
/// **Critical**: There is NO `ctx_id` field. The context is implicitly
/// bound to the file descriptor via CONTEXT_INIT.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuExecbuffer {
    /// VIRTGPU_EXECBUF_FENCE_FD_IN/OUT/RING_IDX flags.
    pub flags: u32,
    /// Command buffer size in bytes.
    pub size: u32,
    /// User-space pointer to command buffer.
    pub command: u64,
    /// User-space pointer to __u32 array of GEM handles.
    pub bo_handles: u64,
    /// Number of bo_handles.
    pub num_bo_handles: u32,
    /// Input/output fence fd (signed! default -1).
    pub fence_fd: i32,
    /// Command ring index (used when RING_IDX flag is set).
    pub ring_idx: u32,
    // ---- syncobj fields (Linux drm_virtgpu_execbuffer tail, 64B total) ----
    // 缺少这些字段会让 struct 只有 40 字节 → ioctl 号(size 位)与 mesa 的 64
    // 字节不一致 → EXECBUFFER 落不到 handler → "expect bad rendering 95"。
    /// Size of each @drm_virtgpu_execbuffer_syncobj.
    pub syncobj_stride: u32,
    /// Number of in syncobjs.
    pub num_in_syncobjs: u32,
    /// Number of out syncobjs.
    pub num_out_syncobjs: u32,
    /// Pointer to in syncobj array.
    pub in_syncobjs: u64,
    /// Pointer to out syncobj array.
    pub out_syncobjs: u64,
}

/// Queries a driver parameter.
/// Linux: `struct drm_virtgpu_getparam` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuGetparam {
    /// Input: parameter ID (VIRTGPU_PARAM_*).
    pub param: u64,
    /// Output: parameter value.
    pub value: u64,
}

/// Creates a 3D resource (texture, render target, buffer, etc.).
/// Linux: `struct drm_virtgpu_resource_create` (56 bytes)
///
/// **Critical**: `bo_handle` (input, GEM handle) and `res_handle` (output,
/// virtio-gpu resource ID) are DIFFERENT concepts. Mesa uses them
/// independently.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuResourceCreate {
    /// Input: target type (GL_TEXTURE_2D, etc.).
    pub target: u32,
    /// Input: PIPE_FORMAT_* pixel format.
    pub format: u32,
    /// Input: VIRGL_BIND_* binding flags.
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    /// Input: mipmap highest level (not "level").
    pub last_level: u32,
    /// Input: MSAA sample count (usually 0).
    pub nr_samples: u32,
    /// Input: resource flags.
    pub flags: u32,
    /// Input: associate with existing GEM BO (0 = kernel allocates new BO).
    pub bo_handle: u32,
    /// Output: virtio-gpu resource ID (NOT GEM handle!).
    pub res_handle: u32,
    /// Output: resource size (for transfer validation).
    pub size: u32,
    /// Input/Output: row stride.
    pub stride: u32,
}

/// Queries resource information.
/// Linux: `struct drm_virtgpu_resource_info` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuResourceInfo {
    /// Input: GEM handle.
    pub bo_handle: u32,
    /// Output: virtio-gpu resource ID.
    pub res_handle: u32,
    /// Output: resource size.
    pub size: u32,
    /// Output: blob memory type (0 for non-blob resources).
    pub blob_mem: u32,
}

/// 3D box region for data transfer operations.
/// Linux: `struct drm_virtgpu_3d_box` (24 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpu3dBox {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

/// Transfers data from guest to host for a 3D resource.
/// Linux: `struct drm_virtgpu_3d_transfer_to_host` (44 bytes)
///
/// **Critical**: `offset` is u32 (not u64), and the box is a NESTED
/// DrmVirtgpu3dBox struct (not 6 inline u32s).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpu3dTransferToHost {
    /// GEM handle.
    pub bo_handle: u32,
    /// Nested 3D box (24 bytes).
    pub box_: DrmVirtgpu3dBox,
    /// Mipmap level.
    pub level: u32,
    /// Buffer offset (u32, not u64!).
    pub offset: u32,
    /// Row stride.
    pub stride: u32,
    /// Layer stride.
    pub layer_stride: u32,
}

/// Transfers data from host to guest for a 3D resource.
/// Linux: `struct drm_virtgpu_3d_transfer_from_host` (44 bytes)
/// Layout identical to transfer_to_host.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpu3dTransferFromHost {
    /// GEM handle.
    pub bo_handle: u32,
    /// Nested 3D box (24 bytes).
    pub box_: DrmVirtgpu3dBox,
    /// Mipmap level.
    pub level: u32,
    /// Buffer offset (u32, not u64!).
    pub offset: u32,
    /// Row stride.
    pub stride: u32,
    /// Layer stride.
    pub layer_stride: u32,
}

/// Waits for a resource to become idle.
/// Linux: `struct drm_virtgpu_3d_wait` (8 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpu3dWait {
    /// GEM handle (0 is invalid).
    pub handle: u32,
    /// VIRTGPU_WAIT_NOWAIT etc.
    pub flags: u32,
}

/// Retrieves capability set data.
/// Linux: `struct drm_virtgpu_get_caps` (24 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuGetCaps {
    /// Input: capset ID (VIRTGPU_DRM_CAPSET_VIRGL=1, VIRGL2=2).
    pub cap_set_id: u32,
    /// Input: capset version.
    pub cap_set_ver: u32,
    /// Input: user-space buffer pointer.
    pub addr: u64,
    /// Input/Output: buffer size.
    pub size: u32,
    pub pad: u32,
}

/// Creates a blob resource for coherent memory sharing.
/// Linux: `struct drm_virtgpu_resource_create_blob` (48 bytes)
///
/// Mesa virgl calls this when `supports_coherent=true` (i.e. both
/// RESOURCE_BLOB and HOST_VISIBLE are reported as supported).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuResourceCreateBlob {
    /// VIRTGPU_BLOB_MEM_* (Mesa uses HOST3D).
    pub blob_mem: u32,
    /// VIRTGPU_BLOB_FLAG_* (Mesa uses USE_MAPPABLE).
    pub blob_flags: u32,
    /// Output: GEM handle.
    pub bo_handle: u32,
    /// Output: virtio-gpu resource ID.
    pub res_handle: u32,
    /// Blob size in bytes.
    pub size: u64,
    pub pad: u32,
    /// VIRGL command size in bytes.
    pub cmd_size: u32,
    /// User-space pointer to VIRGL_PIPE_RES_CREATE command.
    pub cmd: u64,
    /// Blob unique identifier.
    pub blob_id: u64,
}

/// Initializes a rendering context on this file descriptor.
/// Linux: `struct drm_virtgpu_context_init` (16 bytes)
///
/// **Critical**: This is a PURE INPUT struct — there is NO `ctx_id` output
/// field. The context is implicitly bound to the file descriptor. Each fd
/// can only call CONTEXT_INIT once (repeated calls return -EEXIST).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuContextInit {
    /// Input: number of parameters (kernel limits to max 3).
    pub num_params: u32,
    pub pad: u32,
    /// Input: pointer to DrmVirtgpuContextSetParam array.
    pub ctx_set_params: u64,
}

/// A single context parameter for CONTEXT_INIT.
/// Linux: `struct drm_virtgpu_context_set_param` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuContextSetParam {
    /// VIRTGPU_CONTEXT_PARAM_* constant.
    pub param: u64,
    /// Parameter value.
    pub value: u64,
}

/// Syncobj entry for execbuffer (v6.6+).
/// Linux: `struct drm_virtgpu_execbuffer_syncobj` (16 bytes)
/// Mesa virgl does not use this.
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuExecbufferSyncobj {
    pub handle: u32,
    pub flags: u32,
    pub point: u64,
}

#[cfg(all(test, feature = "rknpu"))]
mod tests {
    use super::*;

    #[test]
    fn ioctl_nr_recovers_command_number() {
        // The decode helpers are the inverse of the encode helpers: the
        // command number packed by `iowr`/`io` must round-trip back out.
        assert_eq!(ioctl_nr(DRM_IOCTL_VERSION), 0x00);
        assert_eq!(ioctl_nr(DRM_IOCTL_GET_UNIQUE), 0x01);
        assert_eq!(ioctl_nr(DRM_IOCTL_PRIME_HANDLE_TO_FD), 0x2d);
        assert_eq!(ioctl_nr(DRM_IOCTL_MODE_GETRESOURCES), 0xA0);
    }

    #[test]
    fn ioctl_nr_masks_to_low_byte() {
        // Direction and size occupy the high bits; nr is only bits 7..0.
        assert_eq!(ioctl_nr(0xc010_6402), 0x02);
    }

    #[test]
    fn io_size_recovers_struct_size() {
        // `iowr::<T>` encodes `size_of::<T>()` into bits 29..16.
        assert_eq!(
            io_size(DRM_IOCTL_VERSION),
            core::mem::size_of::<DrmVersion>() as u32
        );
        assert_eq!(
            io_size(DRM_IOCTL_GET_UNIQUE),
            core::mem::size_of::<DrmUnique>() as u32
        );
        // `io(..)` (no payload) encodes a zero size.
        assert_eq!(io_size(DRM_IOCTL_SET_MASTER), 0);
    }

    #[test]
    fn is_driver_ioctl_covers_command_range() {
        // Driver-specific commands live in DRM_COMMAND_BASE..DRM_COMMAND_END.
        assert!(!is_driver_ioctl(DRM_COMMAND_BASE - 1));
        assert!(is_driver_ioctl(DRM_COMMAND_BASE));
        assert!(is_driver_ioctl(DRM_COMMAND_END - 1));
        assert!(!is_driver_ioctl(DRM_COMMAND_END));

        // Core DRM (VERSION, nr 0) and modeset (GETRESOURCES, nr 0xA0) are
        // both outside the driver range; the RKNPU submit/mem commands
        // (nr 0x40..0x46) are inside it.
        assert!(!is_driver_ioctl(ioctl_nr(DRM_IOCTL_VERSION)));
        assert!(!is_driver_ioctl(ioctl_nr(DRM_IOCTL_MODE_GETRESOURCES)));
        assert!(is_driver_ioctl(0x40));
        assert!(is_driver_ioctl(0x45));
    }
}
