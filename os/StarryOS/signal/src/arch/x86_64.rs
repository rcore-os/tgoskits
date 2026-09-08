use alloc::{vec, vec::Vec};
use core::mem::{MaybeUninit, size_of};

use ax_cpu::uspace::UserContext;
use starry_vm::{VmError, VmIo};

use crate::{SignalResult, SignalSet, SignalStack};

const UC_FP_XSTATE: usize = 0x1;
const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;
const FXSAVE_SIZE: usize = 512;
const FXSAVE_SW_RESERVED_OFFSET: usize = 464;
const FXSAVE_SW_RESERVED_SIZE: usize = 48;
const XSAVE_HEADER_OFFSET: usize = 512;
const XSAVE_HEADER_SIZE: usize = 64;
const XFEATURE_MASK_FPSSE: u64 = (1 << 0) | (1 << 1);
const XSTATE_ALIGNMENT: usize = 64;

core::arch::global_asm!(
    "
.section .text
.code64
.balign 4096
.global signal_trampoline
signal_trampoline:
    mov rax, 0xf
    syscall

.fill 4096 - (. - signal_trampoline), 1, 0
"
);

// Linux x86-64 `mcontext_t` (`struct sigcontext`) has NATURAL 8-byte alignment,
// NOT 16. In `ucontext_t`, `uc_mcontext` sits in the MIDDLE of the struct (offset
// 40, after uc_flags/uc_link/uc_stack); forcing `align(16)` here inserts 8 bytes
// of padding and pushes `uc_mcontext` to offset 48 — shifting every general
// register (RSP@160, RIP@168), the fpregs pointer and `uc_sigmask` 8 bytes off
// the Linux ABI. Runtimes that read the context by raw ABI offset — notably Go's
// async preemption, which reads/writes `uc_mcontext.gregs[REG_RSP/REG_RIP]` and
// expects `rt_sigreturn` to honor those writes — then corrupt RSP/RIP (observed:
// `sp` loaded with adjacent non-pointer bytes -> `unsafe.Slice: len out of range`).
// The 16-byte alignment the signal frame itself requires is provided by the outer
// `UContext` (see below), not by over-aligning this inner struct.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MContext {
    r8: usize,
    r9: usize,
    r10: usize,
    r11: usize,
    r12: usize,
    r13: usize,
    r14: usize,
    r15: usize,
    rdi: usize,
    rsi: usize,
    rbp: usize,
    rbx: usize,
    rdx: usize,
    rax: usize,
    rcx: usize,
    rsp: usize,
    rip: usize,
    eflags: usize,
    cs: u16,
    gs: u16,
    fs: u16,
    _pad: u16,
    err: usize,
    trapno: usize,
    oldmask: usize,
    cr2: usize,
    fpstate: usize,
    _reserved1: [usize; 8],
}

// SAFETY: all C-layout fields are integers and the four adjacent `u16` fields
// explicitly fill the only sub-word region.
unsafe impl bytemuck::NoUninit for MContext {}

impl MContext {
    pub fn new(uctx: &UserContext) -> Self {
        Self {
            r8: uctx.r8 as _,
            r9: uctx.r9 as _,
            r10: uctx.r10 as _,
            r11: uctx.r11 as _,
            r12: uctx.r12 as _,
            r13: uctx.r13 as _,
            r14: uctx.r14 as _,
            r15: uctx.r15 as _,
            rdi: uctx.rdi as _,
            rsi: uctx.rsi as _,
            rbp: uctx.rbp as _,
            rbx: uctx.rbx as _,
            rdx: uctx.rdx as _,
            rax: uctx.rax as _,
            rcx: uctx.rcx as _,
            rsp: uctx.rsp as _,
            rip: uctx.rip as _,
            eflags: uctx.rflags as _,
            cs: uctx.cs as _,
            gs: 0,
            fs: 0,
            _pad: 0,
            err: uctx.error_code as _,
            trapno: uctx.vector as _,
            oldmask: 0,
            cr2: 0,
            fpstate: 0,
            _reserved1: [0; 8],
        }
    }

    pub fn restore(&self, uctx: &mut UserContext) {
        uctx.r8 = self.r8 as _;
        uctx.r9 = self.r9 as _;
        uctx.r10 = self.r10 as _;
        uctx.r11 = self.r11 as _;
        uctx.r12 = self.r12 as _;
        uctx.r13 = self.r13 as _;
        uctx.r14 = self.r14 as _;
        uctx.r15 = self.r15 as _;
        uctx.rdi = self.rdi as _;
        uctx.rsi = self.rsi as _;
        uctx.rbp = self.rbp as _;
        uctx.rbx = self.rbx as _;
        uctx.rdx = self.rdx as _;
        uctx.rax = self.rax as _;
        uctx.rcx = self.rcx as _;
        uctx.rsp = self.rsp as _;
        uctx.rip = self.rip as _;
        uctx.rflags = self.eflags as _;
        uctx.cs = self.cs as _;
        uctx.error_code = self.err as _;
        uctx.vector = self.trapno as _;
    }

    const fn fpstate(&self) -> usize {
        self.fpstate
    }

    fn set_fpstate(&mut self, fpstate: usize) {
        self.fpstate = fpstate;
    }
}

// `align(16)` keeps the whole signal frame 16-byte aligned on the user stack (the
// x86-64 signal ABI requires the handler to observe `RSP % 16 == 8` after its
// return address is pushed). This frame alignment was previously (incorrectly)
// supplied by over-aligning the inner `MContext`, which corrupted `uc_mcontext`'s
// offset; aligning the outer `UContext` instead keeps `uc_mcontext` at the Linux
// ABI offset 40 while still guaranteeing frame alignment.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct UContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub mcontext: MContext,
    pub sigmask: SignalSet,
}

// SAFETY: every field implements `NoUninit`; the existing ABI offset assertions
// prove there is no hidden inter-field or trailing padding.
unsafe impl bytemuck::NoUninit for UContext {}

impl UContext {
    pub fn new(uctx: &UserContext, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            mcontext: MContext::new(uctx),
            sigmask,
        }
    }

    /// Publishes the x86 signal fpstate payload through the Linux UABI.
    pub fn set_fpstate(&mut self, fpstate: usize, has_xstate: bool) {
        self.mcontext.set_fpstate(fpstate);
        if has_xstate {
            self.flags |= UC_FP_XSTATE;
        }
    }

    /// Returns the signal fpstate pointer supplied by userspace.
    pub const fn fpstate(&self) -> usize {
        self.mcontext.fpstate()
    }
}

/// Task-owned x86 FPU snapshot encoded in Linux's signal-frame UABI.
pub struct SignalFpState {
    state: ax_cpu::UserXstate,
}

impl SignalFpState {
    /// Wraps a task-owned xstate captured by the current-task runtime boundary.
    pub const fn new(state: ax_cpu::UserXstate) -> Self {
        Self { state }
    }

    /// Returns whether the signal payload uses Linux's extended XSAVE format.
    pub fn has_xstate(&self) -> bool {
        ax_cpu::UserXstate::user_size().is_some()
    }

    /// Returns the payload size, including Linux's trailing XSAVE magic word.
    pub fn frame_size(&self) -> usize {
        ax_cpu::UserXstate::user_size().map_or(FXSAVE_SIZE, |size| size + size_of::<u32>())
    }

    /// Writes an aligned Linux x86 signal fpstate payload to userspace.
    pub fn write<I: VmIo>(&self, vm: &mut I, address: usize) -> SignalResult<()> {
        if !address.is_multiple_of(XSTATE_ALIGNMENT) {
            return Err(VmError::BadAddress.into());
        }

        let mut frame = vec![0; self.frame_size()];
        if let Some(user_size) = ax_cpu::UserXstate::user_size() {
            frame[..user_size].copy_from_slice(
                self.state
                    .user_bytes()
                    .expect("XSAVE signal size and task image must agree"),
            );
            frame[FXSAVE_SW_RESERVED_OFFSET..FXSAVE_SW_RESERVED_OFFSET + FXSAVE_SW_RESERVED_SIZE]
                .fill(0);
            write_u32(&mut frame, FXSAVE_SW_RESERVED_OFFSET, FP_XSTATE_MAGIC1);
            write_u32(
                &mut frame,
                FXSAVE_SW_RESERVED_OFFSET + 4,
                (user_size + size_of::<u32>()) as u32,
            );
            write_u64(
                &mut frame,
                FXSAVE_SW_RESERVED_OFFSET + 8,
                ax_cpu::UserXstate::user_feature_mask(),
            );
            write_u32(&mut frame, FXSAVE_SW_RESERVED_OFFSET + 16, user_size as u32);
            let xstate_bv = read_u64(&frame, XSAVE_HEADER_OFFSET) | XFEATURE_MASK_FPSSE;
            write_u64(&mut frame, XSAVE_HEADER_OFFSET, xstate_bv);
            write_u32(&mut frame, user_size, FP_XSTATE_MAGIC2);
        } else {
            frame.copy_from_slice(self.state.fxsave_bytes());
            frame[FXSAVE_SW_RESERVED_OFFSET..FXSAVE_SW_RESERVED_OFFSET + FXSAVE_SW_RESERVED_SIZE]
                .fill(0);
        }
        vm.write(address, &frame)?;
        Ok(())
    }

    /// Decodes a Linux x86 signal fpstate payload.
    ///
    /// `None` represents Linux's null-fpstate request to reset the current task
    /// to the architecture initial FPU state.
    pub fn restore<I: VmIo>(
        vm: &mut I,
        address: usize,
    ) -> SignalResult<Option<ax_cpu::UserXstate>> {
        if address == 0 {
            return Ok(None);
        }
        Self::restore_inner(vm, address).map(Some)
    }

    fn restore_inner<I: VmIo>(vm: &mut I, address: usize) -> SignalResult<ax_cpu::UserXstate> {
        if !address.is_multiple_of(16) {
            return Err(VmError::BadAddress.into());
        }
        let legacy = read_user_bytes(vm, address, FXSAVE_SIZE)?;
        let mut state = ax_cpu::UserXstate::initial();

        let valid = if let Some(user_size) = ax_cpu::UserXstate::user_size() {
            Self::restore_xsave_or_legacy(vm, address, user_size, &legacy, &mut state)?
        } else {
            state.replace_fxsave_bytes(&legacy)
        };
        if !valid {
            return Err(VmError::BadAddress.into());
        }
        Ok(state)
    }

    fn restore_xsave_or_legacy<I: VmIo>(
        vm: &mut I,
        address: usize,
        user_size: usize,
        legacy: &[u8],
        state: &mut ax_cpu::UserXstate,
    ) -> SignalResult<bool> {
        let magic1 = read_u32(legacy, FXSAVE_SW_RESERVED_OFFSET);
        let extended_size = read_u32(legacy, FXSAVE_SW_RESERVED_OFFSET + 4) as usize;
        let signal_xfeatures = read_u64(legacy, FXSAVE_SW_RESERVED_OFFSET + 8);
        let xstate_size = read_u32(legacy, FXSAVE_SW_RESERVED_OFFSET + 16) as usize;
        let metadata_valid = magic1 == FP_XSTATE_MAGIC1
            && (XSAVE_HEADER_OFFSET + XSAVE_HEADER_SIZE..=user_size).contains(&xstate_size)
            && xstate_size <= extended_size;

        if metadata_valid {
            if !address.is_multiple_of(XSTATE_ALIGNMENT) {
                return Ok(false);
            }
            let mut frame = read_user_bytes(
                vm,
                address,
                xstate_size
                    .checked_add(size_of::<u32>())
                    .ok_or(VmError::BadAddress)?,
            )?;
            if read_u32(&frame, xstate_size) == FP_XSTATE_MAGIC2 {
                let allowed = ax_cpu::UserXstate::user_feature_mask();
                let user_xstate_bv = read_u64(&frame, XSAVE_HEADER_OFFSET);
                if user_xstate_bv & !allowed != 0 {
                    return Ok(false);
                }
                let xstate_bv = user_xstate_bv & signal_xfeatures & allowed;
                write_u64(&mut frame, XSAVE_HEADER_OFFSET, xstate_bv);
                frame[FXSAVE_SW_RESERVED_OFFSET
                    ..FXSAVE_SW_RESERVED_OFFSET + FXSAVE_SW_RESERVED_SIZE]
                    .fill(0);
                return Ok(state.replace_user_bytes_prefix(&frame[..xstate_size]));
            }
        }

        let mut standard = vec![0; user_size];
        standard[..FXSAVE_SIZE].copy_from_slice(legacy);
        standard[FXSAVE_SW_RESERVED_OFFSET..FXSAVE_SW_RESERVED_OFFSET + FXSAVE_SW_RESERVED_SIZE]
            .fill(0);
        write_u64(&mut standard, XSAVE_HEADER_OFFSET, XFEATURE_MASK_FPSSE);
        Ok(state.replace_user_bytes(&standard))
    }
}

impl Default for SignalFpState {
    fn default() -> Self {
        Self::new(ax_cpu::UserXstate::initial())
    }
}

/// Decoded x86 signal FPU state; `None` requests the initial state.
pub type SignalFpRestore = Option<ax_cpu::UserXstate>;

fn read_user_bytes<I: VmIo>(vm: &mut I, address: usize, size: usize) -> SignalResult<Vec<u8>> {
    address.checked_add(size).ok_or(VmError::BadAddress)?;
    let mut bytes = vec![0; size];
    // SAFETY: the `u8` buffer is already initialized. Exposing it as
    // `MaybeUninit<u8>` only permits `VmIo::read` to overwrite those bytes.
    let destination = unsafe {
        core::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<MaybeUninit<u8>>(), bytes.len())
    };
    vm.read(address, destination)?;
    Ok(bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(
        bytes[offset..offset + size_of::<u32>()]
            .try_into()
            .expect("x86 signal frame u32 field has a fixed width"),
    )
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(
        bytes[offset..offset + size_of::<u64>()]
            .try_into()
            .expect("x86 signal frame u64 field has a fixed width"),
    )
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_ne_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_ne_bytes());
}

const _: () = {
    // Lock the Linux/musl x86-64 `ucontext_t` ABI offsets (compile-time regression
    // guard for the alignment trap documented on `MContext`): `uc_mcontext`@40,
    // `uc_sigmask`@296.
    assert!(core::mem::offset_of!(UContext, mcontext) == 40);
    assert!(core::mem::offset_of!(UContext, sigmask) == 296);
    assert!(core::mem::size_of::<MContext>() == 256);
    assert!(core::mem::size_of::<UContext>() == 304);
};
