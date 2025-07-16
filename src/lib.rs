#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{
    fmt,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
};

use log::error;
use spin::Once;

#[cfg(feature = "dwarf")]
mod dwarf;

#[cfg(feature = "dwarf")]
pub use dwarf::{DwarfReader, FrameIter};

static IP_RANGE: Once<Range<usize>> = Once::new();
static FP_RANGE: Once<Range<usize>> = Once::new();

/// Initializes the backtrace library.
pub fn init(ip_range: Range<usize>, fp_range: Range<usize>) {
    IP_RANGE.call_once(|| ip_range);
    FP_RANGE.call_once(|| fp_range);
    #[cfg(feature = "dwarf")]
    dwarf::init();
}

/// Represents a single stack frame in the unwound stack.
#[repr(C)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub struct Frame {
    /// The frame pointer of the previous stack frame.
    pub fp: usize,
    /// The instruction pointer (program counter) after the function call.
    pub ip: usize,
}

impl Frame {
    // See https://github.com/rust-lang/backtrace-rs/blob/b65ab935fb2e0d59dba8966ffca09c9cc5a5f57c/src/symbolize/mod.rs#L145
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(1)
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fp={:#x}, ip={:#x}", self.fp, self.ip)
    }
}

/// Unwind the stack from the given frame pointer.
pub fn unwind_stack(mut fp: usize) -> Vec<Frame> {
    let offset = if cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64") {
        0
    } else {
        1
    };

    let mut frames = vec![];

    let Some(fp_range) = FP_RANGE.get() else {
        // We cannot panic here!
        error!("Backtrace not initialized. Call `axbacktrace::init` first.");
        return frames;
    };

    let mut depth = 0;
    let max_depth = max_depth();

    while fp > 0 && fp % align_of::<usize>() == 0 && fp_range.contains(&fp) && depth < max_depth {
        let frame: &Frame = unsafe { &*(fp as *const Frame).sub(offset) };
        frames.push(*frame);

        if let Some(large_stack_end) = fp.checked_add(8 * 1024 * 1024)
            && frame.fp >= large_stack_end
        {
            break;
        }

        fp = frame.fp;
        depth += 1;
    }

    frames
}

static MAX_DEPTH: AtomicUsize = AtomicUsize::new(32);

/// Sets the maximum depth for stack unwinding.
pub fn set_max_depth(depth: usize) {
    if depth > 0 {
        MAX_DEPTH.store(depth, Ordering::Relaxed);
    }
}
/// Returns the maximum depth for stack unwinding.
pub fn max_depth() -> usize {
    MAX_DEPTH.load(Ordering::Relaxed)
}

/// Returns whether the backtrace feature is enabled.
pub const fn is_enabled() -> bool {
    cfg!(feature = "dwarf")
}

#[allow(dead_code)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Inner {
    Unsupported,
    Disabled,
    #[cfg(feature = "dwarf")]
    Captured(Vec<Frame>),
}

/// A captured OS thread stack backtrace.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Backtrace {
    inner: Inner,
}

impl Backtrace {
    /// Capture the current thread's stack backtrace.
    pub fn capture() -> Self {
        #[cfg(not(feature = "dwarf"))]
        {
            Self {
                inner: Inner::Disabled,
            }
        }
        #[cfg(feature = "dwarf")]
        {
            use core::arch::asm;

            let fp: usize;
            cfg_if::cfg_if! {
                if #[cfg(target_arch = "x86_64")] {
                    unsafe { asm!("mov {ptr}, rbp", ptr = out(reg) fp) };
                } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
                    unsafe { asm!("addi {ptr}, s0, 0", ptr = out(reg) fp) };
                } else if #[cfg(target_arch = "aarch64")] {
                    unsafe { asm!("mov {ptr}, x29", ptr = out(reg) fp) };
                } else if #[cfg(target_arch = "loongarch64")] {
                    unsafe { asm!("move {ptr}, $fp", ptr = out(reg) fp) };
                } else {
                    return Self {
                        inner: Inner::Unsupported,
                    };
                }
            }

            let frames = unwind_stack(fp);
            Self {
                inner: Inner::Captured(frames),
            }
        }
    }

    /// Capture the stack backtrace from a trap.
    #[allow(unused_variables)]
    pub fn capture_trap(fp: usize, ip: usize, ra: usize) -> Self {
        #[cfg(not(feature = "dwarf"))]
        {
            Self {
                inner: Inner::Disabled,
            }
        }
        #[cfg(feature = "dwarf")]
        {
            let mut frames = unwind_stack(fp);
            if let Some(first) = frames.first_mut()
                && let Some(ip_range) = IP_RANGE.get()
                && !ip_range.contains(&first.ip)
            {
                first.ip = ra;
            }

            frames.insert(
                0,
                Frame {
                    fp,
                    ip: ip.wrapping_add(1),
                },
            );

            Self {
                inner: Inner::Captured(frames),
            }
        }
    }

    /// Visit each stack frame in the captured backtrace in order.
    ///
    /// Returns `None` if the backtrace is not captured.
    #[cfg(feature = "dwarf")]
    pub fn frames<'a>(&'a self) -> Option<FrameIter<'a>> {
        let Inner::Captured(capture) = &self.inner else {
            return None;
        };

        Some(FrameIter::new(capture))
    }
}

impl fmt::Display for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Unsupported => {
                writeln!(f, "<unwinding unsupported>")
            }
            Inner::Disabled => {
                writeln!(f, "<backtrace disabled>")
            }
            #[cfg(feature = "dwarf")]
            Inner::Captured(frames) => {
                writeln!(f, "Backtrace:")?;
                dwarf::fmt_frames(f, frames)
            }
        }
    }
}

impl fmt::Debug for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
