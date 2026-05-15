#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::{vec, vec::Vec};
use core::{
    fmt,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
};

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
    #[cfg(feature = "alloc")]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    const OFFSET: usize = 0;
    #[cfg(feature = "alloc")]
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const OFFSET: usize = 1;

    #[cfg(feature = "alloc")]
    fn read(fp: usize) -> Option<Self> {
        if fp == 0 || !fp.is_multiple_of(core::mem::align_of::<Frame>()) {
            return None;
        }

        Some(unsafe { (fp as *const Frame).sub(Self::OFFSET).read() })
    }

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
#[cfg(feature = "alloc")]
pub fn unwind_stack(mut fp: usize) -> Vec<Frame> {
    let mut frames = vec![];

    let Some(fp_range) = FP_RANGE.get() else {
        if !axpanic::oops_in_progress() {
            // Avoid recursive output on panic/oops paths, but keep a diagnostic
            // for ordinary misuse before the backtrace subsystem is ready.
            log::error!("Backtrace not initialized. Call `axbacktrace::init` first.");
        }
        return frames;
    };

    let mut depth = 0;
    let max_depth = max_depth();

    while fp_range.contains(&fp)
        && depth < max_depth
        && let Some(frame) = Frame::read(fp)
    {
        frames.push(frame);

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
    cfg!(feature = "alloc")
}

#[allow(dead_code)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Inner {
    Unsupported,
    Disabled,
    #[cfg(feature = "alloc")]
    Captured(Vec<Frame>),
}

/// A captured OS thread stack backtrace.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Backtrace {
    inner: Inner,
}

pub struct BacktraceReport<'a> {
    backtrace: &'a Backtrace,
    kind: &'static str,
}

impl Backtrace {
    /// Capture the current thread's stack backtrace.
    pub fn capture() -> Self {
        #[cfg(not(feature = "alloc"))]
        {
            Self {
                inner: Inner::Disabled,
            }
        }
        #[cfg(feature = "alloc")]
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

            // prevent this frame from being tail-call optimised away
            core::hint::black_box(());

            Self {
                inner: Inner::Captured(frames),
            }
        }
    }

    /// Capture the stack backtrace from a trap.
    #[allow(unused_variables)]
    pub fn capture_trap(fp: usize, ip: usize, ra: usize) -> Self {
        #[cfg(not(feature = "alloc"))]
        {
            Self {
                inner: Inner::Disabled,
            }
        }
        #[cfg(feature = "alloc")]
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

    pub fn report(&self, kind: &'static str) -> BacktraceReport<'_> {
        BacktraceReport {
            backtrace: self,
            kind,
        }
    }
}

impl fmt::Display for BacktraceReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else if cfg!(target_arch = "riscv64") {
            "riscv64"
        } else if cfg!(target_arch = "riscv32") {
            "riscv32"
        } else if cfg!(target_arch = "loongarch64") {
            "loongarch64"
        } else {
            "unknown"
        };

        writeln!(
            f,
            "BACKTRACE_BEGIN kind={} arch={} alloc={} dwarf={}",
            self.kind,
            arch,
            cfg!(feature = "alloc"),
            cfg!(feature = "dwarf")
        )?;

        match &self.backtrace.inner {
            Inner::Unsupported => {
                writeln!(f, "BT_ERROR unsupported")?;
            }
            Inner::Disabled => {
                if cfg!(feature = "alloc") {
                    writeln!(f, "BT_ERROR disabled")?;
                } else {
                    writeln!(f, "BT_ERROR requires_alloc")?;
                }
            }
            #[cfg(feature = "alloc")]
            Inner::Captured(frames) => {
                for (i, raw) in frames.iter().enumerate() {
                    writeln!(f, "BT {i} ip={:#x} fp={:#x}", raw.ip, raw.fp)?;
                }
            }
        }

        writeln!(f, "BACKTRACE_END")
    }
}

impl fmt::Display for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Unsupported => {
                writeln!(f, "<unwinding unsupported>")
            }
            Inner::Disabled => {
                if cfg!(feature = "alloc") {
                    writeln!(f, "<backtrace disabled>")
                } else {
                    writeln!(f, "<backtrace requires alloc>")
                }
            }
            #[cfg(feature = "alloc")]
            Inner::Captured(frames) => {
                writeln!(f, "Backtrace:")?;
                cfg_if::cfg_if! {
                    if #[cfg(feature = "dwarf")] {
                        dwarf::fmt_frames(f, frames)
                    } else {
                        for (i, raw) in frames.iter().enumerate() {
                            writeln!(f, "{i:>4}: {raw}")?;
                        }
                        Ok(())
                    }
                }
            }
        }
    }
}

impl fmt::Debug for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    fn init_for_tests() {
        init(0..0, 0..usize::MAX);
        set_max_depth(32);
    }

    fn boxed_frame_chain(ips: &[usize]) -> (Box<[Frame]>, usize) {
        let mut frames = ips
            .iter()
            .map(|&ip| Frame { fp: 0, ip })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        let ptr = frames.as_mut_ptr();
        for i in 0..frames.len() {
            let next_fp = if i + 1 < frames.len() {
                unsafe { ptr.add(i + 1) as usize }
            } else {
                0
            };
            frames[i].fp = next_fp;
        }
        (frames, ptr as usize)
    }

    #[test]
    fn report_formats_backtrace_blocks() {
        init_for_tests();
        let (_frames, start_fp) = boxed_frame_chain(&[0x1111, 0x2222]);
        let bt = Backtrace {
            inner: Inner::Captured(unwind_stack(start_fp)),
        };
        let s = alloc::format!("{}", bt.report("panic"));
        assert!(s.contains("BACKTRACE_BEGIN kind=panic"));
        assert!(s.contains("BT 0 ip=0x1111"));
        assert!(s.contains("BACKTRACE_END"));
    }
}
