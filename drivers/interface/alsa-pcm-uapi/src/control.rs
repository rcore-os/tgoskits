// SPDX-License-Identifier: GPL-2.0-or-later WITH Linux-syscall-note
// ABI source: Linux v6.6 include/uapi/sound/asound.h.
//! LP64 control-device wire layouts. Union payloads remain unvalidated bytes.

use bytemuck::{Pod, Zeroable};

use crate::ioctl::command;

/// Linux `snd_ctl_card_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CardInfo {
    pub card: i32,
    pub padding: u32,
    pub id: [u8; 16],
    pub driver: [u8; 16],
    pub name: [u8; 32],
    pub longname: [u8; 80],
    pub reserved: [u8; 16],
    pub mixername: [u8; 80],
    pub components: [u8; 128],
}

/// Linux `snd_ctl_elem_id`; a nonzero numid takes precedence over the name.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ElemId {
    pub numid: u32,
    pub iface: i32,
    pub device: u32,
    pub subdevice: u32,
    pub name: [u8; 44],
    pub index: u32,
}

/// Linux `snd_ctl_elem_list`. `ids` is a user virtual address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ElemList {
    pub offset: u32,
    pub space: u32,
    pub used: u32,
    pub count: u32,
    pub ids: u64,
    pub reserved: [u8; 50],
    pub padding: [u8; 6],
}

/// Linux `snd_ctl_elem_info`. The payload's interpretation depends on kind.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ElemInfo {
    pub id: ElemId,
    pub kind: i32,
    pub access: u32,
    pub count: u32,
    pub owner: i32,
    pub value: [u64; 16],
    pub reserved: [u8; 64],
}

/// Linux `snd_ctl_elem_value`, retaining storage for the largest union member.
///
/// INTEGER values are signed LP64 longs in `value`. `indirect` is the obsolete
/// C bitfield, not a Rust bool; callers must reject unsupported access modes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ElemValue {
    pub id: ElemId,
    pub indirect: u32,
    pub padding: u32,
    pub value: [i64; 128],
    pub reserved: [u8; 128],
}

pub const PVERSION: u32 = command::<i32>(2, b'U', 0x00);
pub const CARD_INFO: u32 = command::<CardInfo>(2, b'U', 0x01);
pub const ELEM_LIST: u32 = command::<ElemList>(3, b'U', 0x10);
pub const ELEM_INFO: u32 = command::<ElemInfo>(3, b'U', 0x11);
pub const ELEM_READ: u32 = command::<ElemValue>(3, b'U', 0x12);
pub const ELEM_WRITE: u32 = command::<ElemValue>(3, b'U', 0x13);
pub const SUBSCRIBE_EVENTS: u32 = command::<i32>(3, b'U', 0x16);
pub const PCM_NEXT_DEVICE: u32 = command::<i32>(2, b'U', 0x30);
pub const PCM_INFO: u32 = command::<crate::Info>(3, b'U', 0x31);
pub const PCM_PREFER_SUBDEVICE: u32 = command::<i32>(1, b'U', 0x32);

#[cfg(test)]
mod tests {
    use core::mem::offset_of;

    use super::*;

    #[test]
    fn control_wire_layout_matches_linux_lp64() {
        assert_eq!(size_of::<CardInfo>(), 376);
        assert_eq!(size_of::<ElemId>(), 64);
        assert_eq!(size_of::<ElemList>(), 80);
        assert_eq!(offset_of!(ElemList, ids), 16);
        assert_eq!(size_of::<ElemInfo>(), 272);
        assert_eq!(offset_of!(ElemInfo, value), 80);
        assert_eq!(size_of::<ElemValue>(), 1224);
        assert_eq!(offset_of!(ElemValue, value), 72);
        assert_eq!(CARD_INFO, 0x8178_5501);
        assert_eq!(ELEM_LIST, 0xc050_5510);
        assert_eq!(ELEM_INFO, 0xc110_5511);
        assert_eq!(ELEM_READ, 0xc4c8_5512);

        let mut bytes = [0_u8; 1224];
        bytes[..4].copy_from_slice(&7_u32.to_le_bytes());
        bytes[72..80].copy_from_slice(&(-12_i64).to_le_bytes());
        let decoded = bytemuck::pod_read_unaligned::<ElemValue>(&bytes);
        assert_eq!(decoded.id.numid, 7);
        assert_eq!(decoded.value[0], -12);
        assert_eq!(bytemuck::bytes_of(&decoded), &bytes);
    }
}
