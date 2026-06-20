INCLUDE "runtime.x"

PROVIDE(_ex_table_start = 0);
PROVIDE(_ex_table_end = 0);

SECTIONS {
    .static_keys : {
        __start___static_keys = .;
        KEEP(*(__static_keys))
        __stop___static_keys = .;
    }

    .dyndbg : {
        __start___dyndbg = .;
        KEEP(*(__dyndbg))
        __stop___dyndbg = .;
    }

    .tracepoint : {
        __start_tracepoint = .;
        KEEP(*(.tracepoint .tracepoint.*))
        __stop_tracepoint = .;
    }

    .kmod_export : {
        __start_kmod_export = .;
        KEEP(*(.kmod_export .kmod_export.*))
        __stop_kmod_export = .;
    }

    .kallsyms : ALIGN(4K) {
        __kallsyms_start = .;
        . += __STARRY_KALLSYMS_RESERVED__; /* reserve space for kallsyms, can be recycled */
        __kallsyms_end = .;
    }
}

INSERT AFTER .data;
