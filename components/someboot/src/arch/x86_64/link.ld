PECOFF_FILE_ALIGN = 0x200;

PROVIDE(PAGE_SIZE = 0x1000);
PROVIDE(STACK_SIZE = 0x4000);
VM_LOAD_ADDRESS = ${kernel_load_vaddr};

OUTPUT_ARCH(i386:x86-64)
ENTRY(_head)
PHDRS {
    text PT_LOAD FLAGS(7);
    note PT_NOTE FLAGS(4);
}

SECTIONS
{
    . = VM_LOAD_ADDRESS;
    .head.text : {
        KEEP(*(.head.text))
    }
    . = ALIGN(PAGE_SIZE);
    _stext = .;
    .text : {
        *(.text .text.*)
        *(.init.text)
        *(.fixup)
        *(.gnu.warning)
    } :text = 0
    . = ALIGN(PAGE_SIZE);
    _etext = .;
    _srodata = .;
    .rodata : {
        *(.rodata)
        *(.rodata.*)
        *(.data.rel.ro*)
    }
    . = ALIGN(PAGE_SIZE);
    _erodata = .;
    _sdata = .;

    .data : ALIGN(16) {
        *(.data .data.*)
        *(.init.data)
        *(.sdata .sdata.*)

        . = ALIGN(PAGE_SIZE);
        __percpu_start = .;
        *(.percpu_data .percpu_data.* .percpu .percpu.*)
        __percpu_end = .;
    }
    .rela.dyn : ALIGN(8) {
        __rela_dyn_begin = .;
        *(.rela .rela*)
        __rela_dyn_end = .;
    }
    .sdata : ALIGN(8) {
        *(.sdata)
        . = ALIGN(PAGE_SIZE);
    }
    _edata = .;
    __kernel_load_end = .;
    __bss_start = .;

    .tdata : {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    }

    .tbss : {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.init.bss)
        *(.tcommon)
        _etbss = .;
    }

    .sbss : {
        *(.dynsbss) *(.sbss) *(.scommon)
    }
    .bss : {
        *(.dynbss)
        *(.bss .bss.*)
        *(COMMON)

        . = ALIGN(PAGE_SIZE);
        __cpu0_stack = .;
        . += STACK_SIZE;
        __cpu0_stack_top = .;
    }
    . = ALIGN(PAGE_SIZE);
    __bss_stop = .;
    _end = .;
    __kernel_code_end = .;

    .stab 0 : { *(.stab) } .stabstr 0 : { *(.stabstr) } .stab.excl 0 : { *(.stab.excl) } .stab.exclstr 0 : { *(.stab.exclstr) } .stab.index 0 : { *(.stab.index) } .stab.indexstr 0 : { *(.stab.indexstr) }
    .debug 0 : { *(.debug) } .line 0 : { *(.line) } .debug_srcinfo 0 : { *(.debug_srcinfo) } .debug_sfnames 0 : { *(.debug_sfnames) } .debug_aranges 0 : { *(.debug_aranges) } .debug_pubnames 0 : { *(.debug_pubnames) } .debug_info 0 : { *(.debug_info .gnu.linkonce.wi.*) } .debug_abbrev 0 : { *(.debug_abbrev) } .debug_line 0 : { *(.debug_line) } .debug_frame 0 : { *(.debug_frame) } .debug_str 0 : { *(.debug_str) } .debug_loc 0 : { *(.debug_loc) } .debug_macinfo 0 : { *(.debug_macinfo) } .debug_pubtypes 0 : { *(.debug_pubtypes) } .debug_ranges 0 : { *(.debug_ranges) } .debug_weaknames 0 : { *(.debug_weaknames) } .debug_funcnames 0 : { *(.debug_funcnames) } .debug_typenames 0 : { *(.debug_typenames) } .debug_varnames 0 : { *(.debug_varnames) } .debug_gnu_pubnames 0 : { *(.debug_gnu_pubnames) } .debug_gnu_pubtypes 0 : { *(.debug_gnu_pubtypes) } .debug_types 0 : { *(.debug_types) } .debug_addr 0 : { *(.debug_addr) } .debug_line_str 0 : { *(.debug_line_str) } .debug_loclists 0 : { *(.debug_loclists) } .debug_macro 0 : { *(.debug_macro) } .debug_names 0 : { *(.debug_names) } .debug_rnglists 0 : { *(.debug_rnglists) } .debug_str_offsets 0 : { *(.debug_str_offsets) }
    .comment 0 : { *(.comment) } .symtab 0 : { *(.symtab) } .strtab 0 : { *(.strtab) } .shstrtab 0 : { *(.shstrtab) }
    _kernel_entry = ABSOLUTE(kernel_entry);
    _kernel_asize = ABSOLUTE(_end - _head);
    _kernel_fsize = ABSOLUTE(_edata - _head);
    _kernel_vsize = ABSOLUTE(_end - _etext);
    _kernel_rsize = ABSOLUTE(_edata - _etext);
    _kernel_code_size = ABSOLUTE(_etext - _stext);
    _kernel_bss_size = ABSOLUTE(_kernel_vsize - _kernel_rsize);
    _kernel_text_offset = ABSOLUTE(_stext - _head);
    _kernel_image_size = ABSOLUTE(_end - _head);
    /DISCARD/ : {
        *(.exit.text)
        *(.text.exit)
        *(.exit.data .exit.data.*)
        *(.fini_array .fini_array.*)
        *(.dtors .dtors.*)
        *(.exitcall.exit)
        *(.discard)
        *(.discard.*)
        *(.export_symbol)
        *(.no_trim_symbol)
        *(.modinfo)
        *(.gnu.version*)
    }
    /DISCARD/ : {
        *(.dynamic .dynsym .dynstr .hash .gnu.hash)
        *(.gnu.attributes)
        *(.eh_frame)
    }
}
