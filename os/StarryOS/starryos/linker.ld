INCLUDE "runtime.x"

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

    .kallsyms : ALIGN(4K) {
        __kallsyms_start = .;
        . += 8M; /* reserve space for kallsyms, can be recycled */
        __kallsyms_end = .;
    }
}

INSERT AFTER .data;
