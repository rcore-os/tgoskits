//! `perf_event_open(2)` runtime: dispatcher across kprobe / tracepoint /
//! software-bpf / uprobe perf event types, the file-like `PerfEvent`
//! wrapper, and the ringbuf output path used by the `bpf_perf_event_output`
//! helper. The `mmap(perf_fd, ...)` path is wired through
//! `FileLike::device_mmap` → `PerfEventOps::device_mmap`, which allocates
//! the backing pages and asks `kbpf_basic` to initialize the
//! `perf_event_mmap_page` header.

pub mod bpf;
pub mod kprobe;
pub mod raw_tracepoint;
pub mod tracepoint;
pub mod uprobe;

use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec};
use core::{any::Any, ffi::c_void, fmt::Debug};

use ax_errno::{AxError, AxResult};
use ax_io::Read;
use ax_kspin::{SpinNoPreempt, SpinNoPreemptGuard};
use ax_lazyinit::LazyInit;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::MappingFlags;
use axpoll::Pollable;
pub use bpf::BpfPerfEventWrapper;
use hashbrown::HashMap;
use kbpf_basic::{
    linux_bpf::{PERF_FLAG_FD_CLOEXEC, perf_event_attr},
    perf::{PerfEventIoc, PerfProbeArgs, PerfTypeId},
};

use crate::{
    ebpf::{error::BpfResultExt, transform::EbpfKernelAuxiliary},
    file::{FileLike, Kstat, add_file_like, get_file_like},
    mm::VmBytes,
    pseudofs::DeviceMmap,
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

    /// Allocate the user-visible ringbuf and return its physical start
    /// address (length is the user-supplied mmap length, page-aligned)
    /// together with a retainer that owns the backing pages. The caller
    /// threads the retainer into `DeviceMmap::Physical(.., Some(anchor))`
    /// so the pages stay live for as long as the user mapping exists, even
    /// after `close(perf_fd)`. Only `bpf::BpfPerfEventWrapper` overrides
    /// this; the other variants (kprobe/tracepoint/raw-tp/uprobe wrappers)
    /// reject `mmap(perf_fd)`.
    fn device_mmap(&mut self, _len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
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

    fn device_mmap(&self, offset: u64, length: u64) -> AxResult<DeviceMmap> {
        // libbpf calls mmap with offset == 0; non-zero offsets address into
        // the ringbuf, which has no meaningful sub-region exposed as a fd
        // offset (data_offset lives inside the header page).
        if offset != 0 {
            return Err(AxError::InvalidInput);
        }
        let len = length as usize;
        let (paddr, anchor) = self.event.lock().device_mmap(len)?;
        // Anchor the ringbuf pages to the VMA: the retainer keeps them alive
        // until `munmap`/exit, so closing the perf fd can't free memory the
        // user address space still maps. See `BpfPerfEventWrapper::pages`.
        Ok(DeviceMmap::Physical(
            PhysAddrRange::from_start_size(paddr, len),
            Some(anchor),
        ))
    }
}

/// `perf_event_open(2)` syscall entry. Copies the user `perf_event_attr` in
/// and trampolines into [`perf_event_open`], which holds the dispatcher
/// across kprobe / tracepoint / software / uprobe types.
pub fn sys_perf_event_open(
    attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> AxResult<isize> {
    let mut buf = vec![0u8; core::mem::size_of::<perf_event_attr>()];
    VmBytes::new(attr_uptr as *mut u8, buf.len()).read(&mut buf)?;
    // SAFETY: perf_event_attr is a `repr(C)` POD; the user buffer is copied
    // bytewise above and we treat the result as the structure.
    let attr = unsafe { &*(buf.as_ptr() as *const perf_event_attr) };
    perf_event_open(attr, pid, cpu, group_fd, flags as u32)
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
            .into_ax_result()?;
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
    // Honour PERF_FLAG_FD_CLOEXEC: Linux opens the perf fd with O_CLOEXEC when
    // the caller sets this flag, otherwise the fd survives execve.
    let cloexec = flags & PERF_FLAG_FD_CLOEXEC != 0;
    let fd = add_file_like(event_arc.clone(), cloexec)?;

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

/// Executable kernel mapping used by rbpf JIT programs on x86_64.
#[allow(unused)]
struct BPFJitMemory {
    num_pages: usize,
    pages: VirtAddr,
}

#[allow(unused)]
impl BPFJitMemory {
    fn new(num_pages: usize) -> AxResult<Self> {
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        let virt_start = guard
            .find_free_area(
                guard.base(),
                num_pages * PAGE_SIZE_4K,
                VirtAddrRange::new(guard.base(), guard.end()),
            )
            .ok_or(AxError::NoMemory)?;
        guard.map_alloc(
            virt_start,
            num_pages * PAGE_SIZE_4K,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
            true,
        )?;

        Ok(BPFJitMemory {
            num_pages,
            pages: virt_start,
        })
    }

    /// Returns a `'static` mutable slice for rbpf's JIT memory registration.
    ///
    /// SAFETY: the caller must keep `self` alive and exclusively owned for at
    /// least as long as the returned slice may be used. The slice must not be
    /// used after this `BPFJitMemory` is dropped, because drop unmaps the
    /// backing pages.
    unsafe fn as_static_mut_slice(&mut self) -> &'static mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.pages.as_ptr() as *mut u8,
                self.num_pages * PAGE_SIZE_4K,
            )
        }
    }
}

impl Drop for BPFJitMemory {
    fn drop(&mut self) {
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        guard
            .unmap(self.pages, self.num_pages * PAGE_SIZE_4K)
            .expect("failed to unmap BPF JIT memory");
    }
}
