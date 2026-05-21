use alloc::vec::Vec;

use ax_errno::{AxError, AxResult};
use ax_sync::spin::SpinNoIrq;

const PAGE_SIZE: usize = 4096;
const PERF_RECORD_SAMPLE: u32 = 9;
const PERF_RECORD_LOST: u32 = 2;

#[allow(dead_code)]
const PERF_TYPE_HARDWARE: u32 = 0;
const PERF_TYPE_SOFTWARE: u32 = 1;
const PERF_TYPE_TRACEPOINT: u32 = 2;
const PERF_TYPE_RAW: u32 = 4;
const PERF_TYPE_KPROBE: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct PerfEventHeader {
    type_: u32,
    misc: u16,
    size: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct SampleHeader {
    header: PerfEventHeader,
    size: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct LostSamples {
    header: PerfEventHeader,
    id: u64,
    count: u64,
}

#[repr(C)]
struct PerfEventMmapPage {
    version: u32,
    compat_version: u32,
    lock: u32,
    index: u32,
    offset: i64,
    time_enabled: u64,
    time_running: u64,
    capabilities: u64,
    pmc_width: u16,
    time_shift: u16,
    time_mult: u32,
    time_offset: u64,
    time_zero: u64,
    size: u32,
    _reserved_1: u32,
    time_cycles: u64,
    time_mask: u64,
    _reserved: [u8; 928],
    data_head: u64,
    data_tail: u64,
    data_offset: u64,
    data_size: u64,
    aux_head: u64,
    aux_tail: u64,
    aux_offset: u64,
    aux_size: u64,
}

const MMAP_PAGE_SIZE: usize = core::mem::size_of::<PerfEventMmapPage>();

struct RingBuffer {
    pages: Vec<u8>,
    data_region_size: usize,
    lost: u64,
}

impl RingBuffer {
    fn new(page_count: usize) -> AxResult<Self> {
        if page_count < 2 {
            return Err(AxError::InvalidInput);
        }
        let total_size = page_count * PAGE_SIZE;
        let data_region_size = total_size - PAGE_SIZE;
        let mut pages = alloc::vec![0u8; total_size];
        let mmap_page = unsafe { &mut *(pages.as_mut_ptr() as *mut PerfEventMmapPage) };
        mmap_page.version = 1;
        mmap_page.compat_version = 1;
        mmap_page.size = MMAP_PAGE_SIZE as u32;
        mmap_page.data_offset = PAGE_SIZE as u64;
        mmap_page.data_size = data_region_size as u64;
        mmap_page.data_head = 0;
        mmap_page.data_tail = 0;
        Ok(Self {
            pages,
            data_region_size,
            lost: 0,
        })
    }

    fn data_head(&self) -> u64 {
        let mmap_page = unsafe { &*(self.pages.as_ptr() as *const PerfEventMmapPage) };
        mmap_page.data_head
    }

    fn set_data_head(&mut self, val: u64) {
        let mmap_page = unsafe { &mut *(self.pages.as_mut_ptr() as *mut PerfEventMmapPage) };
        mmap_page.data_head = val;
    }

    fn data_tail(&self) -> u64 {
        let mmap_page = unsafe { &*(self.pages.as_ptr() as *const PerfEventMmapPage) };
        mmap_page.data_tail
    }

    fn can_write(&self, needed: usize, tail: u64, head: u64) -> bool {
        let capacity = self.data_region_size as u64;
        (capacity - (head - tail)) as usize >= needed
    }

    fn write_bytes(&mut self, data: &[u8], offset_in_data_region: usize) {
        if data.is_empty() {
            return;
        }
        let start = offset_in_data_region % self.data_region_size;
        let data_ptr = unsafe { self.pages.as_mut_ptr().add(PAGE_SIZE) };
        if start + data.len() <= self.data_region_size {
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), data_ptr.add(start), data.len());
            }
        } else {
            let first_len = self.data_region_size - start;
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), data_ptr.add(start), first_len);
                core::ptr::copy_nonoverlapping(
                    data.as_ptr().add(first_len),
                    data_ptr,
                    data.len() - first_len,
                );
            }
        }
    }

    fn fill_size(&self, head_mod: usize) -> usize {
        let remaining = self.data_region_size - head_mod;
        if remaining > 0 && remaining < core::mem::size_of::<PerfEventHeader>() {
            remaining
        } else {
            0
        }
    }

    fn write_sample(&mut self, data: &[u8], head: u64) -> AxResult<u64> {
        let head_mod = (head as usize) % self.data_region_size;
        let fill = self.fill_size(head_mod);
        let total_size = core::mem::size_of::<SampleHeader>() + data.len() + fill;
        let hdr = SampleHeader {
            header: PerfEventHeader {
                type_: PERF_RECORD_SAMPLE,
                misc: 0,
                size: total_size as u16,
            },
            size: data.len() as u32,
        };
        let hdr_bytes = unsafe {
            core::slice::from_raw_parts(
                &hdr as *const SampleHeader as *const u8,
                core::mem::size_of::<SampleHeader>(),
            )
        };
        self.write_bytes(hdr_bytes, head_mod);
        let data_offset = (head_mod + core::mem::size_of::<SampleHeader>()) % self.data_region_size;
        self.write_bytes(data, data_offset);
        if fill > 0 {
            let fill_offset = (head_mod + total_size - fill) % self.data_region_size;
            let zeros = alloc::vec![0u8; fill];
            self.write_bytes(&zeros, fill_offset);
        }
        Ok(head + total_size as u64)
    }

    fn write_lost(&mut self, head: u64, count: u64) -> AxResult<u64> {
        let head_mod = (head as usize) % self.data_region_size;
        let lost = LostSamples {
            header: PerfEventHeader {
                type_: PERF_RECORD_LOST,
                misc: 0,
                size: core::mem::size_of::<LostSamples>() as u16,
            },
            id: 0,
            count,
        };
        let lost_bytes = unsafe {
            core::slice::from_raw_parts(
                &lost as *const LostSamples as *const u8,
                core::mem::size_of::<LostSamples>(),
            )
        };
        self.write_bytes(lost_bytes, head_mod);
        Ok(head + core::mem::size_of::<LostSamples>() as u64)
    }

    fn write_event(&mut self, data: &[u8]) -> AxResult<()> {
        let tail = self.data_tail();
        let mut head = self.data_head();
        let hdr_size = core::mem::size_of::<PerfEventHeader>();
        if !self.can_write(hdr_size, tail, head) {
            self.lost += 1;
            return Ok(());
        }
        if self.lost > 0 {
            let lost_size = core::mem::size_of::<LostSamples>();
            if self.can_write(lost_size, tail, head) {
                head = self.write_lost(head, self.lost)?;
                self.lost = 0;
            }
        }
        let sample_size = core::mem::size_of::<SampleHeader>() + data.len();
        let head_mod = (head as usize) % self.data_region_size;
        let fill = self.fill_size(head_mod);
        let total = sample_size + fill;
        if self.can_write(total, tail, head) {
            head = self.write_sample(data, head)?;
        } else {
            self.lost += 1;
        }
        self.set_data_head(head);
        Ok(())
    }

    #[allow(dead_code)]
    fn readable(&self) -> bool {
        self.data_head() != self.data_tail()
    }
}

struct PerfEvent {
    ring: RingBuffer,
    enabled: bool,
    prog_fd: Option<u32>,
}

impl PerfEvent {
    fn new(page_count: usize) -> AxResult<Self> {
        Ok(Self {
            ring: RingBuffer::new(page_count)?,
            enabled: false,
            prog_fd: None,
        })
    }

    fn enable(&mut self) {
        self.enabled = true;
    }

    fn disable(&mut self) {
        self.enabled = false;
    }

    fn write_event(&mut self, data: &[u8]) -> AxResult<()> {
        if !self.enabled {
            return Ok(());
        }
        self.ring.write_event(data)
    }

    fn attach_prog(&mut self, prog_fd: u32) {
        self.prog_fd = Some(prog_fd);
    }

    #[allow(dead_code)]
    fn attached_prog(&self) -> Option<u32> {
        self.prog_fd
    }
}

struct PerfEventEntry {
    event: PerfEvent,
    fd: u32,
    #[allow(dead_code)]
    event_type: u32,
    #[allow(dead_code)]
    config: u64,
    #[allow(dead_code)]
    pid: i32,
    #[allow(dead_code)]
    cpu: i32,
}

static PERF_EVENTS: SpinNoIrq<Vec<PerfEventEntry>> = SpinNoIrq::new(Vec::new());

fn alloc_perf_fd() -> u32 {
    let guard = PERF_EVENTS.lock();
    (guard.len() as u32) + 100
}

pub fn sys_perf_event_open_impl(
    attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> AxResult<isize> {
    let (event_type, config) = unsafe {
        if attr_uptr == 0 {
            return Err(AxError::InvalidInput);
        }
        let ptr = attr_uptr as *const u32;
        let event_type = core::ptr::read(ptr);
        let size = core::ptr::read(ptr.add(1));
        if size < 16 {
            return Err(AxError::InvalidInput);
        }
        let config = core::ptr::read((ptr as *const u64).add(1));
        (event_type, config)
    };
    match event_type {
        PERF_TYPE_KPROBE | PERF_TYPE_TRACEPOINT | PERF_TYPE_SOFTWARE | PERF_TYPE_RAW => {}
        _ => {
            warn!("perf_event_open: unsupported type {event_type}");
            return Err(AxError::Unsupported);
        }
    }
    let page_count = 2 + 1;
    let event = PerfEvent::new(page_count)?;
    let fd = alloc_perf_fd();
    let entry = PerfEventEntry {
        event,
        fd,
        event_type,
        config,
        pid,
        cpu,
    };
    if group_fd >= 0 {
        let _ = group_fd;
    }
    let _ = flags;
    PERF_EVENTS.lock().push(entry);
    info!("perf_event_open: type={event_type} config={config:#x} pid={pid} cpu={cpu} fd={fd}");
    Ok(fd as isize)
}

pub fn perf_event_write(fd: u32, data: &[u8]) -> AxResult<()> {
    let mut guard = PERF_EVENTS.lock();
    for entry in guard.iter_mut() {
        if entry.fd == fd {
            return entry.event.write_event(data);
        }
    }
    Err(AxError::BadFileDescriptor)
}

pub fn perf_event_enable(fd: u32) -> AxResult<()> {
    let mut guard = PERF_EVENTS.lock();
    for entry in guard.iter_mut() {
        if entry.fd == fd {
            entry.event.enable();
            return Ok(());
        }
    }
    Err(AxError::BadFileDescriptor)
}

pub fn perf_event_disable(fd: u32) -> AxResult<()> {
    let mut guard = PERF_EVENTS.lock();
    for entry in guard.iter_mut() {
        if entry.fd == fd {
            entry.event.disable();
            return Ok(());
        }
    }
    Err(AxError::BadFileDescriptor)
}

pub fn perf_event_attach_prog(fd: u32, prog_fd: u32) -> AxResult<()> {
    let mut guard = PERF_EVENTS.lock();
    for entry in guard.iter_mut() {
        if entry.fd == fd {
            entry.event.attach_prog(prog_fd);
            return Ok(());
        }
    }
    Err(AxError::BadFileDescriptor)
}

#[allow(dead_code)]
pub fn perf_event_get_prog_fd(fd: u32) -> AxResult<Option<u32>> {
    let guard = PERF_EVENTS.lock();
    for entry in guard.iter() {
        if entry.fd == fd {
            return Ok(entry.event.attached_prog());
        }
    }
    Err(AxError::BadFileDescriptor)
}
