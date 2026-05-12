#[macro_use]
mod _macros;

mod addrspace;
mod cache;
mod console;
mod context;
pub(crate) mod entry;
mod head;
pub(crate) mod irq;
mod paging;
pub(crate) mod pte;
mod register;
mod relocate;
mod trap;

use core::{hint::spin_loop, ptr::null};

pub(crate) use entry::_secondary_entry;
use loongArch64::{
    register::*,
    time::{Time, get_timer_freq},
};
pub use paging::Entry as Pte;
pub use relocate::relocate;

use crate::{ArchTrait, DCacheOp, efi_stub, irq::IrqId, power::CpuOnError};

const MIN_TICKS: usize = 4;

pub struct Arch;

impl ArchTrait for Arch {
    type P = paging::Generic;
    type Console = console::Console;

    fn _va(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PAGE_OFFSET) as *mut u8
    }

    fn _io(paddr: usize) -> *mut u8 {
        (paddr + addrspace::IO_BASE) as *mut u8
    }

    fn post_allocator() {}

    fn per_cpu_trap_init(is_primary: bool) {
        trap::per_cpu_trap_init(is_primary);
    }

    fn systimer_enable() {
        tcfg::set_en(true);
    }

    fn systimer_irq_enable() {
        tcfg::set_en(true);
    }

    fn systimer_irq_disable() {
        tcfg::set_en(false);
    }

    fn systimer_irq_is_enabled() -> bool {
        tcfg::read().en()
    }
    fn systimer_set_interval(ticks: usize) {
        let ticks = ticks.max(MIN_TICKS);
        // Ensure the value is aligned to a multiple of 4 as required by TCFG
        let ticks = (ticks + 3) & !3;

        // 先禁用定时器
        tcfg::set_en(false);
        // 设置单次模式
        tcfg::set_periodic(false);
        // 设置初始值
        tcfg::set_init_val(ticks);
        // 清除可能存在的中断
        ticlr::clear_timer_interrupt();
        // 不在这里 enable，让调用者通过 systimer_enable() 来使能
    }

    fn systimer_ack() {
        ticlr::clear_timer_interrupt();
    }

    fn systimer_freq() -> usize {
        get_timer_freq()
    }

    fn systimer_tick() -> usize {
        Time::read()
    }

    fn shutdown() -> ! {
        if efi_stub::is_uefi_available() {
            efi_stub::reset(
                efi_stub::ResetType::SHUTDOWN,
                efi_stub::Status::SUCCESS,
                None,
            );
        }

        loop {
            spin_loop();
        }
    }

    fn secondary_entry_fn_address() -> *const () {
        _secondary_entry as *const ()
    }

    fn irq_all_is_enabled() -> bool {
        crmd::read().ie()
    }

    fn irq_all_set_enable(enable: bool) {
        crmd::set_ie(enable);
    }

    fn irq_is_enabled(irq: IrqId) -> bool {
        use loongArch64::register::ecfg::{self, LineBasedInterrupt};

        match irq.kind() {
            trap::IrqKind::Private(hwirq) => {
                // 对于 CPU 本地中断，检查 ECFG.LIE 对应位
                // ECFG.LIE 位 0-12 对应中断 0-12 (SWI0-1, HWI0-7, PCOV, TI, IPI)
                let lie = ecfg::read().lie();
                let mask = LineBasedInterrupt::from_bits_retain(1 << hwirq);
                lie.contains(mask)
            }
            trap::IrqKind::External(_hwirq) => {
                // 外部中断需要通过级联中断控制器来检查
                // 目前暂不支持，返回 false
                false
            }
        }
    }

    fn irq_set_enable(irq: IrqId, enable: bool) {
        use loongArch64::register::ecfg::{self, LineBasedInterrupt};

        match irq.kind() {
            trap::IrqKind::Private(hwirq) => {
                // 对于 CPU 本地中断，设置 ECFG.LIE 对应位
                // 参考 Linux: set_csr_ecfg(ECFGF(d->hwirq)) / clear_csr_ecfg(ECFGF(d->hwirq))
                let current_lie = ecfg::read().lie();
                let mask = LineBasedInterrupt::from_bits_retain(1 << hwirq);
                let new_lie = if enable {
                    current_lie | mask
                } else {
                    current_lie - mask
                };
                ecfg::set_lie(new_lie);
            }
            trap::IrqKind::External(_hwirq) => {
                // 外部中断需要通过级联中断控制器来设置
                // 目前暂不支持
            }
        }
    }

    fn kernel_page_table() -> crate::mem::PageTableInfo {
        use crate::mem::PageTableInfo;

        PageTableInfo {
            addr: pgdh::read().base(),
            asid: asid::read().asid(),
        }
    }

    fn set_kernel_page_table(val: crate::mem::PageTableInfo) {
        // 设置内核页表基地址到 PGDH (高地址空间)
        pgdh::set_base(val.addr);
        // 设置 ASID
        asid::set_asid(val.asid);
        // 刷新 TLB
        paging::local_flush_tlb_all();
        // 添加指令同步屏障,确保 TLB 刷新生效
        unsafe {
            core::arch::asm!("dbar 0", options(nomem, nostack));
            core::arch::asm!("ibar 0", options(nomem, nostack));
        }
    }

    fn virt_to_phys(vaddr: *const u8) -> usize {
        addrspace::to_phys(vaddr as usize)
    }

    #[cfg(uspace)]
    fn user_page_table() -> crate::mem::PageTableInfo {
        crate::mem::PageTableInfo {
            addr: pgdl::read().base(),
            asid: asid::read().asid(),
        }
    }

    #[cfg(uspace)]
    fn set_user_page_table(val: crate::mem::PageTableInfo) {
        // 设置用户页表基地址到 PGDL (低地址空间)
        pgdl::set_base(val.addr);
        // 设置 ASID
        asid::set_asid(val.asid);
        // 刷新 TLB
        paging::local_flush_tlb_all();
        // 添加指令同步屏障
        unsafe {
            core::arch::asm!("dbar 0", options(nomem, nostack));
            core::arch::asm!("ibar 0", options(nomem, nostack));
        }
    }

    fn trap_addr() -> usize {
        eentry::read().eentry() as usize
    }

    fn jump_to(entry: usize, sp: usize) -> ! {
        unsafe {
            core::arch::asm!(
                "move $sp, {sp}",
                "jr {entry}",
                sp = in(reg) sp,
                entry = in(reg) entry,
                options(noreturn)
            );
        }
    }

    fn cpu_current_hartid() -> usize {
        cpuid::read().core_id()
    }

    fn kernel_space() -> core::ops::Range<usize> {
        0xFFFF_0000_0000_0000..usize::MAX
    }

    fn cpu_on(_hartid: usize, _entry: usize, _arg: usize) -> Result<(), CpuOnError> {
        Err(CpuOnError::NotSupported)
    }

    fn dcache_range(_op: DCacheOp, _addr: usize, _size: usize) {
        unsafe {
            core::arch::asm!("dbar 0", options(nomem, nostack));
        }
    }

    // Safety: `system_table` originates from the EFI entry path and follows
    // the `ArchTrait::efi_enter_kernel` contract.
    unsafe fn efi_enter_kernel(system_table: *const ::core::ffi::c_void) -> bool {
        unsafe { crate::arch::entry::kernel_entry(1, null(), system_table) };
        unreachable!()
    }
}
