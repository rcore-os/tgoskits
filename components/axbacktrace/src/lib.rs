#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::{boxed::Box, vec::Vec};
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
static STACK_BOUNDS: Once<InstalledStackBounds> = Once::new();
const RAW_BACKTRACE_MAX_DEPTH: usize = 32;

/// Exact mapped stack window for the execution context being unwound.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackBounds {
    start: usize,
    end: usize,
}

impl StackBounds {
    /// Creates one half-open stack window.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    fn into_range(self) -> Option<Range<usize>> {
        (self.start < self.end).then_some(self.start..self.end)
    }
}

/// Returns the current execution context's exact mapped kernel-stack window.
pub type StackBoundsProvider = fn() -> Option<StackBounds>;

enum InstalledStackBounds {
    Fixed(Range<usize>),
    Provider(StackBoundsProvider),
}

#[cfg(target_arch = "x86_64")]
const TARGET_ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const TARGET_ARCH: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
const TARGET_ARCH: &str = "riscv64";
#[cfg(target_arch = "riscv32")]
const TARGET_ARCH: &str = "riscv32";
#[cfg(target_arch = "loongarch64")]
const TARGET_ARCH: &str = "loongarch64";
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "riscv32",
    target_arch = "loongarch64"
)))]
const TARGET_ARCH: &str = "unknown";

/// Initializes the backtrace library for one permanently fixed stack window.
///
/// # Safety
///
/// `stack_bounds` must remain mapped and readable for every later synchronous
/// walk and must not include unmapped holes or user-controlled memory. A
/// multitasking runtime should use [`init_with_stack_provider`] instead.
pub unsafe fn init(ip_range: Range<usize>, stack_bounds: Range<usize>) {
    install(ip_range, InstalledStackBounds::Fixed(stack_bounds));
}

/// Initializes the backtrace library with a current-stack capability provider.
///
/// # Safety
///
/// `stack_bounds` must return only the mapped, readable kernel-stack allocation
/// that owns the calling context. The returned window must stay valid until the
/// provider returns and throughout the immediately following synchronous walk;
/// it must never include unmapped holes or user-controlled memory. The provider
/// must remain callable until shutdown and must not allocate, block, or panic.
pub unsafe fn init_with_stack_provider(ip_range: Range<usize>, stack_bounds: StackBoundsProvider) {
    install(ip_range, InstalledStackBounds::Provider(stack_bounds));
}

fn install(ip_range: Range<usize>, stack_bounds: InstalledStackBounds) {
    IP_RANGE.call_once(|| ip_range);
    STACK_BOUNDS.call_once(|| stack_bounds);
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
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    const OFFSET: usize = 0;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const OFFSET: usize = 1;

    fn read_in_range(fp: usize, readable_stack: &Range<usize>) -> Option<Self> {
        if fp == 0 || !fp.is_multiple_of(core::mem::align_of::<Frame>()) {
            return None;
        }

        let record_size = core::mem::size_of::<Frame>();
        let record_offset = Self::OFFSET.checked_mul(record_size)?;
        let record_start = fp.checked_sub(record_offset)?;
        let record_end = record_start.checked_add(record_size)?;
        if record_start < readable_stack.start || record_end > readable_stack.end {
            return None;
        }

        // SAFETY: `record_start` is aligned because `fp` is aligned and the
        // architecture offset is a multiple of Frame alignment. Bounds above
        // cover the complete load; `init`'s provider contract guarantees that
        // this exact current-stack window is mapped and readable for the walk.
        Some(unsafe { (record_start as *const Frame).read() })
    }

    // The stored IP is the return address (instruction after the call).
    // Subtracting the minimum instruction size gives an address that falls
    // within the calling function, which is what DWARF/ELF symbolizers expect.
    #[cfg(target_arch = "x86_64")]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(1) // variable-length, 1 byte minimum
    }
    #[cfg(target_arch = "aarch64")]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(4) // fixed 4-byte instructions
    }
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(2) // C extension: 2-byte minimum
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(4) // fixed 4-byte instructions
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fp={:#x}, ip={:#x}", self.fp, self.ip)
    }
}

/// Capacity of the on-stack capture buffer. Matches the default `max_depth()`.
#[cfg(feature = "alloc")]
const CAPTURE_CAPACITY: usize = 32;

/// On-stack scratch buffer used during FP walking to avoid heap allocation
/// in the hot unwinding loop. Converted to `Box<[Frame]>` after the walk.
#[cfg(feature = "alloc")]
#[derive(Clone)]
struct CaptureBuf {
    frames: [Frame; CAPTURE_CAPACITY],
    len: usize,
}

#[cfg(feature = "alloc")]
impl CaptureBuf {
    const EMPTY: Self = Self {
        frames: [Frame { fp: 0, ip: 0 }; CAPTURE_CAPACITY],
        len: 0,
    };

    fn push(&mut self, frame: Frame) -> bool {
        if self.len < CAPTURE_CAPACITY {
            self.frames[self.len] = frame;
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Insert a frame at the front, shifting existing frames right.
    /// If the buffer is full, the last (deepest) frame is evicted to make room.
    fn insert_front(&mut self, frame: Frame) {
        let end = if self.len < CAPTURE_CAPACITY {
            self.len += 1;
            self.len
        } else {
            CAPTURE_CAPACITY // evict the deepest frame
        };
        self.frames.copy_within(0..end - 1, 1);
        self.frames[0] = frame;
    }

    fn first_mut(&mut self) -> Option<&mut Frame> {
        if self.len > 0 {
            Some(&mut self.frames[0])
        } else {
            None
        }
    }

    /// Convert to a heap-allocated boxed slice trimmed to the actual length.
    fn into_boxed_slice(self) -> Box<[Frame]> {
        self.frames[..self.len].into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalkError {
    Uninitialized,
    StackBoundsUnavailable,
    InvalidStackBounds,
    InvalidFramePointer,
}

/// Core frame pointer walking logic. Calls `callback` for each valid frame.
/// The callback returns `false` to stop unwinding (e.g., buffer full).
#[cfg(feature = "alloc")]
fn walk_stack(fp: usize, callback: impl FnMut(Frame) -> bool) -> Result<(), WalkError> {
    walk_stack_with_limit(fp, max_depth(), callback)
}

#[cfg(feature = "alloc")]
fn walk_stack_with_limit(
    fp: usize,
    depth_limit: usize,
    callback: impl FnMut(Frame) -> bool,
) -> Result<(), WalkError> {
    let stack = resolve_installed_stack_bounds()?;
    walk_stack_with_ranges(fp, depth_limit, Some(&stack), IP_RANGE.get(), callback)
}

fn resolve_stack_bounds(provider: Option<StackBoundsProvider>) -> Result<Range<usize>, WalkError> {
    let provider = provider.ok_or(WalkError::Uninitialized)?;
    provider()
        .ok_or(WalkError::StackBoundsUnavailable)?
        .into_range()
        .ok_or(WalkError::InvalidStackBounds)
}

fn resolve_installed_stack_bounds() -> Result<Range<usize>, WalkError> {
    match STACK_BOUNDS.get().ok_or(WalkError::Uninitialized)? {
        InstalledStackBounds::Fixed(bounds) => (bounds.start < bounds.end)
            .then(|| bounds.clone())
            .ok_or(WalkError::InvalidStackBounds),
        InstalledStackBounds::Provider(provider) => resolve_stack_bounds(Some(*provider)),
    }
}

fn walk_stack_with_ranges(
    mut fp: usize,
    depth_limit: usize,
    fp_range: Option<&Range<usize>>,
    ip_range: Option<&Range<usize>>,
    mut callback: impl FnMut(Frame) -> bool,
) -> Result<(), WalkError> {
    let Some(fp_range) = fp_range else {
        return Err(WalkError::Uninitialized);
    };
    if depth_limit == 0 {
        return Ok(());
    }

    let mut depth = 0;

    while depth < depth_limit {
        let Some(frame) = Frame::read_in_range(fp, fp_range) else {
            return Err(WalkError::InvalidFramePointer);
        };
        // Skip frames whose IP is outside the kernel text range.
        // We continue unwinding rather than stopping, as a corrupted
        // IP does not necessarily mean the FP chain is broken.
        // Skipped frames still count against the depth budget to prevent
        // infinite loops on corrupted FP chains with bad IPs.
        let next_fp = frame.fp;
        // Check FP progress before IP filtering: a bad IP can be skipped, but
        // a non-advancing FP would otherwise keep revisiting the same frame.
        if next_fp != 0 && next_fp <= fp {
            return Err(WalkError::InvalidFramePointer);
        }
        if next_fp != 0
            && let Some(large_stack_end) = fp.checked_add(8 * 1024 * 1024)
            && next_fp >= large_stack_end
        {
            return Err(WalkError::InvalidFramePointer);
        }

        if let Some(ip_range) = ip_range
            && !ip_range.contains(&frame.ip)
        {
            if next_fp == 0 {
                break;
            }
            fp = next_fp;
            depth += 1;
            continue;
        }

        if !callback(frame) {
            break;
        }

        if next_fp == 0 {
            break;
        }

        fp = next_fp;
        depth += 1;
    }
    Ok(())
}

#[cfg(feature = "alloc")]
fn unwind_core(fp: usize, callback: impl FnMut(Frame) -> bool) {
    match walk_stack(fp, callback) {
        Ok(()) => {}
        Err(WalkError::Uninitialized) => {
            log::error!("Backtrace not initialized. Call `axbacktrace::init` first.");
        }
        Err(WalkError::StackBoundsUnavailable) => {
            log::error!("Backtrace has no current kernel-stack capability.");
        }
        Err(WalkError::InvalidStackBounds) => {
            log::error!("Backtrace stack provider returned an invalid range.");
        }
        Err(WalkError::InvalidFramePointer) => {
            log::error!("Backtrace stopped at an invalid frame pointer.");
        }
    }
}

/// Unwind the stack from the given frame pointer.
#[cfg(feature = "alloc")]
pub fn unwind_stack(fp: usize) -> Vec<Frame> {
    let mut frames = Vec::new();
    unwind_core(fp, |frame| {
        frames.push(frame);
        true
    });
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

/// Streams a machine-readable current-task backtrace without allocating.
///
/// This is intended for panic and other fatal paths where heap allocation,
/// symbol lookup, and ordinary logging are unsafe. Frame walking and formatting
/// are bounded by [`max_depth`]. The supplied writer decides how output is
/// transported and may stop the walk by returning [`fmt::Error`].
pub fn write_current_raw(writer: &mut impl fmt::Write, kind: &'static str) -> fmt::Result {
    let Some(frame_pointer) = current_frame_pointer() else {
        writeln!(
            writer,
            "BACKTRACE_BEGIN kind={kind} arch={TARGET_ARCH} alloc=false dwarf=false"
        )?;
        writeln!(writer, "BT_ERROR unsupported")?;
        return writeln!(writer, "BACKTRACE_END");
    };
    write_raw_from_frame_pointer(writer, kind, frame_pointer)
}

fn write_raw_from_frame_pointer(
    writer: &mut impl fmt::Write,
    kind: &'static str,
    frame_pointer: usize,
) -> fmt::Result {
    let stack = match resolve_installed_stack_bounds() {
        Ok(stack) => stack,
        Err(error) => return write_raw_stack_error(writer, kind, error),
    };
    write_raw_from_frame_pointer_with_ranges(
        writer,
        kind,
        frame_pointer,
        Some(&stack),
        IP_RANGE.get(),
    )
}

fn write_raw_stack_error(
    writer: &mut impl fmt::Write,
    kind: &'static str,
    error: WalkError,
) -> fmt::Result {
    writeln!(
        writer,
        "BACKTRACE_BEGIN kind={kind} arch={TARGET_ARCH} alloc=false dwarf=false"
    )?;
    let reason = match error {
        WalkError::Uninitialized => "uninitialized",
        WalkError::StackBoundsUnavailable => "stack_bounds_unavailable",
        WalkError::InvalidStackBounds => "invalid_stack_bounds",
        WalkError::InvalidFramePointer => "invalid_frame_pointer",
    };
    writeln!(writer, "BT_ERROR {reason}")?;
    writeln!(writer, "BACKTRACE_END")
}

fn write_raw_from_frame_pointer_with_ranges(
    writer: &mut impl fmt::Write,
    kind: &'static str,
    frame_pointer: usize,
    fp_range: Option<&Range<usize>>,
    ip_range: Option<&Range<usize>>,
) -> fmt::Result {
    writeln!(
        writer,
        "BACKTRACE_BEGIN kind={kind} arch={TARGET_ARCH} alloc=false dwarf=false"
    )?;

    let mut frame_index = 0usize;
    let mut write_error = None;
    let walk_result = walk_stack_with_ranges(
        frame_pointer,
        max_depth().min(RAW_BACKTRACE_MAX_DEPTH),
        fp_range,
        ip_range,
        |frame| match writeln!(
            writer,
            "BT {frame_index} ip={:#x} fp={:#x}",
            frame.ip, frame.fp
        ) {
            Ok(()) => {
                frame_index += 1;
                true
            }
            Err(error) => {
                write_error = Some(error);
                false
            }
        },
    );
    if let Some(error) = write_error {
        return Err(error);
    }
    match walk_result {
        Ok(()) => {}
        Err(WalkError::Uninitialized) => writeln!(writer, "BT_ERROR uninitialized")?,
        Err(WalkError::StackBoundsUnavailable) => {
            writeln!(writer, "BT_ERROR stack_bounds_unavailable")?
        }
        Err(WalkError::InvalidStackBounds) => writeln!(writer, "BT_ERROR invalid_stack_bounds")?,
        Err(WalkError::InvalidFramePointer) => writeln!(writer, "BT_ERROR invalid_frame_pointer")?,
    }
    writeln!(writer, "BACKTRACE_END")
}

#[cfg(target_arch = "x86_64")]
fn current_frame_pointer() -> Option<usize> {
    let frame_pointer: usize;
    // SAFETY: reading the current frame-pointer register has no side effects.
    // The value is not dereferenced until a complete Frame record has been
    // validated against the initialized readable-stack range.
    unsafe {
        core::arch::asm!(
            "mov {frame_pointer}, rbp",
            frame_pointer = out(reg) frame_pointer,
            options(nomem, nostack, preserves_flags)
        )
    };
    Some(frame_pointer)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn current_frame_pointer() -> Option<usize> {
    let frame_pointer: usize;
    // SAFETY: reading the current frame-pointer register has no side effects.
    // The value is checked against the initialized readable-stack range before
    // any memory access.
    unsafe {
        core::arch::asm!(
            "mv {frame_pointer}, s0",
            frame_pointer = out(reg) frame_pointer,
            options(nomem, nostack)
        )
    };
    Some(frame_pointer)
}

#[cfg(target_arch = "aarch64")]
fn current_frame_pointer() -> Option<usize> {
    let frame_pointer: usize;
    // SAFETY: reading the current frame-pointer register has no side effects.
    // The value is checked against the initialized readable-stack range before
    // any memory access.
    unsafe {
        core::arch::asm!(
            "mov {frame_pointer}, x29",
            frame_pointer = out(reg) frame_pointer,
            options(nomem, nostack)
        )
    };
    Some(frame_pointer)
}

#[cfg(target_arch = "loongarch64")]
fn current_frame_pointer() -> Option<usize> {
    let frame_pointer: usize;
    // SAFETY: reading the current frame-pointer register has no side effects.
    // The value is checked against the initialized readable-stack range before
    // any memory access.
    unsafe {
        core::arch::asm!(
            "move {frame_pointer}, $fp",
            frame_pointer = out(reg) frame_pointer,
            options(nomem, nostack)
        )
    };
    Some(frame_pointer)
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv32",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
fn current_frame_pointer() -> Option<usize> {
    None
}

#[allow(dead_code)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Inner {
    Unsupported,
    Disabled,
    #[cfg(feature = "alloc")]
    Captured(Box<[Frame]>),
}

/// A captured OS thread stack backtrace.
///
/// Internally stores frames as a `Box<[Frame]>` (trimmed to actual length).
/// Capture uses a stack-allocated scratch buffer so the FP walking loop
/// itself is allocation-free; the single `Box` allocation happens only after
/// the walk completes.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Backtrace {
    inner: Inner,
    kind: Option<&'static str>,
}

impl Backtrace {
    /// Capture the current thread's stack backtrace.
    pub fn capture() -> Self {
        #[cfg(not(feature = "alloc"))]
        return Self {
            inner: Inner::Disabled,
            kind: None,
        };

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
                        kind: None,
                    };
                }
            }

            let mut buf = CaptureBuf::EMPTY;
            unwind_core(fp, |frame| buf.push(frame));

            core::hint::black_box(());

            Self {
                inner: Inner::Captured(buf.into_boxed_slice()),
                kind: None,
            }
        }
    }

    /// Capture the stack backtrace from a trap.
    ///
    /// - `fp`: frame pointer from the trap context
    /// - `ip`: faulting instruction pointer (the PC from the trap frame)
    /// - `ra`: return address (link register). On x86_64 this is always 0
    ///   since the return address is stored on the stack as part of the FP chain.
    #[allow(unused_variables)]
    pub fn capture_trap(fp: usize, ip: usize, ra: usize) -> Self {
        #[cfg(not(feature = "alloc"))]
        return Self {
            inner: Inner::Disabled,
            kind: None,
        };

        #[cfg(feature = "alloc")]
        {
            let mut buf = CaptureBuf::EMPTY;
            unwind_core(fp, |frame| buf.push(frame));

            // If the first unwound frame's IP is outside the kernel text,
            // it is likely the saved return address was not yet set (e.g.
            // leaf function fault). Replace it with the link register (ra)
            // only when ra is valid and within the kernel text range.
            // Note: on x86_64, ra=0 is always passed, so this branch
            // never fires for x86_64.
            if let Some(first) = buf.first_mut()
                && let Some(ip_range) = IP_RANGE.get()
                && !ip_range.contains(&first.ip)
                && ra != 0
                && ip_range.contains(&ra)
            {
                first.ip = ra;
            }

            buf.insert_front(Frame {
                fp,
                ip: ip.wrapping_add(1),
            });

            Self {
                inner: Inner::Captured(buf.into_boxed_slice()),
                kind: None,
            }
        }
    }

    /// Sets the backtrace kind for machine-parseable raw output via [`Display`].
    pub fn kind(mut self, kind: &'static str) -> Self {
        self.kind = Some(kind);
        self
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

impl Backtrace {
    fn fmt_raw_block(&self, f: &mut fmt::Formatter<'_>, kind: &'static str) -> fmt::Result {
        let arch = TARGET_ARCH;

        writeln!(
            f,
            "BACKTRACE_BEGIN kind={} arch={} alloc={} dwarf={}",
            kind,
            arch,
            cfg!(feature = "alloc"),
            cfg!(feature = "dwarf")
        )?;

        match &self.inner {
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
        if let Some(kind) = self.kind {
            return self.fmt_raw_block(f, kind);
        }

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
                #[cfg(feature = "dwarf")]
                return dwarf::fmt_frames(f, frames);
                #[cfg(not(feature = "dwarf"))]
                {
                    for (i, raw) in frames.iter().enumerate() {
                        writeln!(f, "{i:>4}: {raw}")?;
                    }
                    Ok(())
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
    use alloc::{boxed::Box, format, vec::Vec};

    use super::*;

    fn test_stack_bounds() -> Option<StackBounds> {
        Some(StackBounds::new(0, usize::MAX))
    }

    fn init_for_tests() {
        // SAFETY: tests construct live frame chains in process memory and keep
        // their allocations alive for every unwind operation.
        unsafe { init_with_stack_provider(0..usize::MAX, test_stack_bounds) };
        set_max_depth(32);
    }

    #[test]
    fn stack_bounds_are_resolved_for_each_walk() {
        use core::sync::atomic::{AtomicUsize, Ordering};

        static START: AtomicUsize = AtomicUsize::new(0x1000);
        fn moving_bounds() -> Option<StackBounds> {
            let start = START.load(Ordering::Acquire);
            Some(StackBounds::new(start, start + 0x1000))
        }

        assert_eq!(
            resolve_stack_bounds(Some(moving_bounds)),
            Ok(0x1000..0x2000)
        );
        START.store(0x3000, Ordering::Release);
        assert_eq!(
            resolve_stack_bounds(Some(moving_bounds)),
            Ok(0x3000..0x4000)
        );
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

    // --- CaptureBuf internal tests ---

    #[test]
    fn capture_buf_push_and_insert() {
        let mut buf = CaptureBuf::EMPTY;
        assert!(buf.push(Frame { fp: 1, ip: 0x10 }));
        assert!(buf.push(Frame { fp: 2, ip: 0x20 }));
        assert_eq!(buf.len, 2);

        buf.insert_front(Frame { fp: 0, ip: 0x05 });
        assert_eq!(buf.len, 3);
        assert_eq!(
            &*buf.clone().into_boxed_slice(),
            &[
                Frame { fp: 0, ip: 0x05 },
                Frame { fp: 1, ip: 0x10 },
                Frame { fp: 2, ip: 0x20 }
            ]
        );
    }

    #[test]
    fn capture_buf_overflow_evicts_deepest() {
        let mut buf = CaptureBuf::EMPTY;
        for i in 0..CAPTURE_CAPACITY {
            assert!(buf.push(Frame { fp: i, ip: i }));
        }
        assert!(!buf.push(Frame { fp: 0, ip: 0 })); // full
        buf.insert_front(Frame { fp: 99, ip: 0x99 });
        assert_eq!(buf.len, CAPTURE_CAPACITY);
        let boxed = buf.into_boxed_slice();
        assert_eq!(boxed[0], Frame { fp: 99, ip: 0x99 });
        assert_eq!(boxed.len(), CAPTURE_CAPACITY);
    }

    #[test]
    fn into_boxed_slice_trims_to_len() {
        let mut buf = CaptureBuf::EMPTY;
        buf.push(Frame { fp: 1, ip: 0x10 });
        buf.push(Frame { fp: 2, ip: 0x20 });
        let boxed = buf.into_boxed_slice();
        assert_eq!(boxed.len(), 2);
        assert_eq!(boxed[0], Frame { fp: 1, ip: 0x10 });
    }

    // --- Frame::read / unwind_core internal tests ---

    #[test]
    fn unwind_stack_collects_fake_frames() {
        init_for_tests();
        let (frames, start_fp) = boxed_frame_chain(&[0x1111, 0x2222, 0x3333]);
        let out = unwind_stack(start_fp);
        assert_eq!(out, frames.as_ref());
    }

    #[test]
    fn unwind_core_callback_stop_early() {
        init_for_tests();
        let (_chain, start_fp) = boxed_frame_chain(&[0x1, 0x2, 0x3, 0x4, 0x5]);
        let mut count = 0;
        unwind_core(start_fp, |_| {
            count += 1;
            count < 3
        });
        assert_eq!(count, 3);
    }

    #[test]
    fn raw_writer_streams_frames_without_building_a_backtrace_object() {
        init_for_tests();
        let (_chain, start_fp) = boxed_frame_chain(&[0x1111, 0x2222]);
        let mut output = alloc::string::String::new();

        write_raw_from_frame_pointer(&mut output, "panic", start_fp).unwrap();

        assert!(output.contains("BACKTRACE_BEGIN kind=panic"));
        assert!(output.contains("BT 0 ip=0x1111"));
        assert!(output.contains("BT 1 ip=0x2222"));
        assert!(output.ends_with("BACKTRACE_END\n"));
    }

    #[test]
    fn raw_writer_caps_fatal_path_depth() {
        init_for_tests();
        set_max_depth(RAW_BACKTRACE_MAX_DEPTH * 2);
        let ips = (0..RAW_BACKTRACE_MAX_DEPTH + 8)
            .map(|index| 0x4000 + index)
            .collect::<Vec<_>>();
        let (_chain, start_fp) = boxed_frame_chain(&ips);
        let mut output = alloc::string::String::new();

        write_raw_from_frame_pointer(&mut output, "panic", start_fp).unwrap();

        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("BT "))
                .count(),
            32
        );
        set_max_depth(32);
    }

    #[test]
    fn raw_writer_reports_uninitialized_without_reading_the_frame_pointer() {
        let mut output = alloc::string::String::new();

        write_raw_from_frame_pointer_with_ranges(&mut output, "panic", usize::MAX, None, None)
            .unwrap();

        assert!(output.contains("BT_ERROR uninitialized"));
        assert!(!output.lines().any(|line| line.starts_with("BT ")));
        assert!(output.ends_with("BACKTRACE_END\n"));
    }

    #[test]
    fn raw_writer_rejects_a_frame_record_outside_the_readable_stack_range() {
        let mut output = alloc::string::String::new();
        let readable_stack = 0x1000..0x2000;

        write_raw_from_frame_pointer_with_ranges(
            &mut output,
            "panic",
            0x2000,
            Some(&readable_stack),
            Some(&(0..usize::MAX)),
        )
        .unwrap();

        assert!(output.contains("BT_ERROR invalid_frame_pointer"));
        assert!(!output.lines().any(|line| line.starts_with("BT ")));
    }

    #[test]
    fn unwind_stack_stops_on_non_advancing_frame_pointer() {
        init_for_tests();
        let mut frames = [Frame { fp: 0, ip: 0x1111 }, Frame { fp: 0, ip: 0x2222 }];
        let base = frames.as_mut_ptr();
        frames[0].fp = unsafe { base.add(1) as usize };
        frames[1].fp = base as usize;

        let out = unwind_stack(base as usize);
        assert_eq!(out, [frames[0]]);
    }

    #[test]
    fn frame_read_rejects_null_and_misaligned() {
        let readable = 0..usize::MAX;
        assert!(Frame::read_in_range(0, &readable).is_none());
        assert!(Frame::read_in_range(1, &readable).is_none());
        assert!(Frame::read_in_range(3, &readable).is_none());
    }

    // --- capture_trap with Inner::Captured verification ---

    #[test]
    fn capture_trap_ra_not_substituted_with_wide_range() {
        init_for_tests();
        let (_chain, start_fp) = boxed_frame_chain(&[0xDEAD]);
        let bt = Backtrace::capture_trap(start_fp, 0x1000, 0xBEEF);
        let Inner::Captured(frames) = &bt.inner else {
            panic!("expected Captured")
        };
        assert_eq!(frames[0].ip, 0x1001);
        assert_eq!(frames[1].ip, 0xDEAD); // not replaced by ra
    }

    // --- Stress tests ---

    /// Build a chain that fills the buffer to exactly CAPTURE_CAPACITY.
    /// Then unwind and verify every frame is collected.
    #[test]
    fn stress_fill_buffer_exactly() {
        init_for_tests();
        let ips: Vec<usize> = (0..CAPTURE_CAPACITY).map(|i| 0xA000 + i).collect();
        let (chain, start_fp) = boxed_frame_chain(&ips);
        let out = unwind_stack(start_fp);
        assert_eq!(out.len(), CAPTURE_CAPACITY);
        assert_eq!(out.as_slice(), chain.as_ref());
    }

    /// Build a chain with CAPTURE_CAPACITY - 1 frames, then capture_trap.
    /// The trap frame is inserted at front, total = CAPTURE_CAPACITY, no eviction.
    #[test]
    fn stress_trap_near_capacity() {
        init_for_tests();
        let n = CAPTURE_CAPACITY - 1;
        let ips: Vec<usize> = (0..n).map(|i| 0xB000 + i).collect();
        let (_chain, start_fp) = boxed_frame_chain(&ips);

        let bt = Backtrace::capture_trap(start_fp, 0xC000, 0);
        let Inner::Captured(frames) = &bt.inner else {
            panic!("expected Captured")
        };
        assert_eq!(frames.len(), CAPTURE_CAPACITY);
        // Trap frame is at front with ip = 0xC000 + 1
        assert_eq!(frames[0].ip, 0xC001);
        // Remaining frames follow
        for (i, f) in frames[1..].iter().enumerate() {
            assert_eq!(f.ip, 0xB000 + i);
        }
    }

    /// Build a chain with CAPTURE_CAPACITY frames, then capture_trap.
    /// The trap insert_front evicts the deepest frame.
    #[test]
    fn stress_trap_overflow_evicts_deepest() {
        init_for_tests();
        let ips: Vec<usize> = (0..CAPTURE_CAPACITY).map(|i| 0xD000 + i).collect();
        let (_chain, start_fp) = boxed_frame_chain(&ips);

        let bt = Backtrace::capture_trap(start_fp, 0xE000, 0);
        let Inner::Captured(frames) = &bt.inner else {
            panic!("expected Captured")
        };
        assert_eq!(frames.len(), CAPTURE_CAPACITY);
        // Trap frame at front
        assert_eq!(frames[0].ip, 0xE001);
        // The first CAPTURE_CAPACITY - 1 unwound frames remain
        for (i, f) in frames[1..].iter().enumerate() {
            assert_eq!(f.ip, 0xD000 + i);
        }
        // The deepest frame (0xD000 + CAPTURE_CAPACITY - 1) was evicted
    }

    /// Build a chain deeper than max_depth and verify truncation.
    #[test]
    fn stress_deep_chain_truncation() {
        init_for_tests();
        set_max_depth(16);
        let ips: Vec<usize> = (0..64).map(|i| 0xF000 + i).collect();
        let (chain, start_fp) = boxed_frame_chain(&ips);

        let out = unwind_stack(start_fp);
        assert_eq!(out.len(), 16);
        // Only the first 16 frames should be collected
        assert_eq!(out.as_slice(), &chain[..16]);

        // Restore default
        set_max_depth(CAPTURE_CAPACITY);
    }

    /// Repeatedly create and drop Backtrace objects to verify no leaks or corruption.
    #[test]
    fn stress_repeated_create_drop() {
        init_for_tests();
        let (chain, start_fp) = boxed_frame_chain(&[0x100, 0x200, 0x300]);
        for _ in 0..500 {
            let bt = Backtrace::capture_trap(start_fp, 0x400, 0);
            let Inner::Captured(frames) = &bt.inner else {
                panic!("expected Captured")
            };
            assert!(frames.len() >= 3);
            drop(bt);
        }
        // Ensure the chain memory is still valid after all iterations
        let _ = &chain;
    }

    /// Interleave capture, Display formatting, and drop to verify no side effects.
    #[test]
    fn stress_interleaved_capture_format() {
        init_for_tests();
        let (chain, start_fp) = boxed_frame_chain(&[0x500, 0x600]);

        for i in 0..100 {
            let bt = Backtrace::capture_trap(start_fp, 0x700, 0);
            let s = format!("{bt}");
            // Raw block should contain the trap IP
            assert!(
                s.contains("0x701"),
                "iteration {i}: missing trap IP in output"
            );

            // Human-readable formatting
            let bt_human = Backtrace::capture_trap(start_fp, 0x700, 0);
            let human = format!("{bt_human}");
            assert!(!human.is_empty(), "iteration {i}: empty human output");

            drop(bt);
            drop(bt_human);
        }
        let _ = &chain;
    }

    /// Repeatedly clone a Backtrace and verify equality.
    #[test]
    fn stress_repeated_clone() {
        init_for_tests();
        let (chain, start_fp) = boxed_frame_chain(&[0x800, 0x900, 0xA00]);
        let original = Backtrace::capture_trap(start_fp, 0xB00, 0);

        for _ in 0..200 {
            let cloned = original.clone();
            assert_eq!(cloned, original);
        }
        let _ = &chain;
    }

    /// Verify Frame and Backtrace sizes remain stable (prevent accidental regressions).
    #[test]
    fn stress_size_stability() {
        // Frame is #[repr(C)] with two usize fields
        assert_eq!(
            core::mem::size_of::<Frame>(),
            2 * core::mem::size_of::<usize>()
        );
        assert_eq!(
            core::mem::align_of::<Frame>(),
            core::mem::align_of::<usize>()
        );

        // Backtrace contains Inner (discriminant + Box<[Frame]>) + Option<&'static str>
        // Size should be stable across compilations
        let bt_size = core::mem::size_of::<Backtrace>();
        assert!(
            bt_size > 0 && bt_size <= 48,
            "Backtrace size unexpected: {bt_size}"
        );

        // CaptureBuf is stack-allocated; verify it's reasonable
        let cap_size = core::mem::size_of::<CaptureBuf>();
        let expected =
            CAPTURE_CAPACITY * core::mem::size_of::<Frame>() + core::mem::size_of::<usize>();
        assert_eq!(cap_size, expected, "CaptureBuf size mismatch");
    }

    /// Verify Frame alignment and that misaligned pointers are rejected.
    #[test]
    fn stress_frame_alignment() {
        let align = core::mem::align_of::<Frame>();
        assert!(align > 0);
        assert!(align.is_power_of_two());

        // All valid FP values must be multiples of the alignment
        for offset in 1..align {
            assert!(
                Frame::read_in_range(offset, &(0..usize::MAX)).is_none(),
                "misaligned {offset} should fail"
            );
        }
        // Zero is always rejected
        assert!(Frame::read_in_range(0, &(0..usize::MAX)).is_none());
    }
}
