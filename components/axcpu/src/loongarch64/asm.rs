//! Wrapper functions for assembly instructions.

use core::arch::asm;

use ax_memory_addr::{PhysAddr, VirtAddr};
use loongArch64::register::{
    crmd,
    ecfg::{self, LineBasedInterrupt},
    eentry, pgdh, pgdl,
};

#[cfg(feature = "tls")]
use crate::KernelTlsBase;

/// Allows the current CPU to respond to interrupts.
#[inline]
pub fn enable_irqs() {
    crmd::set_ie(true)
}

/// Makes the current CPU to ignore interrupts.
#[inline]
pub fn disable_irqs() {
    crmd::set_ie(false)
}

/// Returns whether the current CPU is allowed to respond to interrupts.
#[inline]
pub fn irqs_enabled() -> bool {
    crmd::read().ie()
}

/// Enables or disables the local timer interrupt line.
#[inline]
pub fn set_timer_irq_enabled(enabled: bool) {
    set_local_irq_line_enabled(LineBasedInterrupt::TIMER, enabled)
}

/// Enables or disables a local interrupt line.
#[inline]
fn set_local_irq_line_enabled(line: LineBasedInterrupt, enabled: bool) {
    let current = ecfg::read().lie();
    let new_value = if enabled {
        current | line
    } else {
        current & !line
    };
    ecfg::set_lie(new_value);
}

/// Relaxes the current CPU and waits for interrupts.
///
/// It must be called with interrupts enabled, otherwise it will never return.
#[inline]
pub fn wait_for_irqs() {
    unsafe { asm!("idle 0", options(nomem, nostack)) }
}

/// Halt the current CPU.
#[inline]
pub fn halt() {
    disable_irqs();
    unsafe { loongArch64::asm::idle() }
}

/// Reads the current page table root register for user space (`PGDL`).
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_user_page_table() -> PhysAddr {
    PhysAddr::from(pgdl::read().base())
}

/// Reads the current page table root register for kernel space (`PGDH`).
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_kernel_page_table() -> PhysAddr {
    PhysAddr::from(pgdh::read().base())
}

/// Writes the register to update the current page table root for user space
/// (`PGDL`).
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
pub unsafe fn write_user_page_table(root_paddr: PhysAddr) {
    pgdl::set_base(root_paddr.as_usize() as _);
}

/// Writes the register to update the current page table root for kernel space
/// (`PGDH`).
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
pub unsafe fn write_kernel_page_table(root_paddr: PhysAddr) {
    pgdh::set_base(root_paddr.as_usize());
}

/// Flushes the entire instruction cache.
/// See <https://elixir.bootlin.com/linux/v6.6/source/arch/loongarch/mm/cache.c#L38>
#[inline]
pub fn flush_icache_all() {
    unsafe { asm!("ibar 0") };
}

/// Flushes the TLB.
///
/// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the TLB
/// entry that maps the given virtual address.
#[inline]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    unsafe {
        if let Some(vaddr) = vaddr {
            // <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#_dbar>
            //
            // Only after all previous load/store access operations are completely
            // executed, the DBAR 0 instruction can be executed; and only after the
            // execution of DBAR 0 is completed, all subsequent load/store access
            // operations can be executed.
            //
            // <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#_invtlb>
            //
            // formats: invtlb op, asid, addr
            //
            // op 0x5: Clear all page table entries with G=0 and ASID equal to the
            // register specified ASID, and VA equal to the register specified VA.
            //
            // When the operation indicated by op does not require an ASID, the
            // general register rj should be set to r0.
            asm!("dbar 0; invtlb 0x05, $r0, {reg}", reg = in(reg) vaddr.as_usize());
        } else {
            // op 0x0: Clear all page table entries
            asm!("dbar 0; invtlb 0x00, $r0, $r0");
        }
    }
}

/// Writes the Exception Entry Base Address register (`EENTRY`).
///
/// It also set the Exception Configuration register (`ECFG`) to `VS=0`.
///
/// - ECFG: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#exception-configuration>
/// - EENTRY: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#exception-entry-base-address>
///
/// # Safety
///
/// This function is unsafe as it changes the exception handling behavior of the
/// current CPU.
#[inline]
pub unsafe fn write_exception_entry_base(eentry: usize) {
    ecfg::set_vs(0);
    eentry::set_eentry(eentry);
}

/// Writes the Page Walk Controller registers (`PWCL` and `PWCH`).
///
/// # Safety
///
/// This function is unsafe as it changes the page walk configuration such as
/// levels and starting bits.
///
/// - `PWCL`: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#page-walk-controller-for-lower-half-address-space>
/// - `PWCH`: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#page-walk-controller-for-higher-half-address-space>
#[inline]
pub unsafe fn write_pwc(pwcl: u32, pwch: u32) {
    unsafe {
        asm!(
            include_asm_macros!(),
            "csrwr {}, LA_CSR_PWCL",
            "csrwr {}, LA_CSR_PWCH",
            in(reg) pwcl,
            in(reg) pwch
        )
    }
}

/// Reads the current kernel task's TLS base from `$tp`.
///
/// This register follows the execution context across CPUs. It is distinct
/// from the CPU-local base kept in `$r21`.
#[inline]
#[cfg(feature = "tls")]
pub fn read_thread_pointer() -> KernelTlsBase {
    let address;
    unsafe { asm!("move {}, $tp", out(reg) address) };
    KernelTlsBase::new(address)
}

/// Writes the current kernel task's TLS base to `$tp`.
///
/// This register follows the execution context across CPUs. It is distinct
/// from the CPU-local base kept in `$r21`.
///
/// # Safety
///
/// The caller must ensure `kernel_tls` belongs to the execution context that
/// is becoming current and that no Rust code observes a half-completed context
/// switch.
#[inline]
#[cfg(feature = "tls")]
pub unsafe fn write_thread_pointer(kernel_tls: KernelTlsBase) {
    unsafe { asm!("move $tp, {}", in(reg) kernel_tls.as_usize()) }
}

/// Enables floating-point instructions by setting `EUEN.FPE`.
///
/// - `EUEN`: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#extended-component-unit-enable>
#[inline]
pub fn enable_fp() {
    loongArch64::register::euen::set_fpe(true);
}

/// Enables LSX extension by setting `EUEN.LSX`.
///
/// - `EUEN`: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#extended-component-unit-enable>
pub fn enable_lsx() {
    loongArch64::register::euen::set_sxe(true);
}

/// Enables LASX extension by setting `EUEN.ASXE`.
///
/// - `EUEN`: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#extended-component-unit-enable>
pub fn enable_lasx() {
    loongArch64::register::euen::set_asxe(true);
}

#[cfg(feature = "uspace")]
core::arch::global_asm!(include_asm_macros!(), include_str!("user_copy.S"));

#[cfg(feature = "uspace")]
unsafe extern "C" {
    /// Copies data from source to destination, where addresses may be in user
    /// space. Equivalent to memcpy.
    ///
    /// # Safety
    /// This function is unsafe because it performs raw memory operations.
    ///
    /// # Returns
    /// Returns the number of bytes not copied. This means 0 indicates success,
    /// while a value > 0 indicates failure.
    pub fn user_copy(dst: *mut u8, src: *const u8, size: usize) -> usize;
}
