/*
 * Max available Page Size is 64K, so we set SectionAlignment
 * field of EFI application to 64K.
 */
PECOFF_FILE_ALIGN = 0x200;

${ld_defaults}
VM_LOAD_ADDRESS = ${kernel_load_vaddr};

OUTPUT_ARCH(aarch64)
ENTRY(_head)
PHDRS {
	text PT_LOAD FLAGS(7);	/* RWX */
${tls_phdr}
	note PT_NOTE FLAGS(4);	/* R__ */
}

SECTIONS
{
    . = VM_LOAD_ADDRESS;
    .head.text :  {
        KEEP(*(.head.text))
    }
${ld_text}
${ld_rodata}
${ld_data}
${ld_rela_dyn}

    .init_array : ALIGN(8) {
        __init_array_start = .;
        KEEP(*(SORT_BY_INIT_PRIORITY(.init_array.*)))
        KEEP(*(.init_array))
        __init_array_end = .;
    }

${ld_bss}

${ld_debug}
    _kernel_entry = ABSOLUTE(kernel_entry & 0xffffffffffff);
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
