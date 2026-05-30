//! `perf_event_open(2)` runtime: dispatcher across kprobe / tracepoint /
//! software-bpf / uprobe perf event types, the file-like `PerfEvent`
//! wrapper, and the ringbuf output path used by the `bpf_perf_event_output`
//! helper.
//!
//! Ported from `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/perf/`). Adapted
//! to tgoskits' `ax_*` package naming and the in-tree FileLike trait (which
//! does not expose the source's `custom_mmap()` hook — ringbuf mmap is
//! handled inside the relevant `set_bpf_prog`/`enable` paths rather than
//! through the fd layer for now; see TODO in `perf/bpf.rs`).

pub mod bpf;
pub mod kprobe;
pub mod raw_tracepoint;
pub mod tracepoint;
pub mod uprobe;

use alloc::{borrow::Cow, boxed::Box, sync::Arc};
use core::{any::Any, ffi::c_void, fmt::Debug};

use ax_errno::{AxError, AxResult};
use ax_kspin::{SpinNoPreempt, SpinNoPreemptGuard};
use ax_lazyinit::LazyInit;
use axpoll::Pollable;
pub use bpf::BpfPerfEventWrapper;
use hashbrown::HashMap;
use kbpf_basic::{
    linux_bpf::perf_event_attr,
    perf::{PerfEventIoc, PerfProbeArgs, PerfTypeId},
};

use crate::{
    ebpf::transform::EbpfKernelAuxiliary,
    file::{FileLike, Kstat, add_file_like, get_file_like},
};

/// Behaviour every perf event implements. Each variant in the dispatcher
/// (kprobe / tracepoint / software-bpf / uprobe) provides a
/// `Box<dyn PerfEventOps>` that `PerfEvent` then drives through the file
/// layer (`ioctl`, `mmap`, `read`, etc.).
pub trait PerfEventOps: Pollable + Send + Sync + Debug {
    /// Begin firing into the registered BPF program / ringbuf.
    fn enable(&mut self) -> AxResult<()>;

    /// Stop firing without tearing down the event.
    fn disable(&mut self) -> AxResult<()>;

    /// `Any` upcast (mutable). Used by `perf_event_output` to recover the
    /// concrete `BpfPerfEventWrapper` from a `dyn PerfEventOps`.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Attach a BPF program to this event (`PERF_EVENT_IOC_SET_BPF`).
    fn set_bpf_prog(&mut self, _bpf_prog: Arc<dyn FileLike>) -> AxResult<()> {
        Err(AxError::Unsupported)
    }
}

/// File-like handle returned by `perf_event_open(2)`. Locks a
/// `Box<dyn PerfEventOps>` so the inner implementation can stay generic.
pub struct PerfEvent {
    event: SpinNoPreempt<Box<dyn PerfEventOps>>,
}

impl Debug for PerfEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerfEvent").finish()
    }
}

impl PerfEvent {
    /// Wrap a per-type perf event impl.
    pub fn new(event: Box<dyn PerfEventOps>) -> Self {
        PerfEvent {
            event: SpinNoPreempt::new(event),
        }
    }

    /// Borrow the inner impl under the lock.
    pub fn event(&self) -> SpinNoPreemptGuard<'_, Box<dyn PerfEventOps>> {
        self.event.lock()
    }
}

impl Pollable for PerfEvent {
    fn poll(&self) -> axpoll::IoEvents {
        self.event.lock().poll()
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: axpoll::IoEvents) {
        self.event.lock().register(context, events)
    }
}

impl FileLike for PerfEvent {
    fn read(&self, _dst: &mut crate::file::IoDst) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn write(&self, _src: &mut crate::file::IoSrc) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[perf_event]".into()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        let req = PerfEventIoc::try_from(cmd).map_err(|_| AxError::InvalidInput)?;
        match req {
            PerfEventIoc::Enable => {
                self.event.lock().enable()?;
            }
            PerfEventIoc::Disable => {
                self.event.lock().disable()?;
            }
            PerfEventIoc::SetBpf => {
                let bpf_prog_fd = arg as i32;
                let file = get_file_like(bpf_prog_fd)?;
                self.event.lock().set_bpf_prog(file)?;
            }
        }
        Ok(0)
    }
}

/// Dispatcher entry point for `perf_event_open(2)`. Reads the user-supplied
/// `perf_event_attr`, selects the per-type implementation, registers a
/// file-like in the current fd table and remembers a weak handle so the
/// ringbuf output path can locate the event by fd later.
pub fn perf_event_open(
    attr: &perf_event_attr,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u32,
) -> AxResult<isize> {
    let args =
        PerfProbeArgs::try_from_perf_attr::<EbpfKernelAuxiliary>(attr, pid, cpu, group_fd, flags)
            .map_err(|_| AxError::InvalidInput)?;
    let event: Box<dyn PerfEventOps> = match args.type_ {
        PerfTypeId::PERF_TYPE_KPROBE => Box::new(kprobe::perf_event_open_kprobe(args)?),
        PerfTypeId::PERF_TYPE_SOFTWARE => Box::new(bpf::perf_event_open_bpf(args)),
        PerfTypeId::PERF_TYPE_TRACEPOINT => Box::new(tracepoint::perf_event_open_tracepoint(args)?),
        PerfTypeId::PERF_TYPE_UPROBE => Box::new(uprobe::perf_event_open_uprobe(args)?),
        _ => {
            warn!("perf_event_open: unsupported type {:?}", args.type_);
            return Err(AxError::Unsupported);
        }
    };
    let event_arc: Arc<dyn FileLike> = Arc::new(PerfEvent::new(event));
    let fd = add_file_like(event_arc.clone(), false)?;

    PERF_FILE
        .get()
        .expect("perf subsystem not initialized")
        .lock()
        .insert(fd as usize, Arc::downgrade(&event_arc));

    Ok(fd as isize)
}

/// Map fd → weak<PerfEvent> so `bpf_perf_event_output` can locate the
/// target ringbuf without owning a strong reference (the user side owns
/// it via the fd).
static PERF_FILE: LazyInit<SpinNoPreempt<HashMap<usize, alloc::sync::Weak<dyn FileLike>>>> =
    LazyInit::new();

/// Initialize the perf-event runtime: build the fd→event lookup table.
pub fn perf_event_init() {
    PERF_FILE.init_once(SpinNoPreempt::new(HashMap::new()));
}

/// Implementation of `bpf_perf_event_output` helper: walk the fd→event map,
/// downcast the strong upgrade to `PerfEvent`, and have the bpf-software
/// variant write a record into the ringbuf.
pub fn perf_event_output(_ctx: *mut c_void, fd: usize, _flags: u32, data: &[u8]) -> AxResult<()> {
    let table = PERF_FILE.get().ok_or(AxError::NotFound)?;
    let mut map = table.lock();
    let weak = map.get(&fd).ok_or(AxError::NotFound)?;
    let Some(file) = weak.upgrade() else {
        map.remove(&fd);
        return Err(AxError::NotFound);
    };
    drop(map);

    let perf_event = file
        .into_any_arc()
        .downcast::<PerfEvent>()
        .map_err(|_| AxError::InvalidInput)?;
    let mut inner = perf_event.event();
    let bpf_event = inner
        .as_any_mut()
        .downcast_mut::<BpfPerfEventWrapper>()
        .ok_or(AxError::InvalidInput)?;
    bpf_event.write_event(data)?;
    Ok(())
}
