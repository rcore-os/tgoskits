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
        __dyndbg_start = .;
        KEEP(*(__dyndbg))
        __dyndbg_end = .;
    }
} INSERT AFTER .data;
