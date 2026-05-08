OUTPUT_ARCH(%ARCH%)

BASE_ADDRESS = %KERNEL_BASE_VADDR%;
PHYS_VIRT_OFFSET = %PHYS_VIRT_OFFSET%;
PHYS_BASE_ADDRESS = %KERNEL_BASE_PADDR%;
CPU_NUM = %CPU_NUM%;

EXTERN(_start)
/*
 * QEMU LoongArch direct kernel boot keeps ELF e_entry as-is, so it must point
 * at the low physical entry even though the linked VMA lives in the higher
 * half.
 */
ENTRY(%ENTRY_PADDR%)

SECTIONS
{
    . = BASE_ADDRESS;
    _skernel = .;

    _stext = .;
    .text : AT(ADDR(.text) - PHYS_VIRT_OFFSET) ALIGN(4K) {
        /*
         * The axplat LoongArch boot stub starts with a 64-byte Linux/EFI
         * boot header. Keep `.text.boot`, but make the ELF entry point land on
         * the first real instruction after the header.
         */
        KEEP(*(.text.boot))
        _start_actual = _start + 0x40;
        *(.text .text.*)
        . = ALIGN(4K);
    }
    _etext = .;

    _srodata = .;
    .rodata : AT(ADDR(.rodata) - PHYS_VIRT_OFFSET) ALIGN(4K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }
    _erodata = .;

    . = ALIGN(4K);
    .data : AT(ADDR(.data) - PHYS_VIRT_OFFSET) ALIGN(4K) {
        _sdata = .;
        *(.data .data.*)
        *(.sdata .sdata.*)

        __sdriver_register = .;
        KEEP(*(.driver.register*))
        __edriver_register = .;
    }

    /*
     * Trap handler slices emitted by `linkme` must live in the regular
     * higher-half image. If left orphaned, ld can place them at VMA 0, which
     * makes the trap dispatcher load a NULL handler pointer from the low
     * linear map and jump to 0x0 once IRQs are enabled.
     */
    . = ALIGN(8);
    .linkme : AT(ADDR(.linkme) - PHYS_VIRT_OFFSET) {
        __start_linkme_PAGE_FAULT = .;
        KEEP(*(linkme_PAGE_FAULT))
        __stop_linkme_PAGE_FAULT = .;
        __start_linkme_IRQ = .;
        KEEP(*(linkme_IRQ))
        __stop_linkme_IRQ = .;
    }

    .tdata : AT(ADDR(.tdata) - PHYS_VIRT_OFFSET) ALIGN(0x10) {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    }

    .tbss : AT(ADDR(.tbss) - PHYS_VIRT_OFFSET) ALIGN(0x10) {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.tcommon)
        _etbss = .;
    }

    . = ALIGN(64);
    /*
     * QEMU LoongArch direct boot loads ELF PT_LOAD segments by translating the
     * segment VMA through `cpu_loongarch_virt_to_phys()`, not by using
     * `p_paddr`. Therefore `.percpu` must keep a higher-half VMA here;
     * otherwise a VMA of 0 would be copied to physical 0x0 instead of the
     * runtime percpu area at `_percpu_start`.
     */
    .percpu : AT(ADDR(.percpu) - PHYS_VIRT_OFFSET) {
        _percpu_start = .;
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        _percpu_load_end_aligned = ALIGN(64);
        . = _percpu_start
          + (_percpu_load_end_aligned - _percpu_load_start) * CPU_NUM;
    }
    _percpu_end = .;

    . = ALIGN(4K);
    _edata = .;

    _sbss = .;
    .bss : AT(ADDR(.bss) - PHYS_VIRT_OFFSET) ALIGN(4K) {
        boot_stack = .;
        *(.bss.stack)
        . = ALIGN(4K);
        boot_stack_top = .;

        _sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(4K);
        _ebss = .;
    }

    _ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
        *(.note)
        *(.note.gnu.build-id)
    }
}
