// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux v7.2.3 fbdev declarations from include/linux/fb.h and
// include/uapi/linux/fb.h, for the configured x86_64 build. CONFIG_FB_DEVICE
// and CONFIG_FB_DEFERRED_IO are enabled; CONFIG_FB_BACKLIGHT and
// CONFIG_FB_TILEBLITTING are disabled in the selected reference configuration.
//! Linux fbdev core types.
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_int, c_long, c_ulong, c_void},
    mem::{align_of, offset_of, size_of},
};

pub use drm_kapi::mm::VmAreaStruct;
use drm_kapi::types::{DelayedWork, ListHead, Mutex, Page, RefcountT};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FbBitfield {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

/// `struct fb_var_screeninfo` (include/uapi/linux/fb.h).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FbVarScreenInfo {
    pub xres: u32,
    pub yres: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub bits_per_pixel: u32,
    pub grayscale: u32,
    pub red: FbBitfield,
    pub green: FbBitfield,
    pub blue: FbBitfield,
    pub transp: FbBitfield,
    pub nonstd: u32,
    pub activate: u32,
    pub height: u32,
    pub width: u32,
    pub accel_flags: u32,
    pub pixclock: u32,
    pub left_margin: u32,
    pub right_margin: u32,
    pub upper_margin: u32,
    pub lower_margin: u32,
    pub hsync_len: u32,
    pub vsync_len: u32,
    pub sync: u32,
    pub vmode: u32,
    pub rotate: u32,
    pub colorspace: u32,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbFixScreenInfo {
    pub id: [c_char; 16],
    pub smem_start: c_ulong,
    pub smem_len: u32,
    pub type_: u32,
    pub type_aux: u32,
    pub visual: u32,
    pub xpanstep: u16,
    pub ypanstep: u16,
    pub ywrapstep: u16,
    pub line_length: u32,
    pub mmio_start: c_ulong,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    pub reserved: [u16; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbCmap {
    pub start: u32,
    pub len: u32,
    pub red: *mut u16,
    pub green: *mut u16,
    pub blue: *mut u16,
    pub transp: *mut u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbChroma {
    pub redx: u32,
    pub greenx: u32,
    pub bluex: u32,
    pub whitex: u32,
    pub redy: u32,
    pub greeny: u32,
    pub bluey: u32,
    pub whitey: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbMonSpecs {
    pub chroma: FbChroma,
    pub modedb: *mut FbVideoMode,
    pub manufacturer: [u8; 4],
    pub monitor: [u8; 14],
    pub serial_no: [u8; 14],
    pub ascii: [u8; 14],
    pub modedb_len: u32,
    pub model: u32,
    pub serial: u32,
    pub year: u32,
    pub week: u32,
    pub hfmin: u32,
    pub hfmax: u32,
    pub dclkmin: u32,
    pub dclkmax: u32,
    pub input: u16,
    pub dpms: u16,
    pub signal: u16,
    pub vfmin: u16,
    pub vfmax: u16,
    pub gamma: u16,
    /// The source is `u16 gtf:1`; the storage unit is retained explicitly.
    pub gtf: u16,
    pub misc: u16,
    pub version: u8,
    pub revision: u8,
    pub max_x: u8,
    pub max_y: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbPixmap {
    pub addr: *mut u8,
    pub size: u32,
    pub offset: u32,
    pub buf_align: u32,
    pub scan_align: u32,
    pub access_align: u32,
    pub flags: u32,
    /// `DECLARE_BITMAP(blit_x, FB_MAX_BLIT_WIDTH)` on x86_64, width 64.
    pub blit_x: [c_ulong; 1],
    /// `DECLARE_BITMAP(blit_y, FB_MAX_BLIT_HEIGHT)` on x86_64, width 128.
    pub blit_y: [c_ulong; 2],
    pub writeio: Option<unsafe extern "C" fn(*mut FbInfo, *mut c_void, *mut c_void, u32)>,
    pub readio: Option<unsafe extern "C" fn(*mut FbInfo, *mut c_void, *mut c_void, u32)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbDeferredIo {
    pub delay: c_ulong,
    pub sort_pagereflist: bool,
    pub get_page: Option<unsafe extern "C" fn(*mut FbInfo, c_ulong) -> *mut Page>,
    pub deferred_io: Option<unsafe extern "C" fn(*mut FbInfo, *mut ListHead)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbDeferredIoPageRef {
    pub page: *mut Page,
    pub offset: c_ulong,
    pub list: ListHead,
}

#[repr(C)]
pub struct FbDeferredIoState {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct FbOps {
    pub owner: *mut c_void,
    pub fb_open: Option<unsafe extern "C" fn(*mut FbInfo, c_int) -> c_int>,
    pub fb_release: Option<unsafe extern "C" fn(*mut FbInfo, c_int) -> c_int>,
    pub fb_read:
        Option<unsafe extern "C" fn(*mut FbInfo, *mut c_char, usize, *mut c_long) -> isize>,
    pub fb_write:
        Option<unsafe extern "C" fn(*mut FbInfo, *const c_char, usize, *mut c_long) -> isize>,
    pub fb_check_var: Option<unsafe extern "C" fn(*mut FbVarScreenInfo, *mut FbInfo) -> c_int>,
    pub fb_set_par: Option<unsafe extern "C" fn(*mut FbInfo) -> c_int>,
    pub fb_setcolreg: Option<unsafe extern "C" fn(u32, u32, u32, u32, u32, *mut FbInfo) -> c_int>,
    pub fb_setcmap: Option<unsafe extern "C" fn(*mut FbCmap, *mut FbInfo) -> c_int>,
    pub fb_blank: Option<unsafe extern "C" fn(c_int, *mut FbInfo) -> c_int>,
    pub fb_pan_display: Option<unsafe extern "C" fn(*mut FbVarScreenInfo, *mut FbInfo) -> c_int>,
    pub fb_fillrect: Option<unsafe extern "C" fn(*mut FbInfo, *const FbFillRect)>,
    pub fb_copyarea: Option<unsafe extern "C" fn(*mut FbInfo, *const FbCopyArea)>,
    pub fb_imageblit: Option<unsafe extern "C" fn(*mut FbInfo, *const FbImage)>,
    pub fb_cursor: Option<unsafe extern "C" fn(*mut FbInfo, *mut FbCursor) -> c_int>,
    pub fb_sync: Option<unsafe extern "C" fn(*mut FbInfo) -> c_int>,
    pub fb_ioctl: Option<unsafe extern "C" fn(*mut FbInfo, u32, c_ulong) -> c_int>,
    pub fb_compat_ioctl: Option<unsafe extern "C" fn(*mut FbInfo, u32, c_ulong) -> c_int>,
    pub fb_mmap: Option<unsafe extern "C" fn(*mut FbInfo, *mut VmAreaStruct) -> c_int>,
    pub fb_get_caps:
        Option<unsafe extern "C" fn(*mut FbInfo, *mut FbBlitCaps, *mut FbVarScreenInfo)>,
    pub fb_destroy: Option<unsafe extern "C" fn(*mut FbInfo)>,
}

// These callback records are pointers only in `fb_ops`; fbmem does not embed
// them, and the actual records are owned by fbcon / the relevant driver.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbFillRect {
    pub dx: u32,
    pub dy: u32,
    pub width: u32,
    pub height: u32,
    pub color: u32,
    pub rop: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbCopyArea {
    pub sx: u32,
    pub sy: u32,
    pub dx: u32,
    pub dy: u32,
    pub width: u32,
    pub height: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbImage {
    pub dx: u32,
    pub dy: u32,
    pub width: u32,
    pub height: u32,
    pub fg_color: u32,
    pub bg_color: u32,
    pub depth: u8,
    pub data: *const c_char,
    pub cmap: FbCmap,
}
#[repr(C)]
pub struct FbCursor {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct FbBlitCaps {
    _opaque: [u8; 0],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbVideoMode {
    pub name: *const c_char,
    pub refresh: u32,
    pub xres: u32,
    pub yres: u32,
    pub pixclock: u32,
    pub left_margin: u32,
    pub right_margin: u32,
    pub upper_margin: u32,
    pub lower_margin: u32,
    pub hsync_len: u32,
    pub vsync_len: u32,
    pub sync: u32,
    pub vmode: u32,
    pub flag: u32,
}

#[repr(C)]
pub struct FbModelist {
    pub list: ListHead,
    pub mode: FbVideoMode,
}

/// `struct fb_info` for the selected Linux 7.2.3 x86_64 configuration.
#[repr(C)]
pub struct FbInfo {
    pub count: RefcountT,
    pub node: c_int,
    pub flags: c_int,
    pub fbcon_rotate_hint: c_int,
    pub lock: Mutex,
    pub mm_lock: Mutex,
    pub var: FbVarScreenInfo,
    pub fix: FbFixScreenInfo,
    pub monspecs: FbMonSpecs,
    pub pixmap: FbPixmap,
    pub sprite: FbPixmap,
    pub cmap: FbCmap,
    pub modelist: ListHead,
    pub mode: *mut FbVideoMode,
    pub blank: c_int,
    pub deferred_work: DelayedWork,
    pub fbdefio: *mut FbDeferredIo,
    pub fbdefio_state: *mut FbDeferredIoState,
    pub fbops: *const FbOps,
    pub device: *mut c_void,
    pub dev: *mut c_void,
    /// Linux exposes this pointer and `screen_buffer` as an anonymous union.
    /// A single pointer has the same C layout while retaining direct access.
    pub screen_base: *mut c_char,
    pub screen_size: c_ulong,
    pub pseudo_palette: *mut c_void,
    pub state: u32,
    pub fbcon_par: *mut c_void,
    /// Driver-private tail (separately allocated after `fb_info`).
    pub par: *mut c_void,
    pub skip_vt_switch: bool,
    pub skip_panic: bool,
}

pub const FB_MAX: usize = 32;
pub const FBINFO_STATE_RUNNING: u32 = 0;
pub const FBINFO_STATE_SUSPENDED: u32 = 1;
pub const FB_PIXMAP_DEFAULT: u32 = 1;
pub const FBINFO_VIRTFB: u32 = 0x0004;
pub const FBINFO_READS_FAST: u32 = 0x0080;
pub const FBINFO_HIDE_SMEM_START: u32 = 0x0020_0000;
pub const FB_BLANK_POWERDOWN: c_int = 4;

// Linux x86_64 ABI anchors from the configured fbdev headers.
const _: () = assert!(size_of::<FbVarScreenInfo>() == 160);
const _: () = assert!(size_of::<FbFixScreenInfo>() == 80);
const _: () = assert!(size_of::<FbCmap>() == 40);
const _: () = assert!(size_of::<FbDeferredIo>() == 32);
const _: () = assert!(size_of::<FbOps>() == 168);
const _: () = assert!(size_of::<FbFillRect>() == 24);
const _: () = assert!(size_of::<FbCopyArea>() == 24);
const _: () = assert!(size_of::<FbImage>() == 80);
const _: () = assert!(size_of::<FbMonSpecs>() == 144);
const _: () = assert!(size_of::<FbPixmap>() == 72);
const _: () = assert!(size_of::<FbVideoMode>() == 64);
const _: () = assert!(size_of::<FbModelist>() == 80);
const _: () = assert!(align_of::<FbInfo>() == 8);
const _: () = assert!(size_of::<FbInfo>() == 848);
const _: () = assert!(offset_of!(FbInfo, var) == 64);
const _: () = assert!(offset_of!(FbInfo, fix) == 224);
const _: () = assert!(offset_of!(FbInfo, monspecs) == 304);
const _: () = assert!(offset_of!(FbInfo, pixmap) == 448);
const _: () = assert!(offset_of!(FbInfo, sprite) == 520);
const _: () = assert!(offset_of!(FbInfo, cmap) == 592);
const _: () = assert!(offset_of!(FbInfo, modelist) == 632);
const _: () = assert!(offset_of!(FbInfo, mode) == 648);
const _: () = assert!(offset_of!(FbInfo, blank) == 656);
const _: () = assert!(offset_of!(FbInfo, deferred_work) == 664);
const _: () = assert!(offset_of!(FbInfo, fbdefio) == 752);
const _: () = assert!(offset_of!(FbInfo, fbdefio_state) == 760);
const _: () = assert!(offset_of!(FbInfo, fbops) == 768);
const _: () = assert!(offset_of!(FbInfo, device) == 776);
const _: () = assert!(offset_of!(FbInfo, dev) == 784);
const _: () = assert!(offset_of!(FbInfo, screen_base) == 792);
const _: () = assert!(offset_of!(FbInfo, screen_size) == 800);
const _: () = assert!(offset_of!(FbInfo, pseudo_palette) == 808);
const _: () = assert!(offset_of!(FbInfo, state) == 816);
const _: () = assert!(offset_of!(FbInfo, fbcon_par) == 824);
const _: () = assert!(offset_of!(FbInfo, par) > offset_of!(FbInfo, fbcon_par));
const _: () = assert!(offset_of!(FbInfo, skip_vt_switch) == 840);
const _: () = assert!(offset_of!(FbInfo, skip_panic) == 841);
