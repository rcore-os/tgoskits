//! Linux v7.1 `perf_event_open(2)` attribute and flag ABI.

use alloc::vec;
use core::{
    mem::{MaybeUninit, size_of},
    slice,
};

use ax_io::{Read, Write};
use kbpf_basic::linux_bpf::perf_event_attr;

use crate::{
    StarryError, StarryResult,
    mm::{VmBytes, VmBytesMut},
};

/// First published `perf_event_attr` size.
pub(crate) const PERF_ATTR_SIZE_VER0: usize = 64;
/// Linux v7.1 `perf_event_attr` size, including `config4`.
pub(crate) const PERF_ATTR_SIZE_VER9: usize = 144;
const PERF_ATTR_MAX_SIZE: usize = 4096;

const PERF_SAMPLE_BRANCH_STACK: u64 = 1 << 11;
const PERF_SAMPLE_REGS_USER: u64 = 1 << 12;
/// Linux AArch64 user-register mask requested by the perf FP unwinder.
pub(crate) const PERF_REG_ARM64_LR_MASK: u64 = 1 << 30;
const PERF_SAMPLE_STACK_USER: u64 = 1 << 13;
const PERF_SAMPLE_WEIGHT: u64 = 1 << 14;
const PERF_SAMPLE_WEIGHT_STRUCT: u64 = 1 << 24;
const PERF_SAMPLE_MAX: u64 = 1 << 25;
const PERF_FORMAT_MAX: u64 = 1 << 5;
const PERF_SAMPLE_BRANCH_PLM_ALL: u64 = 0b111;
const PERF_SAMPLE_BRANCH_MAX: u64 = 1 << 20;
const PERF_MAX_STACK_DEPTH: u16 = 127;

/// Validated `perf_event_open(2)` flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfOpenFlags(u32);

impl PerfOpenFlags {
    /// `PERF_FLAG_FD_NO_GROUP`.
    pub(crate) const FD_NO_GROUP: u32 = 1 << 0;
    /// `PERF_FLAG_FD_OUTPUT`.
    pub(crate) const FD_OUTPUT: u32 = 1 << 1;
    /// `PERF_FLAG_PID_CGROUP`.
    pub(crate) const PID_CGROUP: u32 = 1 << 2;
    /// `PERF_FLAG_FD_CLOEXEC`.
    pub(crate) const FD_CLOEXEC: u32 = 1 << 3;
    const ALL: u64 =
        (Self::FD_NO_GROUP | Self::FD_OUTPUT | Self::PID_CGROUP | Self::FD_CLOEXEC) as u64;

    /// Rejects unknown bits before any user pointer access, as Linux does.
    pub(crate) const fn parse(raw: u64) -> StarryResult<Self> {
        if raw & !Self::ALL != 0 {
            return Err(StarryError::InvalidInput);
        }
        Ok(Self(raw as u32))
    }

    /// Reports whether one flag is set.
    pub(crate) const fn contains(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    /// Returns the Linux flag word for the external probe adapter.
    pub(crate) const fn bits(self) -> u32 {
        self.0
    }
}

/// Copies and validates one versioned `perf_event_attr` from userspace.
pub(crate) fn copy_perf_event_attr(
    current: &crate::task::UserTaskRef,
    user_addr: usize,
) -> StarryResult<perf_event_attr> {
    let mut size_bytes = [0u8; size_of::<u32>()];
    let size_addr = user_addr
        .checked_add(size_of::<u32>())
        .ok_or(StarryError::BadAddress)?;
    VmBytes::new(current, size_addr as *mut u8, size_bytes.len()).read(&mut size_bytes)?;
    let requested_size = u32::from_ne_bytes(size_bytes) as usize;
    let size = match normalized_attr_size(requested_size) {
        Ok(size) => size,
        Err(error) => {
            write_kernel_attr_size(current, user_addr);
            return Err(error);
        }
    };

    let mut bytes = vec![0u8; size];
    VmBytes::new(current, user_addr as *mut u8, size).read(&mut bytes)?;
    match decode_perf_event_attr(&bytes, size) {
        Ok(attr) => Ok(attr),
        Err(StarryError::ArgumentListTooLong) => {
            write_kernel_attr_size(current, user_addr);
            Err(StarryError::ArgumentListTooLong)
        }
        Err(error) => Err(error),
    }
}

fn normalized_attr_size(requested_size: usize) -> StarryResult<usize> {
    let size = if requested_size == 0 {
        PERF_ATTR_SIZE_VER0
    } else {
        requested_size
    };
    if !(PERF_ATTR_SIZE_VER0..=PERF_ATTR_MAX_SIZE).contains(&size) {
        return Err(StarryError::ArgumentListTooLong);
    }
    Ok(size)
}

fn write_kernel_attr_size(current: &crate::task::UserTaskRef, user_addr: usize) {
    let Some(size_addr) = user_addr.checked_add(size_of::<u32>()) else {
        return;
    };
    let bytes = (PERF_ATTR_SIZE_VER9 as u32).to_ne_bytes();
    let _ = VmBytesMut::new(current, size_addr as *mut u8, bytes.len()).write(&bytes);
}

fn decode_perf_event_attr(bytes: &[u8], size: usize) -> StarryResult<perf_event_attr> {
    if size > PERF_ATTR_SIZE_VER9 && bytes[PERF_ATTR_SIZE_VER9..].iter().any(|byte| *byte != 0) {
        return Err(StarryError::ArgumentListTooLong);
    }

    // SAFETY: bindgen's `perf_event_attr` consists only of integer fields,
    // integer unions, and a byte-backed bitfield. All-zero is a valid value.
    let mut attr = unsafe { MaybeUninit::<perf_event_attr>::zeroed().assume_init() };
    let copy_len = bytes.len().min(size_of::<perf_event_attr>());
    // SAFETY: `attr` is exclusively borrowed, and `copy_len` is bounded by its
    // exact object size. Copying bytes cannot create an invalid integer value.
    let attr_bytes = unsafe {
        slice::from_raw_parts_mut(
            (&mut attr as *mut perf_event_attr).cast::<u8>(),
            size_of::<perf_event_attr>(),
        )
    };
    attr_bytes[..copy_len].copy_from_slice(&bytes[..copy_len]);
    attr.size = size as u32;
    validate_perf_event_attr(&mut attr, bytes)?;
    Ok(attr)
}

fn validate_perf_event_attr(attr: &mut perf_event_attr, bytes: &[u8]) -> StarryResult<()> {
    let attr_flags = u64::from_ne_bytes(read_zero_extended(bytes, 40));
    if attr_flags >> 40 != 0 {
        return Err(StarryError::InvalidInput);
    }

    let reserved_2 = u16::from_ne_bytes(read_zero_extended(bytes, 110));
    let aux_action = u32::from_ne_bytes(read_zero_extended(bytes, 116));
    if reserved_2 != 0 || aux_action & !0b111 != 0 {
        return Err(StarryError::InvalidInput);
    }
    if attr.sample_type & !(PERF_SAMPLE_MAX - 1) != 0
        || attr.read_format & !(PERF_FORMAT_MAX - 1) != 0
    {
        return Err(StarryError::InvalidInput);
    }

    if attr.sample_type & PERF_SAMPLE_BRANCH_STACK != 0 {
        let branch_type = attr.branch_sample_type;
        if branch_type & !(PERF_SAMPLE_BRANCH_MAX - 1) != 0
            || branch_type & !PERF_SAMPLE_BRANCH_PLM_ALL == 0
        {
            return Err(StarryError::InvalidInput);
        }
    }
    if attr.sample_type & PERF_SAMPLE_STACK_USER != 0
        && (attr.sample_stack_user >= u16::MAX as u32
            || !attr
                .sample_stack_user
                .is_multiple_of(size_of::<u64>() as u32))
    {
        return Err(StarryError::InvalidInput);
    }
    if attr.sample_type & PERF_SAMPLE_REGS_USER != 0
        && attr.sample_regs_user != 0
        && !(cfg!(target_arch = "aarch64") && attr.sample_regs_user == PERF_REG_ARM64_LR_MASK)
    {
        // Only LR is captured for the AArch64 FP unwinder. Never accept a
        // register selection whose values cannot be serialized from the IRQ
        // snapshot; mask zero still produces PERF_SAMPLE_REGS_ABI_NONE.
        return Err(StarryError::InvalidInput);
    }
    if attr.sample_type & PERF_SAMPLE_WEIGHT != 0
        && attr.sample_type & PERF_SAMPLE_WEIGHT_STRUCT != 0
    {
        return Err(StarryError::InvalidInput);
    }
    if attr.inherit() == 0 && attr.inherit_thread() != 0
        || attr.remove_on_exec() != 0 && attr.enable_on_exec() != 0
        || attr.sigtrap() != 0 && attr.remove_on_exec() == 0
    {
        return Err(StarryError::InvalidInput);
    }
    if attr.sample_max_stack == 0 {
        attr.sample_max_stack = PERF_MAX_STACK_DEPTH;
    }
    Ok(())
}

/// Mirrors copy_struct_from_user even when attr.size cuts through a field.
fn read_zero_extended<const N: usize>(bytes: &[u8], offset: usize) -> [u8; N] {
    let mut field = [0; N];
    if let Some(tail) = bytes.get(offset..) {
        let length = tail.len().min(N);
        field[..length].copy_from_slice(&tail[..length]);
    }
    field
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    fn attr_bytes(size: usize) -> alloc::vec::Vec<u8> {
        let mut bytes = vec![0u8; size];
        bytes[4..8].copy_from_slice(&(size as u32).to_ne_bytes());
        bytes
    }

    #[test]
    fn short_attributes_are_zero_filled() {
        let bytes = attr_bytes(PERF_ATTR_SIZE_VER0);
        let attr = decode_perf_event_attr(&bytes, bytes.len()).unwrap();
        assert_eq!(attr.size, PERF_ATTR_SIZE_VER0 as u32);
        assert_eq!(attr.config3, 0);
        assert_eq!(attr.sample_max_stack, PERF_MAX_STACK_DEPTH);
    }

    #[test]
    fn known_config4_is_not_an_unknown_tail() {
        let mut bytes = attr_bytes(PERF_ATTR_SIZE_VER9);
        bytes[136..144].copy_from_slice(&17u64.to_ne_bytes());
        assert!(decode_perf_event_attr(&bytes, bytes.len()).is_ok());
    }

    #[test]
    fn unknown_nonzero_tail_is_e2big() {
        let mut bytes = attr_bytes(PERF_ATTR_SIZE_VER9 + 8);
        bytes[PERF_ATTR_SIZE_VER9] = 1;
        assert!(matches!(
            decode_perf_event_attr(&bytes, bytes.len()),
            Err(StarryError::ArgumentListTooLong)
        ));
    }

    #[test]
    fn malformed_reserved_and_format_bits_are_einval() {
        let mut bytes = attr_bytes(PERF_ATTR_SIZE_VER9);
        bytes[45] = 1;
        assert!(matches!(
            decode_perf_event_attr(&bytes, bytes.len()),
            Err(StarryError::InvalidInput)
        ));

        let mut bytes = attr_bytes(PERF_ATTR_SIZE_VER9);
        bytes[32..40].copy_from_slice(&PERF_FORMAT_MAX.to_ne_bytes());
        assert!(matches!(
            decode_perf_event_attr(&bytes, bytes.len()),
            Err(StarryError::InvalidInput)
        ));
    }

    #[test]
    fn unknown_open_flags_are_rejected_without_truncation() {
        let supported = (PerfOpenFlags::FD_NO_GROUP
            | PerfOpenFlags::FD_OUTPUT
            | PerfOpenFlags::PID_CGROUP
            | PerfOpenFlags::FD_CLOEXEC) as u64;
        let flags = PerfOpenFlags::parse(supported).unwrap();
        assert_eq!(flags.bits(), 0xf);
        assert!(flags.contains(PerfOpenFlags::PID_CGROUP));
        assert!(PerfOpenFlags::parse(1 << 4).is_err());
        assert!(PerfOpenFlags::parse(1 << 32).is_err());
        assert!(PerfOpenFlags::parse(1u64 << 48).is_err());
        assert!(PerfOpenFlags::parse(PerfOpenFlags::FD_CLOEXEC as u64).is_ok());
    }
}
