// SPDX-License-Identifier: GPL-2.0-or-later WITH Linux-syscall-note
//! PCM ioctl numbers using the asm-generic encoding used by RISC-V64.
//!
//! A constant's presence is not an implementation capability advertisement.

use core::mem::size_of;

use crate::{HwParams, Info, Status, SwParams, SyncPtr, XferI};

pub(crate) const fn command<T>(direction: u32, group: u8, number: u8) -> u32 {
    (direction << 30) | ((size_of::<T>() as u32) << 16) | ((group as u32) << 8) | number as u32
}

pub const PVERSION: u32 = command::<i32>(2, b'A', 0x00);
pub const INFO: u32 = command::<Info>(2, b'A', 0x01);
pub const TSTAMP: u32 = command::<i32>(1, b'A', 0x02);
pub const TTSTAMP: u32 = command::<i32>(1, b'A', 0x03);
pub const USER_PVERSION: u32 = command::<u32>(1, b'A', 0x04);
pub const HW_REFINE: u32 = command::<HwParams>(3, b'A', 0x10);
pub const HW_PARAMS: u32 = command::<HwParams>(3, b'A', 0x11);
pub const HW_FREE: u32 = command::<()>(0, b'A', 0x12);
pub const SW_PARAMS: u32 = command::<SwParams>(3, b'A', 0x13);
pub const STATUS: u32 = command::<Status>(2, b'A', 0x20);
pub const DELAY: u32 = command::<i64>(2, b'A', 0x21);
pub const HWSYNC: u32 = command::<()>(0, b'A', 0x22);
pub const SYNC_PTR: u32 = command::<SyncPtr>(3, b'A', 0x23);
pub const STATUS_EXT: u32 = command::<Status>(3, b'A', 0x24);
pub const PREPARE: u32 = command::<()>(0, b'A', 0x40);
pub const RESET: u32 = command::<()>(0, b'A', 0x41);
pub const START: u32 = command::<()>(0, b'A', 0x42);
pub const DROP: u32 = command::<()>(0, b'A', 0x43);
pub const DRAIN: u32 = command::<()>(0, b'A', 0x44);
pub const READI_FRAMES: u32 = command::<XferI>(2, b'A', 0x51);
