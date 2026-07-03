${ld_defaults}
VM_LOAD_ADDRESS = ${kernel_load_vaddr};

OUTPUT_ARCH(riscv)
ENTRY(_head)

SECTIONS
{
    . = VM_LOAD_ADDRESS;
    .head.text : {
        KEEP(*(.head.text))
        KEEP(*(.text._head))
    }
${ld_text}
${ld_rodata}
${ld_data}
${ld_rela_dyn}
    .dynsym : ALIGN(8) {
        *(.dynsym)
    }
    .gnu.hash : ALIGN(8) {
        *(.gnu.hash)
    }
    .hash : ALIGN(8) {
        *(.hash)
    }
    .dynstr : ALIGN(1) {
        *(.dynstr)
    }
    .eh_frame_hdr : ALIGN(4) {
        *(.eh_frame_hdr)
    }
${ld_bss}

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
