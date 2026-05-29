//! BPF performance event handling module.

use super::util::{PerfProbeArgs, *};
use crate::{BpfResult as Result, linux_bpf::*};

const PAGE_SIZE: usize = 4096;

/// Ring buffer page for perf events.
#[derive(Debug)]
pub struct RingPage {
    size: usize,
    ptr: usize,
    data_region_size: usize,
    lost: usize,
}

impl RingPage {
    /// Create an empty RingPage.
    pub fn empty() -> Self {
        RingPage {
            ptr: 0,
            size: 0,
            data_region_size: 0,
            lost: 0,
        }
    }

    /// Get the start address of the RingPage.
    pub fn start(&self) -> usize {
        self.ptr
    }

    /// Initialize a RingPage from start address and length.
    pub fn new_init(start: usize, len: usize) -> Self {
        Self::init(start as _, len)
    }

    fn init(ptr: *mut u8, size: usize) -> Self {
        assert_eq!(size % PAGE_SIZE, 0);
        assert!(size / PAGE_SIZE >= 2);
        // The first page will be filled with perf_event_mmap_page
        unsafe {
            let perf_event_mmap_page = &mut *(ptr as *mut perf_event_mmap_page);
            perf_event_mmap_page.data_offset = PAGE_SIZE as u64;
            perf_event_mmap_page.data_size = (size - PAGE_SIZE) as u64;
            // user will read sample or lost record from data_tail
            perf_event_mmap_page.data_tail = 0;
            // kernel will write sample or lost record from data_head
            perf_event_mmap_page.data_head = 0;
            // It is a ring buffer.
        }
        RingPage {
            ptr: ptr as usize,
            size,
            data_region_size: size - PAGE_SIZE,
            lost: 0,
        }
    }

    #[inline]
    fn can_write(&self, data_size: usize, data_tail: usize, data_head: usize) -> bool {
        let capacity = self.data_region_size - data_head + data_tail;
        data_size <= capacity
    }

    /// Write a perf event to the ring buffer.
    pub fn write_event(&mut self, data: &[u8]) -> Result<()> {
        let data_tail = unsafe { &mut (*(self.ptr as *mut perf_event_mmap_page)).data_tail };
        let data_head = unsafe { &mut (*(self.ptr as *mut perf_event_mmap_page)).data_head };

        // user lib will update the tail after read the data,but it will not % data_region_size
        let perf_header_size = size_of::<perf_event_header>();
        let can_write_perf_header =
            self.can_write(perf_header_size, *data_tail as usize, *data_head as usize);

        if can_write_perf_header {
            let can_write_lost_record = self.can_write(
                size_of::<LostSamples>(),
                *data_tail as usize,
                *data_head as usize,
            );
            // if there is lost record, we need to write the lost record first
            if self.lost > 0 && can_write_lost_record {
                let new_data_head = self.write_lost(*data_head as usize)?;
                *data_head = new_data_head as u64;
                self.lost = 0;
                // try to write the event again
                return self.write_event(data);
            }
            let sample_size = PerfSample::calculate_size(data.len());
            let can_write_sample =
                self.can_write(sample_size, *data_tail as usize, *data_head as usize);
            if can_write_sample {
                let new_data_head = self.write_sample(data, *data_head as usize)?;
                *data_head = new_data_head as u64;
            } else {
                self.lost += 1;
            }
        } else {
            self.lost += 1;
        }
        Ok(())
    }

    /// Write any data to the page.
    ///
    /// Return the new data_head
    fn write_any(&mut self, data: &[u8], data_head: usize) -> Result<()> {
        let data_region_len = self.data_region_size;
        let data_region = self.as_mut_slice()[PAGE_SIZE..].as_mut();
        let data_len = data.len();
        let start = data_head % data_region_len;
        let end = (data_head + data_len) % data_region_len;
        if start < end {
            data_region[start..end].copy_from_slice(data);
        } else {
            let first_len = data_region_len - start;
            data_region[start..start + first_len].copy_from_slice(&data[..first_len]);
            data_region[0..end].copy_from_slice(&data[first_len..]);
        }
        Ok(())
    }
    #[inline]
    fn fill_size(&self, data_head_mod: usize) -> usize {
        if self.data_region_size - data_head_mod < size_of::<perf_event_header>() {
            // The remaining space is not enough to write the perf_event_header
            // We need to fill the remaining space with 0
            self.data_region_size - data_head_mod
        } else {
            0
        }
    }

    /// Write a sample to the page.
    fn write_sample(&mut self, data: &[u8], data_head: usize) -> Result<usize> {
        let sample_size = PerfSample::calculate_size(data.len());
        let maybe_end = (data_head + sample_size) % self.data_region_size;
        let fill_size = self.fill_size(maybe_end);
        let perf_sample = PerfSample {
            s_hdr: SampleHeader {
                header: perf_event_header {
                    type_: perf_event_type::PERF_RECORD_SAMPLE as u32,
                    misc: 0,
                    size: size_of::<SampleHeader>() as u16 + data.len() as u16 + fill_size as u16,
                },
                size: data.len() as u32,
            },
            value: data,
        };
        self.write_any(perf_sample.s_hdr.as_bytes(), data_head)?;
        self.write_any(perf_sample.value, data_head + size_of::<SampleHeader>())?;
        Ok(data_head + sample_size + fill_size)
    }

    /// Write a lost record to the page.
    ///
    /// Return the new data_head
    fn write_lost(&mut self, data_head: usize) -> Result<usize> {
        let maybe_end = (data_head + size_of::<LostSamples>()) % self.data_region_size;
        let fill_size = self.fill_size(maybe_end);
        let lost = LostSamples {
            header: perf_event_header {
                type_: perf_event_type::PERF_RECORD_LOST as u32,
                misc: 0,
                size: size_of::<LostSamples>() as u16 + fill_size as u16,
            },
            id: 0,
            count: self.lost as u64,
        };
        self.write_any(lost.as_bytes(), data_head)?;
        Ok(data_head + size_of::<LostSamples>() + fill_size)
    }

    /// Whether the ring buffer is readable.
    pub fn readable(&self) -> bool {
        let data_tail = unsafe { &(*(self.ptr as *mut perf_event_mmap_page)).data_tail };
        let data_head = unsafe { &(*(self.ptr as *mut perf_event_mmap_page)).data_head };
        data_tail != data_head
    }

    /// Get the ring buffer as a slice.
    #[allow(dead_code)]
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr as *const u8, self.size) }
    }

    /// Get the ring buffer as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr as *mut u8, self.size) }
    }
}

/// BPF performance event structure.
#[derive(Debug)]
pub struct BpfPerfEvent {
    _args: PerfProbeArgs,
    data: BpfPerfEventData,
}

/// Data for BPF performance event.
#[derive(Debug)]
pub struct BpfPerfEventData {
    enabled: bool,
    mmap_page: RingPage,
    offset: usize,
}

impl BpfPerfEvent {
    /// Create a new BpfPerfEvent.
    pub fn new(args: PerfProbeArgs) -> Self {
        BpfPerfEvent {
            _args: args,
            data: BpfPerfEventData {
                enabled: false,
                mmap_page: RingPage::empty(),
                offset: 0,
            },
        }
    }

    /// Bind the perf event to a mmap page.
    pub fn do_mmap(&mut self, start: usize, len: usize, offset: usize) -> Result<()> {
        // create mmap page
        let mmap_page = RingPage::new_init(start, len);
        self.data.mmap_page = mmap_page;
        self.data.offset = offset;
        Ok(())
    }

    /// Write a perf event to the mmap page.
    /// Only when the perf event is enabled, the event will be written.
    pub fn write_event(&mut self, data: &[u8]) -> Result<()> {
        if self.data.enabled {
            self.data.mmap_page.write_event(data)?;
        }
        Ok(())
    }

    /// Enable the perf event
    pub fn enable(&mut self) -> Result<()> {
        self.data.enabled = true;
        Ok(())
    }

    /// Disable the perf event
    pub fn disable(&mut self) -> Result<()> {
        self.data.enabled = false;
        Ok(())
    }

    /// Whether the perf event is enabled
    pub fn enabled(&self) -> bool {
        self.data.enabled
    }

    /// Whether the perf event is readable
    pub fn readable(&self) -> bool {
        self.data.mmap_page.readable()
    }

    /// Whether the perf event is writable
    pub fn writeable(&self) -> bool {
        false
    }
}
