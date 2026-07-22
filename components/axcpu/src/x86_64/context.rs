use core::{
    arch::naked_asm,
    fmt,
    mem::{align_of, offset_of, size_of},
    ptr::NonNull,
};

use ax_memory_addr::VirtAddr;
use cpu_local::{CurrentThreadHeader, PreparedThreadSwitch};

use crate::{KernelTlsBase, TaskLocalState};

/// Saved registers when a trap (interrupt or exception) occurs.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TrapFrame {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    // Pushed by `trap.S`
    pub vector: u64,
    pub error_code: u64,

    // Pushed by CPU
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    /// Returns the privilege domain represented by this register image.
    pub const fn origin(&self) -> crate::TrapOrigin {
        if self.cs & 0b11 == 0 {
            crate::TrapOrigin::Kernel
        } else {
            crate::TrapOrigin::User
        }
    }

    /// Gets the 0th syscall argument.
    pub const fn arg0(&self) -> usize {
        self.rdi as _
    }

    /// Sets the 0th syscall argument.
    pub const fn set_arg0(&mut self, rdi: usize) {
        self.rdi = rdi as _;
    }

    /// Gets the 1st syscall argument.
    pub const fn arg1(&self) -> usize {
        self.rsi as _
    }

    /// Sets the 1st syscall argument.
    pub const fn set_arg1(&mut self, rsi: usize) {
        self.rsi = rsi as _;
    }

    /// Gets the 2nd syscall argument.
    pub const fn arg2(&self) -> usize {
        self.rdx as _
    }

    /// Sets the 2nd syscall argument.
    pub const fn set_arg2(&mut self, rdx: usize) {
        self.rdx = rdx as _;
    }

    /// Gets the 3rd syscall argument.
    pub const fn arg3(&self) -> usize {
        self.r10 as _
    }

    /// Sets the 3rd syscall argument.
    pub const fn set_arg3(&mut self, r10: usize) {
        self.r10 = r10 as _;
    }

    /// Gets the 4th syscall argument.
    pub const fn arg4(&self) -> usize {
        self.r8 as _
    }

    /// Sets the 4th syscall argument.
    pub const fn set_arg4(&mut self, r8: usize) {
        self.r8 = r8 as _;
    }

    /// Gets the 5th syscall argument.
    pub const fn arg5(&self) -> usize {
        self.r9 as _
    }

    /// Sets the 5th syscall argument.
    pub const fn set_arg5(&mut self, r9: usize) {
        self.r9 = r9 as _;
    }

    /// Gets the instruction pointer.
    pub const fn ip(&self) -> usize {
        self.rip as _
    }

    /// Sets the instruction pointer.
    pub const fn set_ip(&mut self, rip: usize) {
        self.rip = rip as _;
    }

    /// Gets the stack pointer.
    pub const fn sp(&self) -> usize {
        self.rsp as _
    }

    /// Sets the stack pointer.
    pub const fn set_sp(&mut self, rsp: usize) {
        self.rsp = rsp as _;
    }

    /// Gets the syscall number.
    pub const fn sysno(&self) -> usize {
        self.rax as usize
    }

    /// Sets the syscall number.
    pub const fn set_sysno(&mut self, rax: usize) {
        self.rax = rax as _;
    }

    /// Gets the return value register.
    pub const fn retval(&self) -> usize {
        self.rax as _
    }

    /// Sets the return value register.
    pub const fn set_retval(&mut self, rax: usize) {
        self.rax = rax as _;
    }

    /// Unwind the stack and get the backtrace.
    pub fn backtrace(&self) -> axbacktrace::Backtrace {
        axbacktrace::Backtrace::capture_trap(self.rbp as _, self.rip as _, 0)
    }
}

#[repr(C)]
#[derive(Debug, Default)]
struct ContextSwitchFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbx: u64,
    rbp: u64,
    rip: u64,
}

/// A 512-byte memory region for the FXSAVE/FXRSTOR instruction to save and
/// restore the x87 FPU, MMX, XMM, and MXCSR registers.
///
/// This is also the legacy region (offset 0..512) at the head of the
/// XSAVE/XRSTOR area, so it doubles as the start of [`XsaveArea`].
///
/// See <https://www.felixcloutier.com/x86/fxsave> for more details.
#[allow(missing_docs)]
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug)]
pub struct FxsaveArea {
    pub fcw: u16,
    pub fsw: u16,
    pub ftw: u16,
    pub fop: u16,
    pub fip: u64,
    pub fdp: u64,
    pub mxcsr: u32,
    pub mxcsr_mask: u32,
    pub st: [u64; 16],
    pub xmm: [u64; 32],
    _padding: [u64; 12],
}

const _: () = assert!(core::mem::size_of::<FxsaveArea>() == 512);

/// Size of the per-task XSAVE/XRSTOR area, in bytes.
///
/// The boot path ([`enable_xsave_features`]) only ever enables the x87, SSE,
/// and AVX components in `XCR0` (it never enables AVX-512, MPX, or PKRU), so the
/// largest XSAVE layout we must hold is the 512-byte legacy region, the 64-byte
/// XSAVE header, and the 256-byte AVX (`YMM_Hi128`) component. 1024 bytes covers
/// that with headroom and is a multiple of the required 64-byte alignment.
///
/// [`enable_xsave_features`]: ../../../../platforms/someboot/src/arch/x86_64/trap.rs
const XSAVE_AREA_SIZE: usize = 1024;

/// A 64-byte-aligned memory region for the XSAVE/XRSTOR instructions, which save
/// and restore the full `XCR0`-enabled extended state (x87, SSE/XMM, and the
/// upper 128 bits of the AVX `YMM` registers that FXSAVE/FXRSTOR drop).
///
/// The first 512 bytes share the legacy [`FxsaveArea`] layout, so the FXSAVE
/// fallback path (CPUs/VMs without XSAVE, e.g. the default `qemu64` model) reads
/// and writes the same region.
///
/// See <https://www.felixcloutier.com/x86/xsave> for more details.
#[repr(C, align(64))]
struct XsaveArea {
    /// Legacy region, identical in layout to the FXSAVE/FXRSTOR area.
    legacy: FxsaveArea,
    /// XSAVE header (`XSTATE_BV`, `XCOMP_BV`, reserved) plus the extended
    /// component area. A zeroed header marks every component as being in its
    /// initial state, which is the correct starting point for a fresh task.
    rest: [u8; XSAVE_AREA_SIZE - 512],
}

const _: () = assert!(core::mem::size_of::<XsaveArea>() == XSAVE_AREA_SIZE);

/// Extended state of a task, such as FP/SIMD states.
///
/// On context switch the state is saved/restored with XSAVE/XRSTOR when the boot
/// path enabled `CR4.OSXSAVE` (so that the AVX `YMM` upper halves are preserved),
/// and falls back to FXSAVE/FXRSTOR otherwise.
pub struct ExtendedState {
    area: XsaveArea,
}

#[cfg(feature = "fp-simd")]
impl ExtendedState {
    /// Provides access to the legacy FXSAVE region for compatibility with code
    /// that inspects the x87/SSE state directly.
    #[inline]
    pub fn fxsave_area(&self) -> &FxsaveArea {
        &self.area.legacy
    }

    /// Returns `true` when the boot path enabled XSAVE state management
    /// (`CR4.OSXSAVE`), which is the single source of truth for whether
    /// XSAVE/XRSTOR (and reading `XCR0` via `XGETBV`) are safe to use.
    #[inline]
    #[cfg(not(feature = "host-test"))]
    fn xsave_enabled() -> bool {
        // SAFETY: reading CR4 from ring 0 is always well-defined.
        let cr4 = unsafe { x86::controlregs::cr4() };
        cr4.contains(x86::controlregs::Cr4::CR4_ENABLE_OS_XSAVE)
    }

    /// Host scheduler tests execute at ring 3 and therefore cannot inspect
    /// CR4. FXSAVE/FXRSTOR remain available and cover the state exercised by
    /// the test fixture without changing the kernel's XSAVE policy.
    #[inline]
    #[cfg(feature = "host-test")]
    fn xsave_enabled() -> bool {
        false
    }

    /// The set of state components to save/restore, i.e. the `XCR0` mask the
    /// boot path programmed. Only valid to call when [`Self::xsave_enabled`].
    #[inline]
    fn xsave_mask() -> u64 {
        // SAFETY: `CR4.OSXSAVE` is set (checked by the caller), so XGETBV is
        // well-defined and will not #UD.
        unsafe { x86::controlregs::xcr0().bits() }
    }

    /// Saves the current extended states from CPU to this structure.
    #[inline]
    pub fn save(&mut self) {
        let ptr = &mut self.area as *mut _ as *mut u8;
        if Self::xsave_enabled() {
            // SAFETY: `area` is 64-byte aligned and large enough for the
            // XCR0-enabled state (x87/SSE/AVX); the mask matches XCR0.
            unsafe { core::arch::x86_64::_xsave64(ptr, Self::xsave_mask()) }
        } else {
            // SAFETY: `area` starts with the 16-byte-aligned legacy FXSAVE region.
            unsafe { core::arch::x86_64::_fxsave64(ptr) }
        }
    }

    /// Restores the extended states from this structure to CPU.
    #[inline]
    pub fn restore(&self) {
        let ptr = &self.area as *const _ as *const u8;
        if Self::xsave_enabled() {
            // SAFETY: `area` was populated by `_xsave64` (or zero-initialized,
            // which XRSTOR reads as the components' initial state) with a header
            // consistent with the XCR0 mask used here.
            unsafe { core::arch::x86_64::_xrstor64(ptr, Self::xsave_mask()) }
        } else {
            // SAFETY: `area` starts with the 16-byte-aligned legacy FXSAVE region.
            unsafe { core::arch::x86_64::_fxrstor64(ptr) }
        }
    }

    /// Returns the extended state with initialized values.
    pub const fn default() -> Self {
        // Zeroing the whole area gives XRSTOR an all-initial XSAVE header
        // (XSTATE_BV = 0) so the first restore loads each component's default
        // state; the legacy fields below seed the FXSAVE fallback path too.
        let mut area: XsaveArea = unsafe { core::mem::MaybeUninit::zeroed().assume_init() };
        area.legacy.fcw = 0x37f;
        // In the 512-byte FXSAVE/FXRSTOR area the x87 tag word is *abridged*: the
        // low byte of this field is one bit per x87 register, where 0 = empty and
        // 1 = occupied (FXRSTOR then derives the full tag from the register data).
        // A freshly-initialized FPU (FNINIT) has an EMPTY x87 stack, i.e. abridged
        // tag 0x00 — NOT the legacy full-tag-word value 0xFFFF (which encodes "all
        // empty" only in the 2-bits-per-register FSAVE/FRSTOR format). Seeding
        // 0xFFFF here set the abridged byte to 0xFF, so on the FXSAVE-fallback path
        // (CPUs/VMs without XSAVE, e.g. the default `qemu64` model, where
        // `ExtendedState::restore` uses FXRSTOR rather than XRSTOR) every new task
        // resumed with all eight x87 registers tagged occupied — a "full" stack.
        // The first `fld`/`fild` then overflowed it, yielding the x87 indefinite
        // value, which is exactly how musl's x87 long-double `fmt_fp` loop got a
        // wild operand, over-ran its on-stack digit array into the thread's `%fs:0`
        // TLS self-pointer, and triggered the recursive-SIGSEGV storm that broke
        // the x86 java workload. (On real XSAVE hardware XRSTOR re-inits x87 from
        // the zeroed XSTATE_BV header, which is why the bug was qemu64-only.)
        area.legacy.ftw = 0x0000;
        area.legacy.mxcsr = 0x1f80;
        Self { area }
    }
}

impl fmt::Debug for ExtendedState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ExtendedState")
            .field("fxsave_area", &self.area.legacy)
            .finish()
    }
}

/// Saved hardware states of a task.
///
/// The context usually includes:
///
/// - Callee-saved registers
/// - Stack pointer register
/// - Thread pointer register (for kernel-space thread-local storage)
/// - FP/SIMD registers
///
/// On context switch, current task saves its context from CPU to memory,
/// and the next task restores its context from memory to CPU.
///
/// On x86_64, callee-saved registers are saved to the kernel stack by the
/// `PUSH` instruction. So that [`rsp`] is the `RSP` after callee-saved
/// registers are pushed, and [`kstack_top`] is the top of the kernel stack
/// (`RSP` before any push).
///
/// [`rsp`]: TaskContext::rsp
/// [`kstack_top`]: TaskContext::kstack_top
#[repr(C)]
#[derive(Debug)]
pub struct TaskContext {
    /// The kernel stack top of the task.
    kstack_top: VirtAddr,
    /// `RSP` after all callee-saved registers are pushed.
    rsp: u64,
    /// Architecture-neutral current-header and kernel-TLS switch state.
    task_local: TaskLocalState,
    /// The `CR3` value restored for this task's userspace address space.
    #[cfg(feature = "uspace")]
    page_table_root: ax_memory_addr::PhysAddr,
    /// Extended states, i.e., FP/SIMD states.
    #[cfg(feature = "fp-simd")]
    ext_state: ExtendedState,
}

// The naked switch loads these fields with machine-word instructions. Keep the
// representation and adjacency assumptions executable as compile-time checks.
const _: () = {
    assert!(size_of::<KernelTlsBase>() == size_of::<usize>());
    assert!(align_of::<KernelTlsBase>() == align_of::<usize>());
    assert!(offset_of!(TaskContext, kstack_top) == 0);
    assert!(offset_of!(TaskContext, rsp) == size_of::<VirtAddr>());
    assert!(offset_of!(TaskContext, task_local) == offset_of!(TaskContext, rsp) + size_of::<u64>());
};

impl TaskContext {
    /// Creates a dummy context for a new task.
    ///
    /// Note the context is not initialized, it will be filled by
    /// [`switch_to_prepared`](Self::switch_to_prepared) (for initial tasks) and [`init`]
    /// (for regular tasks) methods.
    ///
    /// [`init`]: TaskContext::init
    pub fn new() -> Self {
        Self {
            kstack_top: va!(0),
            rsp: 0,
            task_local: TaskLocalState::new(),
            #[cfg(feature = "uspace")]
            page_table_root: crate::asm::read_kernel_page_table(),
            #[cfg(feature = "fp-simd")]
            ext_state: ExtendedState::default(),
        }
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, kernel_tls: KernelTlsBase) {
        unsafe {
            // x86_64 calling convention: the stack must be 16-byte aligned before
            // calling a function. That means when entering a new task (`ret` in `context_switch`
            // is executed), (stack pointer + 8) should be 16-byte aligned.
            let frame_ptr = (kstack_top.as_mut_ptr() as *mut u64).sub(1);
            let frame_ptr = (frame_ptr as *mut ContextSwitchFrame).sub(1);
            core::ptr::write(
                frame_ptr,
                ContextSwitchFrame {
                    rip: entry as _,
                    ..Default::default()
                },
            );
            self.rsp = frame_ptr as u64;
        }
        self.kstack_top = kstack_top;
        self.task_local.set_kernel_tls(kernel_tls);
    }

    /// Sets the pinned task-owned current-thread header restored by the raw
    /// switch tail in LinuxCurrent images.
    pub fn set_current_header(&mut self, header: NonNull<CurrentThreadHeader>) {
        self.task_local.set_current_header(header);
    }

    /// Returns the configured task-owned current-thread header.
    pub const fn current_header(&self) -> Option<NonNull<CurrentThreadHeader>> {
        self.task_local.current_header()
    }

    /// Changes the page table root restored for this task.
    #[cfg(feature = "uspace")]
    pub fn set_page_table_root(&mut self, page_table_root: ax_memory_addr::PhysAddr) {
        self.page_table_root = page_table_root;
    }

    /// Completes every helper operation that must precede current publication.
    pub fn prepare_switch_to(&mut self, _next_ctx: &Self) {
        #[cfg(feature = "fp-simd")]
        {
            self.ext_state.save();
            _next_ctx.ext_state.restore();
        }
        #[cfg(feature = "uspace")]
        if self.page_table_root != _next_ctx.page_table_root {
            // SAFETY: the scheduler owns both contexts with IRQs disabled.
            unsafe { crate::asm::write_user_page_table(_next_ctx.page_table_root) };
            // Writing CR3 flushes the non-global TLB entries.
        }
    }

    /// Commits current-thread publication and performs the raw transfer.
    ///
    /// # Safety
    ///
    /// The caller must have serialized scheduling, prepared FP/SIMD state, and
    /// `prepared` must belong to `next_ctx`. No fallible Rust work may be
    /// placed between its commit and the naked switch tail.
    #[inline(always)]
    pub unsafe fn switch_to_prepared(
        &mut self,
        next_ctx: &Self,
        prepared: PreparedThreadSwitch<'_>,
    ) {
        assert_eq!(
            next_ctx.current_header(),
            Some(prepared.next_header()),
            "prepared switch token must belong to the next task context",
        );
        unsafe { prepared.commit() };
        unsafe { context_switch_raw(self, next_ctx) }
    }
}

#[cfg(feature = "tls")]
#[unsafe(naked)]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        "
        .code64
        push    rbp
        push    rbx
        push    r12
        push    r13
        push    r14
        push    r15
        mov     [rdi + {rsp_offset}], rsp

        // Save and restore task TLS only after all Rust helpers have finished.
        mov     ecx, {fs_base_msr}
        rdmsr
        shl     rdx, 32
        or      rax, rdx
        mov     [rdi + {kernel_tls_offset}], rax
        mov     rax, [rsi + {kernel_tls_offset}]
        mov     rdx, rax
        shr     rdx, 32
        mov     ecx, {fs_base_msr}
        wrmsr

        mov     rsp, [rsi + {rsp_offset}]
        pop     r15
        pop     r14
        pop     r13
        pop     r12
        pop     rbx
        pop     rbp
        ret",
        rsp_offset = const offset_of!(TaskContext, rsp),
        kernel_tls_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, kernel_tls),
        fs_base_msr = const 0xc000_0100_u32,
    )
}

#[cfg(not(feature = "tls"))]
#[unsafe(naked)]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        "
        .code64
        push    rbp
        push    rbx
        push    r12
        push    r13
        push    r14
        push    r15
        mov     [rdi + {rsp_offset}], rsp

        // LinuxCurrent uses the already-published kernel GS slot. FS remains
        // userspace-owned and must not be touched by a kernel task switch.
        mov     rsp, [rsi + {rsp_offset}]
        pop     r15
        pop     r14
        pop     r13
        pop     r12
        pop     rbx
        pop     rbp
        ret",
        rsp_offset = const offset_of!(TaskContext, rsp),
    )
}
