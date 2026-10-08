// SPDX-License-Identifier: GPL-2.0-or-later WITH Linux-syscall-note
// ABI source: Linux v6.6 include/uapi/sound/asound.h.
// ALSA UAPI copyright (c) 1994-2003 Jaroslav Kysela and Abramo Bagnara.

//! Wire structures for the Linux 64-bit little-endian ALSA PCM interface.
//!
//! These are unvalidated request/response representations, not driver state.
//! User addresses remain integers; decoding a structure grants no permission
//! to dereference them. All padding is explicit so a zeroed response cannot
//! expose uninitialized Rust padding. This crate does not implement ALSA.

#![no_std]

#[cfg(not(all(target_pointer_width = "64", target_endian = "little")))]
compile_error!("alsa-pcm-uapi currently supports only 64-bit little-endian layouts");

use bytemuck::{Pod, Zeroable};

pub mod control;
pub mod ioctl;

/// Linux `snd_pcm_info`, also used by the control device's PCM_INFO ioctl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Info {
    pub device: u32,
    pub subdevice: u32,
    pub stream: i32,
    pub card: i32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub subname: [u8; 32],
    pub dev_class: i32,
    pub dev_subclass: i32,
    pub subdevices_count: u32,
    pub subdevices_avail: u32,
    pub sync: [u8; 16],
    pub reserved: [u8; 64],
}

/// Linux `snd_mask`: bit positions identify protocol values, not Rust enums.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Mask {
    pub bits: [u32; 8],
}

/// Linux `snd_interval`, with the C bitfield represented as raw wire bits.
///
/// Bits 0..=3 are openmin, openmax, integer, and empty respectively.
/// Consumers must validate flags and bounds before using this interval.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Interval {
    pub min: u32,
    pub max: u32,
    pub flags: u32,
}

/// Linux `snd_pcm_hw_params` for parameter refinement and commitment.
///
/// Masks correspond to parameter IDs 0..=2; intervals to IDs 8..=19.
/// In particular, rates are constraints, not a single requested integer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct HwParams {
    pub flags: u32,
    pub masks: [Mask; 3],
    pub reserved_masks: [Mask; 5],
    pub intervals: [Interval; 12],
    pub reserved_intervals: [Interval; 9],
    pub rmask: u32,
    pub cmask: u32,
    pub info: u32,
    pub msbits: u32,
    pub rate_num: u32,
    pub rate_den: u32,
    pub fifo_size: u64,
    pub reserved: [u8; 64],
}

/// Linux `snd_pcm_sw_params`; frame-valued fields are not byte counts.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SwParams {
    pub tstamp_mode: i32,
    pub period_step: u32,
    pub sleep_min: u32,
    pub padding: u32,
    pub avail_min: u64,
    pub xfer_align: u64,
    pub start_threshold: u64,
    pub stop_threshold: u64,
    pub silence_threshold: u64,
    pub silence_size: u64,
    pub boundary: u64,
    pub proto: u32,
    pub tstamp_type: u32,
    pub reserved: [u8; 56],
}

/// Linux `snd_xferi`, shared by interleaved capture and playback commands.
///
/// `result` is a signed frame count/error output. The ioctl return value is
/// separate. `buffer` is a user virtual address, never a DMA address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct XferI {
    pub result: i64,
    pub buffer: u64,
    pub frames: u64,
}

/// Native 64-bit Linux time representation, independent of the host libc.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Timespec {
    pub seconds: i64,
    pub nanoseconds: i64,
}

/// Linux `snd_pcm_status` on LP64, including the extended timestamp fields.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Status {
    pub state: i32,
    pub padding: u32,
    pub trigger_tstamp: Timespec,
    pub tstamp: Timespec,
    pub appl_ptr: u64,
    pub hw_ptr: u64,
    pub delay: i64,
    pub avail: u64,
    pub avail_max: u64,
    pub overrange: u64,
    pub suspended_state: i32,
    pub audio_tstamp_data: u32,
    pub audio_tstamp: Timespec,
    pub driver_tstamp: Timespec,
    pub audio_tstamp_accuracy: u32,
    pub reserved: [u8; 20],
}

/// Status member of Linux `snd_pcm_sync_ptr`'s 64-byte status union.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SyncStatus {
    pub state: i32,
    pub padding: u32,
    pub hw_ptr: u64,
    pub tstamp: Timespec,
    pub suspended_state: i32,
    pub padding_after_state: u32,
    pub audio_tstamp: Timespec,
    pub reserved: [u8; 8],
}

/// Control member of Linux `snd_pcm_sync_ptr`'s 64-byte control union.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SyncControl {
    pub appl_ptr: u64,
    pub avail_min: u64,
    pub reserved: [u8; 48],
}

/// Linux `snd_pcm_sync_ptr`, including explicit union tail storage.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SyncPtr {
    pub flags: u32,
    pub padding: u32,
    pub status: SyncStatus,
    pub control: SyncControl,
}

/// Request hardware pointer synchronization before returning status.
pub const SYNC_HWSYNC: u32 = 1;
/// When set, return the application pointer instead of applying the input.
pub const SYNC_APPL: u32 = 1 << 1;
/// When set, return avail_min instead of applying the input.
pub const SYNC_AVAIL_MIN: u32 = 1 << 2;

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn parameter_wire_layout_matches_linux_lp64() {
        assert_eq!(size_of::<Info>(), 288);
        assert_eq!(offset_of!(Info, sync), 208);
        assert_eq!(size_of::<Status>(), 152);
        assert_eq!(offset_of!(Status, appl_ptr), 40);
        assert_eq!(offset_of!(Status, audio_tstamp), 96);
        assert_eq!(ioctl::INFO, 0x8120_4101);
        assert_eq!(ioctl::STATUS, 0x8098_4120);
        assert_eq!(ioctl::STATUS_EXT, 0xc098_4124);
        assert_eq!(size_of::<HwParams>(), 608);
        assert_eq!(align_of::<HwParams>(), 8);
        assert_eq!(offset_of!(HwParams, intervals), 260);
        assert_eq!(offset_of!(HwParams, rmask), 512);
        assert_eq!(offset_of!(HwParams, fifo_size), 536);
        assert_eq!(size_of::<SwParams>(), 136);
        assert_eq!(offset_of!(SwParams, avail_min), 16);
        assert_eq!(offset_of!(SwParams, boundary), 64);
        assert_eq!(offset_of!(SwParams, reserved), 80);
        assert_eq!(ioctl::HW_REFINE, 0xc260_4110);
        assert_eq!(ioctl::HW_PARAMS, 0xc260_4111);
        assert_eq!(ioctl::SW_PARAMS, 0xc088_4113);
    }

    #[test]
    fn frame_and_sync_messages_decode_at_linux_offsets() {
        let mut transfer = [0_u8; 24];
        transfer[..8].copy_from_slice(&(-32_i64).to_le_bytes());
        transfer[8..16].copy_from_slice(&0x1234_5678_9000_u64.to_le_bytes());
        transfer[16..].copy_from_slice(&480_u64.to_le_bytes());
        let decoded = bytemuck::pod_read_unaligned::<XferI>(&transfer);
        assert_eq!(decoded.result, -32);
        assert_eq!(decoded.buffer, 0x1234_5678_9000);
        assert_eq!(decoded.frames, 480);
        assert_eq!(ioctl::READI_FRAMES, 0x8018_4151);

        let mut sync = [0_u8; 136];
        sync[..4].copy_from_slice(&(SYNC_APPL | SYNC_AVAIL_MIN).to_le_bytes());
        sync[8..12].copy_from_slice(&3_i32.to_le_bytes());
        sync[16..24].copy_from_slice(&960_u64.to_le_bytes());
        sync[72..80].copy_from_slice(&480_u64.to_le_bytes());
        sync[80..88].copy_from_slice(&160_u64.to_le_bytes());
        let decoded = bytemuck::pod_read_unaligned::<SyncPtr>(&sync);
        assert_eq!(decoded.status.state, 3);
        assert_eq!(decoded.status.hw_ptr, 960);
        assert_eq!(decoded.control.appl_ptr, 480);
        assert_eq!(decoded.control.avail_min, 160);
        assert_eq!(bytemuck::bytes_of(&decoded), &sync);
        assert_eq!(ioctl::SYNC_PTR, 0xc088_4123);
    }
}
