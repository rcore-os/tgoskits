use alloc::borrow::Cow;
use core::{cell::UnsafeCell, fmt, slice};

use addr2line::Context;
use log::{error, info};
use paste::paste;

pub type DwarfReader = gimli::EndianSlice<'static, gimli::RunTimeEndian>;

struct ContextCell(UnsafeCell<Option<Context<DwarfReader>>>);

// SAFETY: `CONTEXT` is written exactly once during `init()` at startup
// (single-threaded boot, enforced by the `INITIALIZED` flag). After
// initialization all access is read-only. `addr2line::FrameIter` borrows
// `Context` across iterator `next()` calls, which makes lock-based approaches
// (Mutex/OnceCell) unworkable because they would require holding a lock
// across the entire iteration.
unsafe impl Sync for ContextCell {}

static CONTEXT: ContextCell = ContextCell(UnsafeCell::new(None));
static INITIALIZED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

macro_rules! generate_sections {
    ($($name:ident),*) => {
        unsafe extern "C" {
            paste! {
                $(
                    safe static [<__start_ $name>]: [u8; 0];
                    safe static [<__stop_ $name>]: [u8; 0];
                )*
            }
        }

        paste! {
            $(
                let $name = DwarfReader::new(
                    unsafe {
                        core::slice::from_raw_parts(
                            [<__start_ $name>].as_ptr(),
                            [<__stop_ $name>]
                                .as_ptr()
                                .offset_from_unsigned([<__start_ $name>].as_ptr()),
                        )
                    },
                    gimli::RunTimeEndian::default(),
                );
            )*
        }
    };
}

pub fn init() {
    use core::sync::atomic::Ordering;

    if INITIALIZED.swap(true, Ordering::SeqCst) {
        log::warn!("axbacktrace::init() called more than once, skipping.");
        return;
    }

    generate_sections!(
        debug_abbrev,
        debug_addr,
        debug_aranges,
        debug_info,
        debug_line,
        debug_line_str,
        debug_ranges,
        debug_rnglists,
        debug_str,
        debug_str_offsets
    );

    let default_section = DwarfReader::new(&[], gimli::RunTimeEndian::default());

    match Context::from_sections(
        debug_abbrev.into(),
        debug_addr.into(),
        debug_aranges.into(),
        debug_info.into(),
        debug_line.into(),
        debug_line_str.into(),
        debug_ranges.into(),
        debug_rnglists.into(),
        debug_str.into(),
        debug_str_offsets.into(),
        default_section,
    ) {
        Ok(ctx) => {
            // SAFETY: single-threaded boot; no concurrent access possible.
            // INITIALIZED guard ensures this write happens exactly once.
            unsafe {
                *CONTEXT.0.get() = Some(ctx);
            }
            info!("Initialized addr2line context successfully.");
        }
        Err(e) => {
            // Graceful degradation: Context stays None, FrameIter returns
            // no frames, system continues without DWARF symbol resolution.
            error!("Failed to initialize addr2line context: {e}");
        }
    }
}

/// An iterator over the stack frames in a captured backtrace.
///
/// See [`Backtrace::frames`].
///
/// [`Backtrace::frames`]: crate::Backtrace::frames
pub struct FrameIter<'a> {
    src: slice::Iter<'a, crate::Frame>,
    inner: Option<(crate::Frame, addr2line::FrameIter<'static, DwarfReader>)>,
}

impl<'a> FrameIter<'a> {
    pub(crate) fn new(frames: &'a [crate::Frame]) -> Self {
        let src = frames.iter();
        Self { src, inner: None }
    }
}

impl Iterator for FrameIter<'_> {
    type Item = (crate::Frame, addr2line::Frame<'static, DwarfReader>);

    fn next(&mut self) -> Option<Self::Item> {
        let ptr = CONTEXT.0.get();
        // SAFETY: see `ContextCell` — read-only after `init()`.
        let ctx = unsafe { &*ptr }.as_ref()?;

        loop {
            if let Some((raw, inner)) = &mut self.inner
                && let Ok(Some(frame)) = inner.next()
            {
                return Some((*raw, frame));
            }

            let raw = self.src.next()?;
            self.inner = ctx
                .find_frames(raw.adjust_ip() as _)
                .skip_all_loads()
                .ok()
                .map(|x| (*raw, x));
        }
    }
}

fn fmt_frame<R: gimli::Reader>(
    f: &mut fmt::Formatter<'_>,
    frame: &addr2line::Frame<R>,
) -> fmt::Result {
    let func = frame
        .function
        .as_ref()
        .and_then(|func| func.demangle().ok())
        .unwrap_or(Cow::Borrowed("<unknown>"));
    writeln!(f, ": {func}")?;

    let Some(location) = &frame.location else {
        return Ok(());
    };
    write!(f, "            at ")?;

    let Some(file) = &location.file else {
        return write!(f, "??");
    };
    write!(f, "{file}")?;
    let Some(line) = location.line else {
        return Ok(());
    };
    write!(f, ":{line}")?;
    let Some(col) = location.column else {
        return Ok(());
    };
    write!(f, ":{col}")?;

    Ok(())
}

pub(crate) fn fmt_frames(f: &mut fmt::Formatter<'_>, frames: &[crate::Frame]) -> fmt::Result {
    // SAFETY: see `ContextCell` — read-only after `init()`.
    if unsafe { (*CONTEXT.0.get()).is_none() } {
        return write!(f, "Backtracing is not initialized.");
    }
    for (i, (raw, frame)) in FrameIter::new(frames).enumerate() {
        write!(f, "{i:>4}")?;
        fmt_frame(f, &frame)?;
        writeln!(f, " with {raw}")?;
    }

    Ok(())
}
