//! BPF program file-like wrapper. Ported from
//! `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/bpf/prog/mod.rs`); adapted
//! for tgoskits naming.

use alloc::{borrow::Cow, sync::Arc};
use core::fmt::Debug;

use ax_errno::{AxError, AxResult};
use axpoll::Pollable;
use kbpf_basic::{preprocessor::EbpfPreProcessor, prog::BpfProgMeta};

use crate::{
    ebpf::{map::BpfMap, transform::EbpfKernelAuxiliary},
    file::FileLike,
};

/// File-like handle for a loaded BPF program. Owns the (preprocessed) byte
/// stream of instructions, and on drop releases any per-map `Arc`s the
/// preprocessor stashed as raw pointers in operand fields.
pub struct BpfProg {
    _meta: BpfProgMeta,
    preprocessor: EbpfPreProcessor,
}

impl Debug for BpfProg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BpfProg").finish()
    }
}

impl BpfProg {
    /// Construct a `BpfProg` wrapping an already-preprocessed instruction
    /// stream.
    pub fn new(meta: BpfProgMeta, preprocessor: EbpfPreProcessor) -> Self {
        Self {
            _meta: meta,
            preprocessor,
        }
    }

    /// View the program's instructions (post-relocation), suitable for
    /// passing to `rbpf::EbpfVmRaw::new(Some(insns))`.
    pub fn insns(&self) -> &[u8] {
        self.preprocessor.get_new_insn()
    }
}

impl Drop for BpfProg {
    fn drop(&mut self) {
        // The preprocessor stashed `Arc<BpfMap>::into_raw` pointers in each
        // map-fd relocated operand so they outlive the load; reconstruct and
        // drop those Arcs here to release the map references.
        unsafe {
            for ptr in self.preprocessor.get_raw_file_ptr() {
                let file = Arc::from_raw(*ptr as *const u8 as *const BpfMap);
                drop(file);
            }
        }
    }
}

impl Pollable for BpfProg {
    fn poll(&self) -> axpoll::IoEvents {
        axpoll::IoEvents::empty()
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: axpoll::IoEvents) {
        // No poll semantics on bpf prog fds.
    }
}

impl FileLike for BpfProg {
    fn read(&self, _dst: &mut crate::file::IoDst) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn write(&self, _src: &mut crate::file::IoSrc) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn stat(&self) -> AxResult<crate::file::Kstat> {
        Ok(crate::file::Kstat::default())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[bpf_prog]".into()
    }
}

/// Verifier + preprocessor entry point. Loads a `BpfProgMeta` produced by
/// `BpfProgMeta::try_from_bpf_attr` and yields a ready-to-attach
/// [`BpfProg`].
pub fn load_prog(meta: &mut BpfProgMeta) -> kbpf_basic::BpfResult<BpfProg> {
    let insns = meta.take_insns().ok_or(kbpf_basic::BpfError::EINVAL)?;
    let preprocessor = EbpfPreProcessor::preprocess::<EbpfKernelAuxiliary>(insns)?;
    Ok(BpfProg::new(
        BpfProgMeta {
            prog_flags: meta.prog_flags,
            prog_type: meta.prog_type,
            expected_attach_type: meta.expected_attach_type,
            insns: None,
            license: core::mem::take(&mut meta.license),
            kern_version: meta.kern_version,
            name: core::mem::take(&mut meta.name),
        },
        preprocessor,
    ))
}
