/*
 * Max available Page Size is 64K, so we set SectionAlignment
 * field of EFI application to 64K.
 */
PECOFF_FILE_ALIGN = 0x200;
VM_LOAD_ADDRESS = ${kernel_load_vaddr};
KERNEL_LOAD_ADDRESS = ${kernel_load_paddr};

${ld_defaults}

OUTPUT_ARCH(loongarch)
ENTRY(kernel_entry)
PHDRS {
	text PT_LOAD FLAGS(7);	/* RWX */
	tls PT_TLS FLAGS(4);	/* R__ */
	note PT_NOTE FLAGS(4);	/* R__ */
}

SECTIONS
{
    . = VM_LOAD_ADDRESS;
    .head.text :  {
        KEEP(*(.head.text))
    }
${ld_text}

    /* Exception vector table - must be 64KB aligned */
    /* VECSIZE = 0x200 (512 bytes), 128 vectors = 64KB total */
    . = ALIGN(0x10000);
    __exception_vectors = .;
    .exception.vectors : {
        KEEP(*(.exception.vectors))
    } :text
    . = __exception_vectors + 0x10000;
    __exception_vectors_end = .;

    . = ALIGN(PAGE_SIZE);
    .vectors : {
        KEEP(*(.vectors))
        KEEP(*(.vectors.*))
    } :text

${ld_rodata}
${ld_data}
${ld_rela_dyn}
    .relr.dyn : ALIGN(8) {
        __relr_dyn_begin = .;
        *(.relr.dyn)
        __relr_dyn_end = .;
    }
    .data.rel : ALIGN(8) {
        *(.data.rel*)
    }
    .la_abs : ALIGN(8) {
        __la_abs_begin = .;
        *(.la_abs)
        __la_abs_end = .;
    }
${ld_bss}

${ld_debug}
    _kernel_entry = ABSOLUTE(KERNEL_LOAD_ADDRESS + (kernel_entry - _head));
    _kernel_asize = ABSOLUTE(_end   - _head);
    _kernel_fsize = ABSOLUTE(_edata - _head);
    _kernel_vsize = ABSOLUTE(_end   - _etext);
    _kernel_rsize = ABSOLUTE(_edata - _etext);
    .gptab.sdata : {
        *(.gptab.data)
        *(.gptab.sdata)
    }
    .gptab.sbss : {
        *(.gptab.bss)
        *(.gptab.sbss)
    }
${ld_discard_exit}
${ld_discard_dynamic}
}
