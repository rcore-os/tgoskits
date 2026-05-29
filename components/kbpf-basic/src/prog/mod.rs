//! Metadata and verifier info for BPF programs.
use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{ffi::CStr, fmt::Debug};

use crate::{
    BpfError, BpfResult as Result, KernelAuxiliaryOps,
    linux_bpf::{bpf_attach_type, bpf_attr, bpf_prog_type},
};

/// Metadata for a BPF program.
pub struct BpfProgMeta {
    /// Program flags.
    pub prog_flags: u32,
    /// Program type.
    pub prog_type: bpf_prog_type,
    /// Expected attach type.
    pub expected_attach_type: bpf_attach_type,
    /// eBPF instructions.
    pub insns: Option<Vec<u8>>,
    /// License string.
    pub license: String,
    /// Kernel version.
    pub kern_version: u32,
    /// Program name.
    pub name: String,
}

impl Debug for BpfProgMeta {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let insn_len = if let Some(insns) = &self.insns {
            insns.len() / 8
        } else {
            0
        };
        f.debug_struct("BpfProgMeta")
            .field("prog_flags", &self.prog_flags)
            .field("prog_type", &self.prog_type)
            .field("expected_attach_type", &self.expected_attach_type)
            .field("insns_len", &insn_len)
            .field("license", &self.license)
            .field("kern_version", &self.kern_version)
            .field("name", &self.name)
            .finish()
    }
}

impl BpfProgMeta {
    /// Take the instructions out of the metadata.
    pub fn take_insns(&mut self) -> Option<Vec<u8>> {
        self.insns.take()
    }

    /// Try to create a `BpfProgMeta` from a `bpf_attr` structure.
    pub fn try_from_bpf_attr<F: KernelAuxiliaryOps>(attr: &bpf_attr) -> Result<Self> {
        let u = unsafe { &attr.__bindgen_anon_3 };
        let prog_type = bpf_prog_type::try_from(u.prog_type).map_err(|_| BpfError::EINVAL)?;
        let expected_attach_type =
            bpf_attach_type::try_from(u.expected_attach_type).map_err(|_| BpfError::EINVAL)?;
        let name_slice = unsafe {
            core::slice::from_raw_parts(u.prog_name.as_ptr() as *const u8, u.prog_name.len())
        };
        let prog_name = CStr::from_bytes_until_nul(name_slice)
            .map_err(|_| BpfError::EINVAL)?
            .to_str()
            .map_err(|_| BpfError::EINVAL)?
            .to_string();
        let license = if u.license != 0 {
            F::string_from_user_cstr(u.license as *const u8)?
        } else {
            String::new()
        };

        let insns_buf = if u.insns == 0 {
            assert_eq!(u.insn_cnt, 0);
            Vec::new()
        } else {
            let mut insns_buf = vec![0u8; u.insn_cnt as usize * 8];
            F::copy_from_user(
                u.insns as *const u8,
                u.insn_cnt as usize * 8,
                &mut insns_buf,
            )?;
            insns_buf
        };
        Ok(Self {
            prog_flags: u.prog_flags,
            prog_type,
            expected_attach_type,
            insns: Some(insns_buf),
            license,
            kern_version: u.kern_version,
            name: prog_name,
        })
    }
}

bitflags::bitflags! {

    /// The log level for BPF program verifier.
    #[derive(Debug, Clone, Copy)]
    pub struct VerifierLogLevel: u32 {
        /// Sets no verifier logging.
        const DISABLE = 0;
        /// Enables debug verifier logging.
        const DEBUG = 1;
        /// Enables verbose verifier logging.
        const VERBOSE = 2 | Self::DEBUG.bits();
        /// Enables verifier stats.
        const STATS = 4;
    }
}

/// BPF program verifier information.
#[derive(Debug)]
pub struct BpfProgVerifierInfo {
    /// This attribute specifies the level/detail of the log output. Valid values are.
    pub log_level: VerifierLogLevel,
    /// This attributes indicates the size of the memory region in bytes
    /// indicated by `log_buf` which can safely be written to by the kernel.
    pub _log_buf_size: u32,
    /// This attributes can be set to a pointer to a memory region
    /// allocated/reservedby the loader process where the verifier log will
    /// be written to.
    /// The detail of the log is set by log_level. The verifier log
    /// is often the only indication in addition to the error code of
    /// why the syscall command failed to load the program.
    ///
    /// The log is also written to on success. If the kernel runs out of
    /// space in the buffer while loading, the loading process will fail
    /// and the command will return with an error code of -ENOSPC. So it
    /// is important to correctly size the buffer when enabling logging.
    pub _log_buf_ptr: usize,
}

impl From<&bpf_attr> for BpfProgVerifierInfo {
    fn from(attr: &bpf_attr) -> Self {
        unsafe {
            let u = &attr.__bindgen_anon_3;
            Self {
                log_level: VerifierLogLevel::from_bits_truncate(u.log_level),
                _log_buf_size: u.log_size,
                _log_buf_ptr: u.log_buf as usize,
            }
        }
    }
}
