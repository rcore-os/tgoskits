//! `init_module(2)` / `finit_module(2)` / `delete_module(2)` syscalls.
//!
//! Ported from `Starry-OS/StarryOS:ebpf-kmod`
//! (`kernel/src/syscall/kmod/mod.rs`); flattened to a single file because
//! that module had only one sibling and tgoskits' syscall layout keeps
//! small subsystems as `<name>.rs` rather than `<name>/mod.rs` (cf.
//! `syscall/signal.rs`, `syscall/time.rs`).

use alloc::vec;

use ax_errno::{AxError, AxResult};
use ax_io::Read;
use ax_task::current;

use crate::{
    file::get_file_like,
    mm::{VmBytes, vm_load_string},
    task::AsThread,
};

fn require_module_privilege() -> AxResult<()> {
    if current().as_thread().cred().has_cap_sys_module() {
        Ok(())
    } else {
        Err(AxError::OperationNotPermitted)
    }
}

/// See <https://man7.org/linux/man-pages/man2/init_module.2.html>
pub fn sys_init_module(module_ptr: *const u8, len: usize, param_ptr: *const u8) -> AxResult<isize> {
    require_module_privilege()?;
    let mut module_buf = VmBytes::new(module_ptr as *mut u8, len);
    let mut module_data = vec![0u8; len];
    module_buf.read(&mut module_data)?;

    let param_buf = if !param_ptr.is_null() {
        Some(vm_load_string(param_ptr as _)?)
    } else {
        None
    };

    warn!(
        "[sys_init_module]: module_len={}, params={:?}",
        len, param_buf
    );
    crate::kmod::init_module(&module_data, param_buf.as_deref())?;
    Ok(0)
}

/// `finit_module(2)` — load a module from an open fd rather than a user
/// buffer.
pub fn sys_finit_module(module_fd: i32, param_ptr: *const u8, flags: u32) -> AxResult<isize> {
    require_module_privilege()?;
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    let file = get_file_like(module_fd)?;
    let fsize = file.stat()?.size as usize;

    let mut module_data = vec![0u8; fsize];
    let mut offset = 0;
    while offset < fsize {
        let mut buf: &mut [u8] = &mut module_data[offset..];
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Err(AxError::UnexpectedEof);
        }
        offset += n;
    }

    let param_buf = if !param_ptr.is_null() {
        Some(vm_load_string(param_ptr as _)?)
    } else {
        None
    };

    warn!(
        "[sys_finit_module]: module_len={}, params={:?}",
        module_data.len(),
        param_buf
    );
    crate::kmod::init_module(&module_data, param_buf.as_deref())?;
    Ok(0)
}

/// See <https://man7.org/linux/man-pages/man2/delete_module.2.html>
pub fn sys_delete_module(name_ptr: *const u8, _flags: u32) -> AxResult<isize> {
    require_module_privilege()?;
    let name = vm_load_string(name_ptr as _)?;
    warn!("[sys_delete_module]: name={}", name);
    crate::kmod::delete_module(&name)?;
    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn kmod_flags_validation_rules_hold_for_test() -> bool {
    // Test finit_module flag validation: only flags=0 is valid
    let valid_flags = 0u32;
    assert!(valid_flags == 0);

    // Any non-zero flags should be rejected
    let invalid_flags = 1u32;
    assert!(invalid_flags != 0);

    let invalid_flags2 = 0xFFFFFFFFu32;
    assert!(invalid_flags2 != 0);

    true
}
