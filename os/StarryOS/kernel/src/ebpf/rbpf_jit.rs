use alloc::collections::BTreeMap;
use core::alloc::Layout;

const PAGE_SIZE: usize = 4096;
const NUM_PAGES: usize = 1;

pub struct RbpfJitBuffer {
    memory: *mut u8,
    layout: Layout,
    pub func: unsafe fn(*mut u8, usize, *mut u8, usize, usize, usize) -> u64,
}

// SAFETY: `RbpfJitBuffer` owns a heap-allocated memory region containing generated JIT code.
// The JIT code acts as a pure function and is never mutated after compilation.
// Therefore, it is safe to transfer ownership across threads (Send) and to share references across threads (Sync).
unsafe impl Send for RbpfJitBuffer {}
// SAFETY: See above.
unsafe impl Sync for RbpfJitBuffer {}

impl Drop for RbpfJitBuffer {
    fn drop(&mut self) {
        unsafe {
            alloc::alloc::dealloc(self.memory, self.layout);
        }
    }
}

impl core::fmt::Debug for RbpfJitBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RbpfJitBuffer({:p})", self.memory)
    }
}

pub fn try_jit_compile(
    insns: &[crate::ebpf::bpf_insn::BpfInsn],
    helpers: &BTreeMap<u32, rbpf::ebpf::Helper>,
) -> Option<RbpfJitBuffer> {
    let size = NUM_PAGES * PAGE_SIZE;
    let layout = Layout::from_size_align(size, PAGE_SIZE).ok()?;
    let memory = unsafe { alloc::alloc::alloc(layout) };
    if memory.is_null() {
        return None;
    }

    // We need to make the memory executable. Since it's allocated from the kernel heap,
    // we use `ax_mm::kernel_aspace().lock().protect()` to set RWX permissions.
    let start_addr = ax_memory_addr::VirtAddr::from(memory as usize);
    let flags = ax_page_table_entry::MappingFlags::READ
        | ax_page_table_entry::MappingFlags::WRITE
        | ax_page_table_entry::MappingFlags::EXECUTE;
    if let Err(e) = ax_mm::kernel_aspace()
        .lock()
        .protect(start_addr, size, flags)
    {
        ax_log::error!("Failed to set JIT memory to RWX: {:?}", e);
        unsafe { alloc::alloc::dealloc(memory, layout) };
        return None;
    }

    let exec_slice = unsafe { core::slice::from_raw_parts_mut(memory, size) };

    // Convert BpfInsn to u8 slice
    let prog_bytes = unsafe {
        core::slice::from_raw_parts(insns.as_ptr() as *const u8, core::mem::size_of_val(insns))
    };

    // rbpf helpers use hashbrown::HashMap, but wait, BTreeMap is not HashMap!
    // rbpf::ebpf::Helper is `fn(u64, u64, u64, u64, u64) -> u64`.
    // Wait, the helpers we pass in `try_jit_compile` are `BTreeMap<u32, ...>`.
    // We need to convert it to hashbrown::HashMap.
    let mut rbpf_helpers = hashbrown::HashMap::new();
    for (&k, &v) in helpers.iter() {
        rbpf_helpers.insert(k, v);
    }

    match rbpf::jit::JitMemory::new(prog_bytes, exec_slice, &rbpf_helpers, false, false) {
        Ok(jit) => {
            let func = jit.get_prog();
            Some(RbpfJitBuffer {
                memory,
                layout,
                func,
            })
        }
        Err(e) => {
            ax_log::warn!("rbpf JIT compilation failed: {:?}", e);
            unsafe { alloc::alloc::dealloc(memory, layout) };
            None
        }
    }
}

impl RbpfJitBuffer {
    pub fn execute(&self, ctx: u64) -> u64 {
        unsafe { (self.func)(ctx as *mut u8, 0, ctx as *mut u8, 0, 0, 0) }
    }
}
