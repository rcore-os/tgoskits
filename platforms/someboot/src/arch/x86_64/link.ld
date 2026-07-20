PECOFF_FILE_ALIGN = 0x200;

${ld_defaults}
VM_LOAD_ADDRESS = ${kernel_load_vaddr};

OUTPUT_ARCH(i386:x86-64)
ENTRY(_head)
PHDRS {
    text PT_LOAD FLAGS(7);
    note PT_NOTE FLAGS(4);
    tls PT_TLS FLAGS(4);
}

SECTIONS
{
    . = VM_LOAD_ADDRESS;
    .head.text : {
        KEEP(*(.head.text))
    }
${ld_text}
${ld_rodata}
${ld_data}
${ld_rela_dyn}
${ld_bss}

${ld_debug}
    _kernel_entry = ABSOLUTE(kernel_entry);
    _kernel_asize = ABSOLUTE(_end - _head);
    _kernel_fsize = ABSOLUTE(_edata - _head);
    _kernel_vsize = ABSOLUTE(_end - _etext);
    _kernel_rsize = ABSOLUTE(_edata - _etext);
    _kernel_code_size = ABSOLUTE(_etext - _stext);
    _kernel_bss_size = ABSOLUTE(_kernel_vsize - _kernel_rsize);
    _kernel_text_offset = ABSOLUTE(_stext - _head);
    _kernel_image_size = ABSOLUTE(_end - _head);
${ld_discard_exit}
${ld_discard_dynamic}
}
