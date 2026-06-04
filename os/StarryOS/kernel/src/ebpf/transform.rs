//! `KernelAuxiliaryOps` and `PerCpuVariantsOps` impls — the glue layer
//! between `kbpf-basic` and the tgoskits kernel.
//!
//! Source: `Starry-OS/StarryOS:ebpf-kmod`
//! `kernel/src/bpf/tansform.rs`. The original filename had a typo
//! (`tansform`), corrected here to `transform.rs`. The behavioral changes
//! are limited to:
//! * package-name imports (`axhal` → `ax_runtime::hal`, `axalloc` → `ax_alloc`, etc.);
//! * `AxError`/`AxResult` ↔ `kbpf_basic::BpfError`/`BpfResult` boundary;
//! * use of tgoskits' `mm::{VmBytes, VmBytesMut, vm_load_string}` and the
//!   in-tree frame allocator instead of the source `alloc_frame` helper.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ffi::c_void,
    fmt::{Debug, Formatter},
};

use ax_io::{Read, Write};
use ax_memory_addr::{PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{
    paging::{MappingFlags, PageSize},
    percpu::this_cpu_id,
    time::monotonic_time_nanos,
};
use kbpf_basic::{
    BpfError, KernelAuxiliaryOps,
    map::{PerCpuVariants, PerCpuVariantsOps, UnifiedMap},
    preprocessor::EbpfInst,
};
use rbpf::ebpf::Insn;

use crate::{
    ebpf::map::BpfMap,
    file::get_file_like,
    mm::{VmBytes, VmBytesMut, vm_load_string},
};

/// Per-cpu variants implementation backed by a fixed-length `Vec<T>` of
/// size `cpu_num()`, indexed by `this_cpu_id()`. Same shape as the source
/// impl in `ebpf-kmod`; safety contract unchanged.
#[derive(Debug)]
pub struct PerCpuImpl;

impl PerCpuVariantsOps for PerCpuImpl {
    fn create<T: Clone + Sync + Send + 'static>(value: T) -> Option<Box<dyn PerCpuVariants<T>>> {
        Some(Box::new(PerCpuVariantsImpl::new_with_value(value)))
    }

    fn num_cpus() -> u32 {
        ax_runtime::hal::cpu_num() as _
    }
}

/// Concrete per-cpu container. Each CPU has its own `UnsafeCell<T>` slot in a
/// fixed-length `Box<[..]>`; `get()` / `get_mut()` index by `this_cpu_id()`,
/// the `force_*` variants by an explicit CPU id. Slots are never re-allocated
/// after construction, so `&UnsafeCell<T>` references remain valid for the
/// lifetime of the container.
pub struct PerCpuVariantsImpl<T> {
    /// One slot per CPU. `Box<[..]>` (not `Vec<..>`) so the backing storage
    /// is treated as immutable after construction — no re-allocation can
    /// move slots out from under outstanding `&UnsafeCell<T>` borrows.
    slots: Box<[UnsafeCell<T>]>,
}

// SAFETY: each CPU owns a disjoint slot. `get()` / `get_mut()` only touch the
// caller-CPU's slot under the per-cpu preemption-disabled invariant the
// `kbpf-basic` contract imposes on its callers; the `force_*` variants are
// `unsafe` and forward that obligation to the caller. As long as `T: Send`
// the container can be sent between CPUs; as long as `T: Sync` two CPUs may
// hold `&PerCpuVariantsImpl<T>` simultaneously (each touching its own slot).
unsafe impl<T: Send> Send for PerCpuVariantsImpl<T> {}
unsafe impl<T: Sync> Sync for PerCpuVariantsImpl<T> {}

impl<T: Send + Sync + Clone> PerCpuVariantsImpl<T> {
    /// Build a per-cpu container pre-filled with `value` for every CPU.
    pub fn new_with_value(value: T) -> Self {
        let n = ax_runtime::hal::cpu_num();
        let mut slots = Vec::with_capacity(n);
        for _ in 0..n {
            slots.push(UnsafeCell::new(value.clone()));
        }
        Self {
            slots: slots.into_boxed_slice(),
        }
    }
}

impl<T> Debug for PerCpuVariantsImpl<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerCpuVariantsImpl").finish()
    }
}

impl<T: Send + Sync + Clone> PerCpuVariants<T> for PerCpuVariantsImpl<T> {
    fn get(&self) -> &T {
        // SAFETY: per-cpu slot for the current CPU; concurrent `get_mut` on
        // the same slot is impossible while the caller-CPU holds preemption
        // disabled (the kbpf-basic contract for these helpers).
        unsafe { &*self.slots[this_cpu_id()].get() }
    }

    fn get_mut(&self) -> &mut T {
        // SAFETY: per-cpu slot for the current CPU. The kbpf-basic contract
        // guarantees the caller has preemption disabled, so no other thread
        // on this CPU observes the same slot, and other CPUs touch only
        // their own (disjoint) slots.
        unsafe { &mut *self.slots[this_cpu_id()].get() }
    }

    unsafe fn force_get(&self, cpu: u32) -> &T {
        // SAFETY: forwarded to the caller — they must ensure no concurrent
        // `*_get_mut` access on slot `cpu`.
        unsafe { &*self.slots[cpu as usize].get() }
    }

    unsafe fn force_get_mut(&self, cpu: u32) -> &mut T {
        // SAFETY: forwarded to the caller — they must ensure exclusive
        // access to slot `cpu` for the lifetime of the returned reference.
        unsafe { &mut *self.slots[cpu as usize].get() }
    }
}

/// The kernel-side glue type that satisfies `kbpf_basic::KernelAuxiliaryOps`.
/// Stateless marker; all callbacks reach into the tgoskits subsystems
/// directly.
#[derive(Debug)]
pub struct EbpfKernelAuxiliary;

impl KernelAuxiliaryOps for EbpfKernelAuxiliary {
    fn get_unified_map_from_ptr<F, R>(ptr: *const u8, func: F) -> kbpf_basic::BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> kbpf_basic::BpfResult<R>,
    {
        // SAFETY: ptr was produced by `Arc::into_raw` in
        // `get_unified_map_ptr_from_fd`; the caller passes it back here so
        // we may reconstruct the Arc, run the closure, and re-leak it.
        let map = unsafe { Arc::from_raw(ptr as *const BpfMap) };
        let mut unified = map.unified_map();
        let ret = func(&mut unified);
        drop(unified);
        let _ = Arc::into_raw(map);
        ret
    }

    fn get_unified_map_from_fd<F, R>(map_fd: u32, func: F) -> kbpf_basic::BpfResult<R>
    where
        F: FnOnce(&mut UnifiedMap) -> kbpf_basic::BpfResult<R>,
    {
        let file = get_file_like(map_fd as _).map_err(|_| BpfError::ENOENT)?;
        let bpf_map = file
            .into_any_arc()
            .downcast::<BpfMap>()
            .map_err(|_| BpfError::EINVAL)?;
        let unified = &mut bpf_map.unified_map();
        func(unified)
    }

    // `#[inline(never)]` here (and on the other leaf `KernelAuxiliaryOps`
    // callbacks below) is load-bearing for the loadable-module path: a
    // `kebpf.ko` carries its own copy of the stateless `kbpf-basic` map/prog
    // logic but must call back into *this* kernel's address-space / fd-table /
    // frame-allocator glue. The module relocates against these methods by their
    // exact mangled name through `.kallsyms`, so they must survive as standalone
    // symbols. Without `inline(never)` the optimizer folds them into the
    // built-in `sys_bpf` call sites and they vanish from the symbol table (the
    // kallsyms filter keeps even *local* `t` symbols, so non-`pub` is fine —
    // only inlining, not visibility, removes them). See
    // `docs/ebpf-followup/syscall-registration.md`.
    #[inline(never)]
    fn get_unified_map_ptr_from_fd(map_fd: u32) -> kbpf_basic::BpfResult<*const u8> {
        let file = get_file_like(map_fd as _).map_err(|_| BpfError::ENOENT)?;
        let bpf_map = file
            .into_any_arc()
            .downcast::<BpfMap>()
            .map_err(|_| BpfError::EINVAL)?;
        Ok(Arc::into_raw(bpf_map) as *const u8)
    }

    #[inline(never)]
    fn translate_instruction(
        instruction: Vec<u8>,
    ) -> kbpf_basic::BpfResult<Vec<impl kbpf_basic::preprocessor::EbpfInst>> {
        let insns = rbpf::ebpf::to_insn_vec(&instruction);
        let translated = insns.into_iter().map(TranslatedInst).collect();
        Ok(translated)
    }

    #[inline(never)]
    fn copy_from_user(src: *const u8, size: usize, dst: &mut [u8]) -> kbpf_basic::BpfResult<()> {
        let n = VmBytes::new(src, size)
            .read(dst)
            .map_err(|_| BpfError::EFAULT)?;
        if n == size {
            Ok(())
        } else {
            Err(BpfError::EFAULT)
        }
    }

    #[inline(never)]
    fn copy_to_user(dest: *mut u8, size: usize, src: &[u8]) -> kbpf_basic::BpfResult<()> {
        let n = VmBytesMut::new(dest, size)
            .write(src)
            .map_err(|_| BpfError::EFAULT)?;
        if n == size {
            Ok(())
        } else {
            Err(BpfError::EFAULT)
        }
    }

    fn current_cpu_id() -> u32 {
        this_cpu_id() as _
    }

    fn perf_event_output(
        ctx: *mut c_void,
        fd: u32,
        flags: u32,
        data: &[u8],
    ) -> kbpf_basic::BpfResult<()> {
        crate::perf::perf_event_output(ctx, fd as usize, flags, data).map_err(|_| BpfError::EINVAL)
    }

    #[inline(never)]
    fn string_from_user_cstr(ptr: *const u8) -> kbpf_basic::BpfResult<String> {
        vm_load_string(ptr as *const _).map_err(|_| BpfError::EFAULT)
    }

    fn ebpf_write_str(s: &str) -> kbpf_basic::BpfResult<()> {
        info!("[bpf_trace] {}", s);
        Ok(())
    }

    fn ebpf_time_ns() -> kbpf_basic::BpfResult<u64> {
        Ok(monotonic_time_nanos())
    }

    #[inline(never)]
    fn alloc_page() -> kbpf_basic::BpfResult<usize> {
        // Reuse the address-space backend's frame allocator
        // (`mm::aspace::backend::alloc_frame`) so eBPF page allocation goes
        // through the same path as the rest of the kernel.
        crate::mm::alloc_frame(true, PageSize::Size4K)
            .map(|p| p.as_usize())
            .map_err(|_| BpfError::ENOMEM)
    }

    #[inline(never)]
    fn free_page(phys_addr: usize) {
        crate::mm::dealloc_frame(PhysAddr::from_usize(phys_addr), PageSize::Size4K);
    }

    #[inline(never)]
    fn vmap(phys_addrs: &[usize]) -> kbpf_basic::BpfResult<usize> {
        let len = phys_addrs.len() * PageSize::Size4K as usize;
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        let mut virt_start = guard
            .find_free_area(
                guard.base(),
                len,
                VirtAddrRange::new(guard.base(), guard.end()),
            )
            .ok_or(BpfError::ENOMEM)?;
        let res_virt = virt_start.as_usize();
        for phys in phys_addrs {
            let start_paddr = PhysAddr::from_usize(*phys);
            guard
                .map_linear(
                    virt_start,
                    start_paddr,
                    PageSize::Size4K as usize,
                    MappingFlags::READ | MappingFlags::WRITE,
                )
                .map_err(|_| BpfError::EINVAL)?;
            virt_start += PageSize::Size4K as usize;
        }
        Ok(res_virt)
    }

    #[inline(never)]
    fn unmap(virt_addr: usize) {
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        let _ = guard.unmap(VirtAddr::from_usize(virt_addr), PageSize::Size4K as usize);
    }
}

/// Adapter so `rbpf::ebpf::Insn` satisfies `kbpf_basic::preprocessor::EbpfInst`.
#[derive(Clone)]
pub struct TranslatedInst(pub Insn);

impl EbpfInst for TranslatedInst {
    fn imm(&self) -> i32 {
        self.0.imm
    }

    fn opc(&self) -> u8 {
        self.0.opc
    }

    fn src(&self) -> u8 {
        self.0.src
    }

    fn set_imm(&mut self, imm: i32) {
        self.0.imm = imm;
    }

    fn to_array(&self) -> [u8; 8] {
        self.0.to_array()
    }
}
