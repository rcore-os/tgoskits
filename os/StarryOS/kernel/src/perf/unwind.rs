//! Frame-pointer callchain integration for PMU hard-IRQ samples.

use core::ops::Range;

const USER_VA_END: usize = 1 << 48;
const USER_STACK_WINDOW: usize = 8 * 1024 * 1024;

fn kernel_ranges() -> Option<(Range<usize>, Range<usize>)> {
    Some((axbacktrace::ip_range()?, axbacktrace::fp_range()?))
}

/// Walks an interrupted kernel frame chain into caller-owned fixed storage.
pub fn kernel_callchain(pc: usize, fp: usize, out: &mut [u64]) -> usize {
    let Some((ip_range, fp_range)) = kernel_ranges() else {
        return write_leaf(pc, out);
    };
    axbacktrace::walk_fp(
        pc,
        fp,
        &ip_range,
        &fp_range,
        |va| super::nofault::read_kernel_word(va).map(|word| word as usize),
        out,
    )
}

/// Walks an interrupted AAPCS64 user frame chain through the active page table.
pub fn user_callchain(pc: usize, fp: usize, sp: usize, out: &mut [u64]) -> usize {
    if !(1..USER_VA_END).contains(&sp) || !(1..USER_VA_END).contains(&fp) {
        return write_leaf(pc, out);
    }
    let lo = sp & !0xfff;
    let Some(hi) = lo.checked_add(USER_STACK_WINDOW) else {
        return write_leaf(pc, out);
    };
    let hi = hi.min(USER_VA_END);
    let Some(record_end) = fp.checked_add(2 * core::mem::size_of::<usize>()) else {
        return write_leaf(pc, out);
    };
    if fp < lo || record_end > hi {
        return write_leaf(pc, out);
    }
    axbacktrace::walk_fp(
        pc,
        fp,
        &(1..USER_VA_END),
        &(lo..hi),
        |va| super::nofault::read_user_word(va).map(|word| word as usize),
        out,
    )
}

fn write_leaf(pc: usize, out: &mut [u64]) -> usize {
    let Some(leaf) = out.first_mut() else {
        return 0;
    };
    *leaf = pc as u64;
    1
}
