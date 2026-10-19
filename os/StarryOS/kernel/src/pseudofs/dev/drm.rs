//! DRM ioctl decoding helpers and userspace struct definitions.
//!
//! See Linux's `include/uapi/drm/drm.h` for the canonical definitions —
//! everything here is layout-compatible with that header. This module holds
//! core DRM ABI shared by the device nodes; VirtIO GPU private ABI lives in
//! `card0/virtgpu_uapi.rs`.

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
pub(super) const fn iowr<T>(ty: u8, nr: u8) -> u32 {
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
#[cfg(feature = "rknpu")]
pub const DRM_COMMAND_BASE: u32 = 0x40;
/// One past the highest driver-specific DRM command number
/// (`DRM_COMMAND_END`).
#[cfg(feature = "rknpu")]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVersion {
    pub version_major: c_int,
    pub version_minor: c_int,
    pub version_patchlevel: c_int,
    /// Explicit 64-bit ABI alignment bytes before the pointer-sized fields.
    pub _padding: u32,
    pub name_len: usize,
    pub name: u64,
    pub date_len: usize,
    pub date: u64,
    pub desc_len: usize,
    pub desc: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmUnique {
    pub unique_len: usize,
    pub unique: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
/// Syncobj objects are supported (`DRM_IOCTL_SYNCOBJ_*` family served).
pub const DRM_CAP_SYNCOBJ: u64 = 0x13;
/// Timeline syncobjs are supported (points on every wait/query/signal).
///
/// The value must match the UAPI headers the guest userspace was compiled
/// against: mesa's DRM sync provider selects the kernel syncobj path from
/// this cap, and a stale value makes it silently fall back to the sync-file
/// path, which the venus face does not honour.
pub const DRM_CAP_SYNCOBJ_TIMELINE: u64 = 0x14;
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
/// Legacy `ADDFB` (28-byte `drm_mode_fb_cmd`). GBM userspaces issue it for
/// plain buffers; the card0 handler synthesizes the `ADDFB2` core from it.
pub const DRM_IOCTL_MODE_ADDFB: u32 = iowr::<DrmModeFbCmd>(DRM_TYPE, 0xAE);
pub const DRM_IOCTL_MODE_OBJ_GETPROPERTIES: u32 = iowr::<DrmModeObjGetProperties>(DRM_TYPE, 0xB9);
pub const DRM_IOCTL_MODE_ATOMIC: u32 = iowr::<DrmModeAtomic>(DRM_TYPE, 0xBC);
pub const DRM_IOCTL_MODE_CREATEPROPBLOB: u32 = iowr::<DrmModeCreateBlob>(DRM_TYPE, 0xBD);
pub const DRM_IOCTL_MODE_DESTROYPROPBLOB: u32 = iowr::<DrmModeDestroyBlob>(DRM_TYPE, 0xBE);
pub const DRM_IOCTL_MODE_GETPROPBLOB: u32 = iowr::<DrmModeGetBlob>(DRM_TYPE, 0xAC);
pub const DRM_IOCTL_WAIT_VBLANK: u32 = ioc(
    IOC_READ | IOC_WRITE,
    DRM_TYPE,
    0x3A,
    core::mem::size_of::<DrmWaitVblank>() as u16,
);
pub const DRM_IOCTL_CRTC_GET_SEQUENCE: u32 = iowr::<DrmModeCrtcGetSequence>(DRM_TYPE, 0x3b);
pub const DRM_IOCTL_CRTC_QUEUE_SEQUENCE: u32 = iowr::<DrmModeCrtcQueueSequence>(DRM_TYPE, 0x3c);

/// 32 bytes — Linux's `DRM_DISPLAY_MODE_LEN`.
pub const DRM_MODE_NAME_LEN: usize = 32;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeGetEncoder {
    pub encoder_id: u32,
    pub encoder_type: u32,
    pub crtc_id: u32,
    pub possible_crtcs: u32,
    pub possible_clones: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
pub const DRM_MODE_DISCONNECTED: u32 = 2;
pub const DRM_MODE_CONNECTOR_UNKNOWN: u32 = 0;
pub const DRM_MODE_CONNECTOR_VGA: u32 = 1;
pub const DRM_MODE_CONNECTOR_DISPLAYPORT: u32 = 10;
pub const DRM_MODE_CONNECTOR_HDMIA: u32 = 11;
pub const DRM_MODE_CONNECTOR_EDP: u32 = 14;
pub const DRM_MODE_CONNECTOR_VIRTUAL: u32 = 15;
pub const DRM_MODE_ENCODER_NONE: u32 = 0;
pub const DRM_MODE_ENCODER_DAC: u32 = 1;
pub const DRM_MODE_ENCODER_TMDS: u32 = 2;
pub const DRM_MODE_ENCODER_LVDS: u32 = 3;
pub const DRM_MODE_ENCODER_VIRTUAL: u32 = 5;

/// Legacy framebuffer addition request.
/// Linux: `struct drm_mode_fb_cmd` (28 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeFbCmd {
    /// Output: framebuffer id.
    pub fb_id: u32,
    pub width: u32,
    pub height: u32,
    /// `DRM_FORMAT_HEADER` depth (bpp/depth pair packed by userspace).
    pub pitch: u32,
    pub bpp: u32,
    pub depth: u32,
    /// Input: GEM handle of the single plane.
    pub handle: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeFbCmd2 {
    pub fb_id: u32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub flags: u32,
    pub handles: [u32; 4],
    pub pitches: [u32; 4],
    pub offsets: [u32; 4],
    /// Explicit ABI alignment bytes before the modifier array.
    pub _padding: u32,
    pub modifier: [u64; 4],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeMapDumb {
    pub handle: u32,
    pub pad: u32,
    pub offset: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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

/// Linux DRM fourcc formats supported by the portable GPU interface.
pub const DRM_FORMAT_RGB565: u32 =
    (b'R' as u32) | ((b'G' as u32) << 8) | ((b'1' as u32) << 16) | ((b'6' as u32) << 24);
pub const DRM_FORMAT_RGB888: u32 =
    (b'R' as u32) | ((b'G' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);
pub const DRM_FORMAT_BGR888: u32 =
    (b'B' as u32) | ((b'G' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);
/// XRGB8888 — four bytes per pixel, little-endian, X/R/G/B in low-to-high.
pub const DRM_FORMAT_XRGB8888: u32 =
    (b'X' as u32) | ((b'R' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);
/// ARGB8888 — same layout but with meaningful alpha.
pub const DRM_FORMAT_ARGB8888: u32 =
    (b'A' as u32) | ((b'R' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);
pub const DRM_FORMAT_XBGR8888: u32 =
    (b'X' as u32) | ((b'B' as u32) << 8) | ((b'2' as u32) << 16) | ((b'4' as u32) << 24);

// ======== M4b: planes, properties, page flip, vblank ========

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeGetPlaneRes {
    /// user ptr to array of u32 plane ids
    pub plane_id_ptr: u64,
    pub count_planes: u32,
    /// Explicit tail bytes in the 64-bit DRM ABI.
    pub _padding: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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

/// Linux `drm_mode.h` object type tags used by object queries and
/// OBJECT property metadata.
pub const DRM_MODE_OBJECT_CRTC: u32 = 0xcccc_cccc;
pub const DRM_MODE_OBJECT_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const DRM_MODE_OBJECT_PLANE: u32 = 0xeeee_eeee;
pub const DRM_MODE_OBJECT_FB: u32 = 0xfbfb_fbfb;

pub const DRM_PLANE_TYPE_PRIMARY: u64 = 1;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeObjGetProperties {
    /// user ptr to u32 array of property ids
    pub props_ptr: u64,
    /// user ptr to u64 array of property values (parallel to props_ptr)
    pub prop_values_ptr: u64,
    pub count_props: u32,
    pub obj_id: u32,
    pub obj_type: u32,
    /// Explicit tail bytes in the 64-bit DRM ABI.
    pub _padding: u32,
}

/// Property-flag bits (`DRM_MODE_PROP_*`).  Only the values we actually
/// tag properties with.
pub const DRM_MODE_PROP_RANGE: u32 = 1 << 1;
pub const DRM_MODE_PROP_IMMUTABLE: u32 = 1 << 2;
pub const DRM_MODE_PROP_ENUM: u32 = 1 << 3;
pub const DRM_MODE_PROP_BLOB: u32 = 1 << 4;
pub const DRM_MODE_PROP_OBJECT: u32 = 1 << 6;
pub const DRM_MODE_PROP_SIGNED_RANGE: u32 = 1 << 7;
pub const DRM_MODE_PROP_ATOMIC: u32 = 0x8000_0000;

/// `DRM_PROP_NAME_LEN` from Linux uapi.
pub const DRM_PROP_NAME_LEN: usize = 32;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModePropertyEnum {
    pub value: u64,
    pub name: [u8; DRM_PROP_NAME_LEN],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
/// `_DRM_VBLANK_EVENT` — deliver a `DRM_EVENT_VBLANK` on the fd instead
/// of blocking inside the ioctl.
pub const DRM_VBLANK_EVENT: u32 = 0x400_0000;
/// `_DRM_VBLANK_NEXTONMISS` — if the target already passed, jump to the
/// next edge instead of firing immediately.
pub const DRM_VBLANK_NEXTONMISS: u32 = 0x1000_0000;
/// `_DRM_VBLANK_SECONDARY` — select the second CRTC (none on this card).
pub const DRM_VBLANK_SECONDARY: u32 = 0x2000_0000;
/// `_DRM_VBLANK_SIGNAL` — signal-based delivery, rejected as EINVAL by
/// modern Linux.
pub const DRM_VBLANK_SIGNAL: u32 = 0x4000_0000;
/// `_DRM_VBLANK_HIGH_CRTC_MASK` — bits 1..6 carry a CRTC index beyond
/// the secondary bit.
pub const DRM_VBLANK_HIGH_CRTC_MASK: u32 = 0x3e;
/// `_DRM_VBLANK_TYPES_MASK` — the absolute/relative selector bits.
pub const DRM_VBLANK_TYPES_MASK: u32 = DRM_VBLANK_RELATIVE;
/// `_DRM_VBLANK_FLAGS_MASK` — every flag bit of `type`.
pub const DRM_VBLANK_FLAGS_MASK: u32 =
    DRM_VBLANK_EVENT | DRM_VBLANK_SIGNAL | DRM_VBLANK_SECONDARY | DRM_VBLANK_NEXTONMISS;

// ---- CRTC vblank sequence clock (Linux 4.19+ `drm_crtc_get_sequence`) ----

/// `DRM_IOCTL_CRTC_GET_SEQUENCE` payload: query the current vblank
/// sequence and the timestamp of its edge.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeCrtcGetSequence {
    pub crtc_id: u32,
    /// Return: 1 while the CRTC is actively scanning out.
    pub active: u32,
    /// Return: most recent vblank sequence.
    pub sequence: u64,
    /// Return: time of the sequence's first pixel out, in
    /// `CLOCK_MONOTONIC` nanoseconds.
    pub sequence_ns: i64,
}

/// `DRM_IOCTL_CRTC_QUEUE_SEQUENCE` payload: deliver a
/// `DRM_EVENT_CRTC_SEQUENCE` when the counter reaches `sequence`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeCrtcQueueSequence {
    pub crtc_id: u32,
    pub flags: u32,
    /// Input: target sequence. Output: target (or the actual counter
    /// when the target had already passed).
    pub sequence: u64,
    pub user_data: u64,
}

/// `DRM_CRTC_SEQUENCE_RELATIVE` — `sequence` counts from the current
/// counter value.
pub const DRM_CRTC_SEQUENCE_RELATIVE: u32 = 0x0000_0001;
/// `DRM_CRTC_SEQUENCE_NEXT_ON_MISS` — use the next edge if the target
/// was missed.
pub const DRM_CRTC_SEQUENCE_NEXT_ON_MISS: u32 = 0x0000_0002;

// ---- event delivery ----
//
// Display events are delivered by reading the DRM fd. Each event begins
// with a `drm_event` header (type + total length); the event type selects
// either a vblank/page-flip payload or a CRTC sequence payload.

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

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmEventCrtcSequence {
    pub base: DrmEvent,
    pub user_data: u64,
    /// `CLOCK_MONOTONIC` nanoseconds of the delivery edge.
    pub tv_ns: i64,
    pub sequence: u64,
}

pub const DRM_EVENT_VBLANK: u32 = 0x01;
pub const DRM_EVENT_FLIP_COMPLETE: u32 = 0x02;
pub const DRM_EVENT_CRTC_SEQUENCE: u32 = 0x03;

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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
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
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeCreateBlob {
    /// user ptr to the source bytes
    pub data: u64,
    pub length: u32,
    /// kernel writes the allocated blob id here
    pub blob_id: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeDestroyBlob {
    pub blob_id: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmModeGetBlob {
    pub blob_id: u32,
    pub length: u32,
    /// user ptr the kernel writes the blob bytes to (truncated to `length`)
    pub data: u64,
}

// ---- Generic-DRM syncobj family (core ioctls 0xBF..0xCF) ----
//
// Mesa's venus ICD tracks GPU completion through timeline syncobjs
// (util_sync_provider_drm selects the kernel path when DRM_CAP_SYNCOBJ_TIMELINE
// is reported), so card0 serves the family alongside the VIRTGPU_* ioctls.
// The MODE_* ioctl nrs stop at 0xBE, so nothing collides.

/// Creates a syncobj. Linux: `struct drm_syncobj_create` (8 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjCreate {
    /// Output: syncobj handle.
    pub handle: u32,
    /// Input: `DRM_SYNCOBJ_CREATE_*` flags.
    pub flags: u32,
}

/// Destroys a syncobj. Linux: `struct drm_syncobj_destroy` (8 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjDestroy {
    /// Input: syncobj handle.
    pub handle: u32,
    pub pad: u32,
}

/// Exports/imports a syncobj as a sync_file fd.
/// `struct drm_syncobj_handle` as the userspace headers define it for this
/// stack: 24 bytes, with a trailing timeline point (the new-style
/// handle-to-fd export carries the point to export at; 0 means the binary
/// view of the syncobj).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
    pub pad: u32,
    pub point: u64,
}

/// Waits on binary syncobjs. Linux: `struct drm_syncobj_wait` (40 bytes:
/// handles, timeout_nsec, count_handles, flags, first_signaled, pad,
/// deadline_nsec).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjWait {
    /// Pointer to a __u32 handle array.
    pub handles: u64,
    /// Absolute CLOCK_MONOTONIC deadline in nanoseconds; a value that
    /// already lapsed degenerates to a single poll (Linux
    /// `drm_timeout_abs_to_jiffies`).
    pub timeout_nsec: i64,
    /// Number of handles in the array.
    pub count_handles: u32,
    /// `DRM_SYNCOBJ_WAIT_FLAGS_*`.
    pub flags: u32,
    /// Output: index of the first signaled handle (valid on a successful
    /// non-WAIT_ALL return only).
    pub first_signaled: u32,
    pub pad: u32,
    /// Optional per-wait deadline hint forwarded to the fences when
    /// WAIT_DEADLINE is set (Linux master, post-v7.1); accepted and unused.
    pub deadline_nsec: u64,
}

/// Resets/signals binary syncobjs. Linux: `struct drm_syncobj_array` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjArray {
    pub handles: u64,
    pub count_handles: u32,
    pub flags: u32,
}

/// Queries/signals timeline syncobjs.
/// Linux: `struct drm_syncobj_timeline_array` (24 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjTimelineArray {
    pub handles: u64,
    /// Pointer to a __u64 point array.
    pub points: u64,
    pub count_handles: u32,
    pub flags: u32,
}

/// Waits on timeline syncobjs. Linux: `struct drm_syncobj_timeline_wait`
/// (48 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjTimelineWait {
    pub handles: u64,
    pub points: u64,
    /// Absolute CLOCK_MONOTONIC deadline (`__s64` in Linux: negative values
    /// read as already lapsed).
    pub timeout_nsec: i64,
    pub count_handles: u32,
    pub flags: u32,
    /// Output: index of the first signaled handle (valid on a successful
    /// non-WAIT_ALL return only).
    pub first_signaled: u32,
    pub pad: u32,
    /// Optional per-wait deadline hint forwarded to the fences when
    /// WAIT_DEADLINE is set (Linux master, post-v7.1); accepted and unused.
    pub deadline_nsec: u64,
}

/// Registers an eventfd to be signalled when a syncobj point completes.
/// Linux: `struct drm_syncobj_eventfd` (24 bytes, v6.7 UAPI).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmSyncobjEventfd {
    pub handle: u32,
    /// `DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE` waits for a fence to become
    /// available rather than for the point to signal.
    pub flags: u32,
    /// Timeline point (0 for binary syncobjs).
    pub point: u64,
    /// Existing eventfd to signal.
    pub fd: i32,
    pub pad: u32,
}

pub const DRM_IOCTL_SYNCOBJ_CREATE: u32 = iowr::<DrmSyncobjCreate>(DRM_TYPE, 0xBF);
pub const DRM_IOCTL_SYNCOBJ_DESTROY: u32 = iowr::<DrmSyncobjDestroy>(DRM_TYPE, 0xC0);
pub const DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD: u32 = iowr::<DrmSyncobjHandle>(DRM_TYPE, 0xC1);
pub const DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE: u32 = iowr::<DrmSyncobjHandle>(DRM_TYPE, 0xC2);
pub const DRM_IOCTL_SYNCOBJ_WAIT: u32 = iowr::<DrmSyncobjWait>(DRM_TYPE, 0xC3);
pub const DRM_IOCTL_SYNCOBJ_RESET: u32 = iowr::<DrmSyncobjArray>(DRM_TYPE, 0xC4);
pub const DRM_IOCTL_SYNCOBJ_SIGNAL: u32 = iowr::<DrmSyncobjArray>(DRM_TYPE, 0xC5);
pub const DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT: u32 = iowr::<DrmSyncobjTimelineWait>(DRM_TYPE, 0xCA);
pub const DRM_IOCTL_SYNCOBJ_QUERY: u32 = iowr::<DrmSyncobjTimelineArray>(DRM_TYPE, 0xCB);
pub const DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL: u32 =
    iowr::<DrmSyncobjTimelineArray>(DRM_TYPE, 0xCD);
pub const DRM_IOCTL_SYNCOBJ_EVENTFD: u32 = iowr::<DrmSyncobjEventfd>(DRM_TYPE, 0xCF);

/// `DRM_SYNCOBJ_CREATE_SIGNALED`: create the syncobj in the signaled state.
pub const DRM_SYNCOBJ_CREATE_SIGNALED: u32 = 1 << 0;
/// `DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL`: wait for every handle, not any one.
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL: u32 = 1 << 0;
/// `DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT`: allow waiting on a future point.
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT: u32 = 1 << 1;
/// `DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE`: wait for a fence to become
/// available at the point rather than for the point to signal.
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE: u32 = 1 << 2;
/// `DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE`: pass `deadline_nsec` as a
/// scheduling hint to the fences (v6.11+). Accepted and ignored here — it
/// never changes the wait predicate.
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE: u32 = 1 << 3;

/// `DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE`: export a sync file
/// fd instead of a syncobj fd.
pub const DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE: u32 = 1 << 0;
/// `DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE`: export the sync file at
/// `point` (v6.16+); without it `point` must be zero.
pub const DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE: u32 = 1 << 1;
/// `DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE`: import a sync file fd.
pub const DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE: u32 = 1 << 0;
/// `DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE`: import the fence at `point`
/// (v6.16+); without it `point` must be zero.
pub const DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE: u32 = 1 << 1;

/// `DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED`: QUERY reports the newest
/// attached point instead of the last completed one.
pub const DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED: u32 = 1 << 0;

#[cfg(all(test, not(axtest), feature = "rknpu"))]
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

/// Layout pins for the UAPI command words this file defines. Deliberately
/// outside the `rknpu`-gated `mod tests` (whose helpers are rknpu-only):
/// these assertions only touch always-compiled constants, and keeping them
/// in the default-feature test build makes `cargo test -p starry-kernel`
/// run them — the guest-side `_Static_assert`s in `drm-test-syncobj` are
/// the second, independent layer.
#[cfg(all(test, not(axtest)))]
mod uapi_layout_tests {
    use super::*;

    #[test]
    fn legacy_addfb_matches_linux_command_word() {
        // GBM userspaces issue the legacy ADDFB with the 28-byte
        // `drm_mode_fb_cmd`; the packed command must be the exact word
        // observed on real clients, or the dispatch never sees it. The
        // size field (bits 29..16) is decoded locally — `io_size` lives
        // behind the `rknpu` feature.
        assert_eq!(DRM_IOCTL_MODE_ADDFB, 0xc01c_64ae);
        assert_eq!(
            (DRM_IOCTL_MODE_ADDFB >> 16) & 0x3fff,
            core::mem::size_of::<DrmModeFbCmd>() as u32
        );
        assert_eq!(core::mem::size_of::<DrmModeFbCmd>(), 28);
    }

    #[test]
    fn syncobj_handle_to_fd_matches_userspace_command_word() {
        // Mesa's EGL native-fence path exports syncobjs with the 24-byte
        // (point-carrying) `drm_syncobj_handle`; the size is part of the
        // command word, so a 16-byte struct silently misses the dispatch.
        assert_eq!(DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, 0xc018_64c1);
        assert_eq!(core::mem::size_of::<DrmSyncobjHandle>(), 24);
        assert_eq!(core::mem::offset_of!(DrmSyncobjHandle, fd), 8);
        assert_eq!(core::mem::offset_of!(DrmSyncobjHandle, point), 16);
    }

    #[test]
    fn syncobj_wait_structs_match_linux_command_words() {
        // libdrm compiles `struct drm_syncobj_wait` from the Linux UAPI
        // header as 40 bytes — timeout_nsec sits before count_handles and
        // the trailing deadline_nsec hint rides along — and
        // `drm_syncobj_timeline_wait` as 48 bytes. The packed size is part
        // of the command word, so any layout drift silently misses the
        // dispatch (`drmSyncobjWait` gets ENOTTY from the default arm).
        // The offset pins catch same-size reorders a size check cannot.
        assert_eq!(DRM_IOCTL_SYNCOBJ_WAIT, 0xc028_64c3);
        assert_eq!(core::mem::size_of::<DrmSyncobjWait>(), 40);
        assert_eq!(core::mem::offset_of!(DrmSyncobjWait, timeout_nsec), 8);
        assert_eq!(core::mem::offset_of!(DrmSyncobjWait, count_handles), 16);
        assert_eq!(core::mem::offset_of!(DrmSyncobjWait, first_signaled), 24);
        assert_eq!(core::mem::offset_of!(DrmSyncobjWait, deadline_nsec), 32);
        assert_eq!(DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, 0xc030_64ca);
        assert_eq!(core::mem::size_of::<DrmSyncobjTimelineWait>(), 48);
        assert_eq!(core::mem::offset_of!(DrmSyncobjTimelineWait, points), 8);
        assert_eq!(core::mem::offset_of!(DrmSyncobjTimelineWait, timeout_nsec), 16);
        assert_eq!(
            core::mem::offset_of!(DrmSyncobjTimelineWait, count_handles),
            24
        );
        assert_eq!(
            core::mem::offset_of!(DrmSyncobjTimelineWait, deadline_nsec),
            40
        );
    }
}
