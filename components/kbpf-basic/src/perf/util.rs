use alloc::string::String;

use bitflags::bitflags;
use int_enum::IntEnum;

use crate::{BpfError, BpfResult as Result, KernelAuxiliaryOps, linux_bpf::*};

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct PerfEventOpenFlags: u32 {
        const PERF_FLAG_FD_NO_GROUP = 1;
        const PERF_FLAG_FD_OUTPUT = 2;
        const PERF_FLAG_PID_CGROUP = 4;
        const PERF_FLAG_FD_CLOEXEC = 8;
    }
}

/// The `PerfEventIoc` enum is used to define the ioctl commands for perf events.
///
/// See https://elixir.bootlin.com/linux/v6.1/source/include/uapi/linux/perf_event.h#L544
#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, IntEnum)]
pub enum PerfEventIoc {
    /// Equivalent to [crate::linux_bpf::AYA_PERF_EVENT_IOC_ENABLE].
    Enable  = 9216,
    /// Equivalent to [crate::linux_bpf::AYA_PERF_EVENT_IOC_DISABLE].
    Disable = 9217,
    /// Equivalent to [crate::linux_bpf::AYA_PERF_EVENT_IOC_SET_BPF].
    SetBpf  = 1074013192,
}

#[allow(non_camel_case_types, missing_docs)]
#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, IntEnum)]
pub enum PerfTypeId {
    PERF_TYPE_HARDWARE   = 0,
    PERF_TYPE_SOFTWARE   = 1,
    PERF_TYPE_TRACEPOINT = 2,
    PERF_TYPE_HW_CACHE   = 3,
    PERF_TYPE_RAW        = 4,
    PERF_TYPE_BREAKPOINT = 5,
    // TODO: Make sure these are correct.
    PERF_TYPE_KPROBE     = 6,
    PERF_TYPE_UPROBE     = 7,
}

#[derive(Debug, Clone)]
#[allow(unused)]
/// `perf_event_open` syscall arguments.
pub struct PerfProbeArgs {
    /// Configuration for the perf probe.
    pub config: PerfProbeConfig,
    /// Name of the perf probe.
    pub name: String,
    /// Offset for the perf probe.
    pub offset: u64,
    /// Size of the perf probe.
    pub size: u32,
    /// Type of the perf probe.
    pub type_: PerfTypeId,
    /// PID for the perf probe.
    pub pid: i32,
    /// CPU for the perf probe.
    pub cpu: i32,
    /// Group file descriptor for the perf probe.
    pub group_fd: i32,
    /// Flags for the perf probe.
    pub flags: PerfEventOpenFlags,
    /// Sample type for the perf probe.
    pub sample_type: Option<perf_event_sample_format>,
}

/// Configuration for the perf probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfProbeConfig {
    /// Perf software IDs.
    PerfSwIds(perf_sw_ids),
    /// Perf raw config.
    Raw(u64),
}

impl PerfProbeArgs {
    /// Try to create a `PerfProbeArgs` from a `perf_event_attr` structure.
    pub fn try_from_perf_attr<F: KernelAuxiliaryOps>(
        attr: &perf_event_attr,
        pid: i32,
        cpu: i32,
        group_fd: i32,
        flags: u32,
    ) -> Result<Self> {
        let ty = PerfTypeId::try_from(attr.type_).map_err(|_| BpfError::EINVAL)?;
        let config = match ty {
            PerfTypeId::PERF_TYPE_KPROBE
            | PerfTypeId::PERF_TYPE_UPROBE
            | PerfTypeId::PERF_TYPE_TRACEPOINT => PerfProbeConfig::Raw(attr.config),
            _ => {
                let sw_id =
                    perf_sw_ids::try_from(attr.config as u32).map_err(|_| BpfError::EINVAL)?;
                PerfProbeConfig::PerfSwIds(sw_id)
            }
        };

        let name = if ty == PerfTypeId::PERF_TYPE_KPROBE || ty == PerfTypeId::PERF_TYPE_UPROBE {
            let name_ptr = unsafe { attr.__bindgen_anon_3.config1 } as *const u8;

            F::string_from_user_cstr(name_ptr)?
        } else {
            String::new()
        };
        let sample_ty = perf_event_sample_format::try_from(attr.sample_type as u32).ok();
        let args = PerfProbeArgs {
            config,
            name,
            offset: unsafe { attr.__bindgen_anon_4.config2 },
            size: attr.size,
            type_: ty,
            pid,
            cpu,
            group_fd,
            flags: PerfEventOpenFlags::from_bits_truncate(flags),
            sample_type: sample_ty,
        };
        Ok(args)
    }
}

/// The event type in our particular use case will be `PERF_RECORD_SAMPLE` or `PERF_RECORD_LOST`.
/// `PERF_RECORD_SAMPLE` indicating that there is an actual sample after this header.
/// And `PERF_RECORD_LOST` indicating that there is a record lost header following the perf event header.
#[repr(C)]
#[derive(Debug)]
pub struct LostSamples {
    pub header: perf_event_header,
    pub id: u64,
    pub count: u64,
}

impl LostSamples {
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self as *const Self as *const u8, size_of::<Self>()) }
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct SampleHeader {
    pub header: perf_event_header,
    pub size: u32,
}

impl SampleHeader {
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self as *const Self as *const u8, size_of::<Self>()) }
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct PerfSample<'a> {
    pub s_hdr: SampleHeader,
    pub value: &'a [u8],
}

impl PerfSample<'_> {
    pub fn calculate_size(value_size: usize) -> usize {
        size_of::<SampleHeader>() + value_size
    }
}
