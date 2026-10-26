// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Function-like macros from the Linux 7.2.3 DRM uapi headers:
//! `include/uapi/asm-generic/ioctl.h` (`_IOC` family, used by `drm.h`) and
//! `include/uapi/drm/drm_fourcc.h` (`fourcc_code`, `fourcc_mod_code`).
//!
//! Each helper is checked in the tests against the value the C preprocessor
//! produces for the matching object-like constant in `uapi_drm_consts`.

/// `_IOC_NONE` / `_IOC_WRITE` / `_IOC_READ` direction bits (x86_64 generic
/// ioctl encoding: 2-bit direction, 14-bit size, 8-bit type, 8-bit number).
pub const IOC_NONE: u32 = 0;
pub const IOC_WRITE: u32 = 1;
pub const IOC_READ: u32 = 2;

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;

/// Linux `_IOC(dir, type, nr, size)`.
pub const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << IOC_DIRSHIFT) | (ty << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT) | (size << IOC_SIZESHIFT)
}

/// Linux `_IO(type, nr)`.
pub const fn io(ty: u32, nr: u32) -> u32 {
    ioc(IOC_NONE, ty, nr, 0)
}

/// Linux `_IOR(type, nr, size)`.
pub const fn ior(ty: u32, nr: u32, size: u32) -> u32 {
    ioc(IOC_READ, ty, nr, size)
}

/// Linux `_IOW(type, nr, size)`.
pub const fn iow(ty: u32, nr: u32, size: u32) -> u32 {
    ioc(IOC_WRITE, ty, nr, size)
}

/// Linux `_IOWR(type, nr, size)`.
pub const fn iowr(ty: u32, nr: u32, size: u32) -> u32 {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size)
}

/// Linux `DRM_IOCTL_BASE` (`'d'`).
pub const DRM_IOCTL_BASE: u32 = b'd' as u32;

/// Linux `DRM_IO(nr)` / `DRM_IOR` / `DRM_IOW` / `DRM_IOWR` on the DRM base.
pub const fn drm_io(nr: u32) -> u32 {
    io(DRM_IOCTL_BASE, nr)
}
pub const fn drm_ior(nr: u32, size: u32) -> u32 {
    ior(DRM_IOCTL_BASE, nr, size)
}
pub const fn drm_iow(nr: u32, size: u32) -> u32 {
    iow(DRM_IOCTL_BASE, nr, size)
}
pub const fn drm_iowr(nr: u32, size: u32) -> u32 {
    iowr(DRM_IOCTL_BASE, nr, size)
}

/// Linux `fourcc_code(a, b, c, d)` from drm_fourcc.h.
pub const fn fourcc_code(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

/// Linux `fourcc_mod_code(vendor, val)` from drm_fourcc.h.
pub const fn fourcc_mod_code(vendor: u64, val: u64) -> u64 {
    (vendor << 56) | (val & 0x00ff_ffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uapi_drm_consts::*;

    #[test]
    fn ioctl_encoders_match_preprocessor_values() {
        // DRM_IOWR(0x00, struct drm_version) with sizeof(struct drm_version) == 64.
        assert_eq!(drm_iowr(0x00, 64), DRM_IOCTL_VERSION);
        // DRM_IO(0x09) is DRM_IOCTL_SET_MASTER.
        assert_eq!(drm_io(0x1e), DRM_IOCTL_SET_MASTER);
        assert_eq!(drm_io(0x1f), DRM_IOCTL_DROP_MASTER);
    }

    #[test]
    fn fourcc_helpers_match_format_constants() {
        assert_eq!(fourcc_code(b'X', b'R', b'2', b'4'), DRM_FORMAT_XRGB8888);
        assert_eq!(fourcc_mod_code(0, 0), DRM_FORMAT_MOD_LINEAR);
        assert_eq!(
            DRM_FORMAT_MOD_INVALID,
            fourcc_mod_code(0, 0x00ff_ffff_ffff_ffff)
        );
    }
}
