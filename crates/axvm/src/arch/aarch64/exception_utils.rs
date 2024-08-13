use tock_registers::interfaces::*;

#[inline(always)]
pub fn exception_esr() -> usize {
    cortex_a::registers::ESR_EL2.get() as usize
}

#[inline(always)]
pub fn exception_esr_el1() -> usize {
    cortex_a::registers::ESR_EL1.get() as usize
}

#[inline(always)]
pub fn exception_class() -> usize {
    (exception_esr() >> 26) & 0b111111
}

#[inline(always)]
fn exception_far() -> usize {
    cortex_a::registers::FAR_EL2.get() as usize
}

#[inline(always)]
fn exception_hpfar() -> usize {
    let hpfar: u64;
    unsafe {
        core::arch::asm!("mrs {}, HPFAR_EL2", out(reg) hpfar);
    }
    hpfar as usize
}

#[allow(non_upper_case_globals)]
const ESR_ELx_S1PTW_SHIFT: usize = 7;
#[allow(non_upper_case_globals)]
const ESR_ELx_S1PTW: usize = 1 << ESR_ELx_S1PTW_SHIFT;

macro_rules! arm_at {
    ($at_op:expr, $addr:expr) => {
        unsafe {
            core::arch::asm!(concat!("AT ", $at_op, ", {0}"), in(reg) $addr, options(nomem, nostack));
            core::arch::asm!("isb");
        }
    };
}

fn translate_far_to_hpfar(far: usize) -> Result<usize, ()> {
    /*
     * We have
     *	PAR[PA_Shift - 1 : 12] = PA[PA_Shift - 1 : 12]
     *	HPFAR[PA_Shift - 9 : 4]  = FIPA[PA_Shift - 1 : 12]
     */
    // #define PAR_TO_HPFAR(par) (((par) & GENMASK_ULL(PHYS_MASK_SHIFT - 1, 12)) >> 8)
    fn par_to_far(par: u64) -> u64 {
        let mask = ((1 << (52 - 12)) - 1) << 12;
        (par & mask) >> 8
    }

    use cortex_a::registers::PAR_EL1;

    let par = PAR_EL1.get();
    arm_at!("s1e1r", far);
    let tmp = PAR_EL1.get();
    PAR_EL1.set(par);
    if (tmp & PAR_EL1::F::TranslationAborted.value) != 0 {
        Err(())
    } else {
        Ok(par_to_far(tmp) as usize)
    }
}

// addr be ipa
#[inline(always)]
pub fn exception_fault_addr() -> usize {
    let far = exception_far();
    let hpfar =
        if (exception_esr() & ESR_ELx_S1PTW) == 0 && exception_data_abort_is_permission_fault() {
            translate_far_to_hpfar(far).unwrap_or_else(|_| {
                info!("error happen in translate_far_to_hpfar");
                0
            })
        } else {
            exception_hpfar()
        };
    (far & 0xfff) | (hpfar << 8)
}

/// return 1 means 32-bit instruction, 0 means 16-bit instruction
#[inline(always)]
fn exception_instruction_length() -> usize {
    (exception_esr() >> 25) & 1
}

#[inline(always)]
pub fn exception_next_instruction_step() -> usize {
    2 + 2 * exception_instruction_length()
}

#[inline(always)]
pub fn exception_iss() -> usize {
    exception_esr() & ((1 << 25) - 1)
}

#[inline(always)]
pub fn exception_data_abort_handleable() -> bool {
    (!(exception_iss() & (1 << 10)) | (exception_iss() & (1 << 24))) != 0
}

#[inline(always)]
pub fn exception_data_abort_is_translate_fault() -> bool {
    (exception_iss() & 0b111111 & (0xf << 2)) == 4
}

#[inline(always)]
pub fn exception_data_abort_is_permission_fault() -> bool {
    (exception_iss() & 0b111111 & (0xf << 2)) == 12
}

#[inline(always)]
pub fn exception_data_abort_access_width() -> usize {
    1 << ((exception_iss() >> 22) & 0b11)
}

#[inline(always)]
pub fn exception_data_abort_access_is_write() -> bool {
    (exception_iss() & (1 << 6)) != 0
}

#[inline(always)]
pub fn exception_data_abort_access_in_stage2() -> bool {
    (exception_iss() & (1 << 7)) != 0
}

#[inline(always)]
pub fn exception_data_abort_access_reg() -> usize {
    (exception_iss() >> 16) & 0b11111
}

#[inline(always)]
pub fn exception_data_abort_access_reg_width() -> usize {
    4 + 4 * ((exception_iss() >> 15) & 1)
}

#[inline(always)]
pub fn exception_data_abort_access_is_sign_ext() -> bool {
    ((exception_iss() >> 21) & 1) != 0
}

macro_rules! save_regs_to_stack {
    () => {
        "
        sub     sp, sp, 34 * 8
        stp     x0, x1, [sp]
        stp     x2, x3, [sp, 2 * 8]
        stp     x4, x5, [sp, 4 * 8]
        stp     x6, x7, [sp, 6 * 8]
        stp     x8, x9, [sp, 8 * 8]
        stp     x10, x11, [sp, 10 * 8]
        stp     x12, x13, [sp, 12 * 8]
        stp     x14, x15, [sp, 14 * 8]
        stp     x16, x17, [sp, 16 * 8]
        stp     x18, x19, [sp, 18 * 8]
        stp     x20, x21, [sp, 20 * 8]
        stp     x22, x23, [sp, 22 * 8]
        stp     x24, x25, [sp, 24 * 8]
        stp     x26, x27, [sp, 26 * 8]
        stp     x28, x29, [sp, 28 * 8]

        mov     x1, sp
        add     x1, x1, #(0x110)
        stp     x30, x1, [sp, 30 * 8]
        mrs     x10, elr_el2
        mrs     x11, spsr_el2
        stp     x10, x11, [sp, 32 * 8]

        add    sp, sp, 34 * 8"
    };
}

macro_rules! restore_regs_to_stack {
    () => {
        "
        sub     sp, sp, 34 * 8

        ldp     x10, x11, [sp, 32 * 8]
        msr     elr_el2, x10
        msr     spsr_el2, x11

        ldr     x30,      [sp, 30 * 8]
        ldp     x28, x29, [sp, 28 * 8]
        ldp     x26, x27, [sp, 26 * 8]
        ldp     x24, x25, [sp, 24 * 8]
        ldp     x22, x23, [sp, 22 * 8]
        ldp     x20, x21, [sp, 20 * 8]
        ldp     x18, x19, [sp, 18 * 8]
        ldp     x16, x17, [sp, 16 * 8]
        ldp     x14, x15, [sp, 14 * 8]
        ldp     x12, x13, [sp, 12 * 8]
        ldp     x10, x11, [sp, 10 * 8]
        ldp     x8, x9, [sp, 8 * 8]
        ldp     x6, x7, [sp, 6 * 8]
        ldp     x4, x5, [sp, 4 * 8]
        ldp     x2, x3, [sp, 2 * 8]
        ldp     x0, x1, [sp]

        add     sp, sp, 34 * 8"
    };
}
