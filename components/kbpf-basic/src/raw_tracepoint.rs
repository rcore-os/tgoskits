//! Arguments for attaching BPF raw tracepoint programs.
use alloc::string::String;

use crate::{BpfResult as Result, KernelAuxiliaryOps, linux_bpf::*};

/// Arguments for attaching a BPF raw tracepoint program.
#[derive(Debug)]
pub struct BpfRawTracePointArg {
    /// Name of the raw tracepoint.
    pub name: String,
    /// File descriptor of the BPF program.
    pub prog_fd: u32,
}

impl BpfRawTracePointArg {
    /// Try to create a `BpfRawTracePointArg` from a `bpf_attr` structure.
    pub fn try_from_bpf_attr<F: KernelAuxiliaryOps>(attr: &bpf_attr) -> Result<Self> {
        let (name_ptr, prog_fd) = unsafe {
            let name_ptr = attr.raw_tracepoint.name as *const u8;

            let prog_fd = attr.raw_tracepoint.prog_fd;
            (name_ptr, prog_fd)
        };
        let name = F::string_from_user_cstr(name_ptr)?;
        Ok(BpfRawTracePointArg { name, prog_fd })
    }
}
