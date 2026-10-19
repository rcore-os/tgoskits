use ax_cpu::{registers::FpState, user::UserContext};

use crate::{SignalError, SignalResult, SignalSet, SignalStack};

/// Linux arm64 `struct _aarch64_ctx` record header (`uapi/asm/sigcontext.h`).
const CONTEXT_HEADER_SIZE: usize = 8;
/// The record that carries the interrupted FPSIMD state.
const FPSIMD_MAGIC: u32 = 0x4650_8001;
/// An informational record Linux appends for faulting signals; it is skipped.
const ESR_MAGIC: u32 = 0x4553_5201;
/// `struct fpsimd_context`: header, FPSR, FPCR and the 32 vector registers.
const FPSIMD_CONTEXT_SIZE: usize = 528;
const FPSIMD_FPSR_OFFSET: usize = 8;
const FPSIMD_FPCR_OFFSET: usize = 12;
const FPSIMD_VREG_SIZE: usize = 16;
const FPSIMD_VREGS_OFFSET: usize = 16;
/// Every record starts on a 16-byte boundary while it fits `__reserved`.
const RECORD_ALIGNMENT: usize = 16;

core::arch::global_asm!(
    "
.section .text
.balign 4096
.global signal_trampoline
signal_trampoline:
    mov x8, #139
    svc #0

.fill 4096 - (. - signal_trampoline), 1, 0
"
);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct MContextPadding([u8; 4096]);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MContext {
    fault_address: u64,
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
    __reserved_align: [u8; 8],
    __reserved: MContextPadding,
}

// SAFETY: every byte is covered by integer arrays, including the explicit
// alignment gap before the 16-byte-aligned reserved area.
unsafe impl bytemuck::NoUninit for MContext {}

impl MContext {
    pub fn new(uctx: &UserContext) -> Self {
        Self {
            fault_address: 0,
            regs: uctx.x,
            sp: uctx.sp,
            pc: uctx.elr,
            pstate: uctx.spsr,
            __reserved_align: [0; 8],
            __reserved: MContextPadding([0; 4096]),
        }
    }

    pub fn restore(&self, uctx: &mut UserContext) {
        uctx.x = self.regs;
        uctx.sp = self.sp;
        uctx.elr = self.pc;
        uctx.spsr = self.pstate;
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub sigmask: SignalSet,
    __unused: [u8; 1024 / 8 - size_of::<SignalSet>()],
    __mcontext_align: [u8; 8],
    pub mcontext: MContext,
}

// SAFETY: every field implements `NoUninit`; the explicit alignment field
// consumes the gap before `MContext`.
unsafe impl bytemuck::NoUninit for UContext {}

impl UContext {
    pub fn new(uctx: &UserContext, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            sigmask,
            __unused: [0; 1024 / 8 - size_of::<SignalSet>()],
            __mcontext_align: [0; 8],
            mcontext: MContext::new(uctx),
        }
    }

    /// Publishes the captured FPSIMD state through the Linux signal UABI.
    pub fn set_fpstate(&mut self, fpstate: &SignalFpState) {
        fpstate.write(&mut self.mcontext);
    }

    /// Decodes the FPSIMD image the handler left in the frame reservation.
    pub fn fpstate(&self) -> SignalResult<SignalFpRestore> {
        SignalFpState::restore(&self.mcontext)
    }
}

/// Task-owned AArch64 FPSIMD snapshot encoded in Linux's signal-frame UABI.
pub struct SignalFpState {
    state: FpState,
}

impl SignalFpState {
    /// Wraps a task-owned FPSIMD image captured at the current-task boundary.
    pub fn new(state: FpState) -> Self {
        Self { state }
    }

    /// Writes the FPSIMD record and its terminator into the reservation.
    ///
    /// Linux lays the record out at the start of `__reserved`; the zeroed
    /// header of the following terminator record stays implicit because the
    /// whole reservation is cleared first.
    fn write(&self, mcontext: &mut MContext) {
        let reserved = &mut mcontext.__reserved.0;
        reserved.fill(0);
        reserved[..4].copy_from_slice(&FPSIMD_MAGIC.to_ne_bytes());
        reserved[4..8].copy_from_slice(&(FPSIMD_CONTEXT_SIZE as u32).to_ne_bytes());
        reserved[FPSIMD_FPSR_OFFSET..FPSIMD_FPSR_OFFSET + 4]
            .copy_from_slice(&self.state.fpsr.to_ne_bytes());
        reserved[FPSIMD_FPCR_OFFSET..FPSIMD_FPCR_OFFSET + 4]
            .copy_from_slice(&self.state.fpcr.to_ne_bytes());
        for (index, register) in self.state.regs.iter().enumerate() {
            let offset = FPSIMD_VREGS_OFFSET + index * FPSIMD_VREG_SIZE;
            reserved[offset..offset + FPSIMD_VREG_SIZE].copy_from_slice(&register.to_ne_bytes());
        }
    }

    /// Decodes the FPSIMD image from a frame reservation.
    ///
    /// Mirrors Linux `parse_user_sigframe` followed by `restore_sigframe`: the
    /// record list must be terminated, every record must be aligned and fit the
    /// reservation, an FPSIMD record must appear exactly once with the ABI size,
    /// and a frame without one is rejected because this system supports FPSIMD.
    fn restore(mcontext: &MContext) -> SignalResult<SignalFpRestore> {
        let reserved = &mcontext.__reserved.0;
        let mut offset = 0;
        let mut fpsimd = None;
        loop {
            if reserved.len() - offset < CONTEXT_HEADER_SIZE {
                return Err(malformed_frame());
            }
            if !offset.is_multiple_of(RECORD_ALIGNMENT) {
                return Err(malformed_frame());
            }
            let magic = read_u32(reserved, offset);
            let size = read_u32(reserved, offset + 4) as usize;
            if magic == 0 {
                // The list ends with an empty terminator record.
                if size != 0 {
                    return Err(malformed_frame());
                }
                break;
            }
            if reserved.len() - offset < size {
                return Err(malformed_frame());
            }
            match magic {
                FPSIMD_MAGIC => {
                    if fpsimd.is_some() || size != FPSIMD_CONTEXT_SIZE {
                        return Err(malformed_frame());
                    }
                    fpsimd = Some(offset);
                }
                ESR_MAGIC => {}
                // Unknown extensions are rejected instead of being guessed at.
                _ => return Err(malformed_frame()),
            }
            if size < CONTEXT_HEADER_SIZE || !size.is_multiple_of(RECORD_ALIGNMENT) {
                return Err(malformed_frame());
            }
            offset += size;
        }

        let Some(fpsimd) = fpsimd else {
            return Err(malformed_frame());
        };
        let mut state = FpState::default();
        state.fpsr = read_u32(reserved, fpsimd + FPSIMD_FPSR_OFFSET);
        state.fpcr = read_u32(reserved, fpsimd + FPSIMD_FPCR_OFFSET);
        for (index, register) in state.regs.iter_mut().enumerate() {
            let offset = fpsimd + FPSIMD_VREGS_OFFSET + index * FPSIMD_VREG_SIZE;
            let mut bytes = [0u8; FPSIMD_VREG_SIZE];
            bytes.copy_from_slice(&reserved[offset..offset + FPSIMD_VREG_SIZE]);
            *register = u128::from_ne_bytes(bytes);
        }
        Ok(state)
    }
}

impl Default for SignalFpState {
    /// Constructs the architectural initial image without reading the CPU.
    /// Live task delivery uses `new` with the runtime's captured image instead.
    fn default() -> Self {
        Self {
            state: FpState::default(),
        }
    }
}

/// FPSIMD image to install after `rt_sigreturn` decoded a valid frame.
pub type SignalFpRestore = FpState;

/// Rejects a reservation that is not a valid Linux arm64 signal frame.
///
/// Linux returns `-EINVAL` from `restore_sigframe` and then injects SIGSEGV
/// from `rt_sigreturn`. `SignalError` has no frame variant, so the rejection
/// reuses the user-memory error the x86_64 payload decoder also reports.
fn malformed_frame() -> SignalError {
    SignalError::from(crate::VmError::BadAddress)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    let mut value = [0u8; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_ne_bytes(value)
}

const _: () = {
    assert!(core::mem::offset_of!(MContext, __reserved) == 288);
    assert!(core::mem::offset_of!(MContext, __reserved) % RECORD_ALIGNMENT == 0);
    assert!(size_of::<MContext>() == 4384);
    assert!(core::mem::offset_of!(UContext, mcontext) == 176);
    assert!(size_of::<UContext>() == 4560);
    // The FPSIMD record and its terminator always fit the reservation.
    assert!(FPSIMD_VREGS_OFFSET + 32 * FPSIMD_VREG_SIZE == FPSIMD_CONTEXT_SIZE);
    assert!(FPSIMD_CONTEXT_SIZE + CONTEXT_HEADER_SIZE <= size_of::<MContextPadding>());
};
