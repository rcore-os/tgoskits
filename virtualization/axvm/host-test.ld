PROVIDE(__CPU_LOCAL_TSS_OFFSET = 0);

SECTIONS
{
    __ex_table : ALIGN(4) {
        PROVIDE(_ex_table_start = .);
        *(__ex_table)
        PROVIDE(_ex_table_end = .);
    }

    __nofault_ex_table : ALIGN(4) {
        PROVIDE(_nofault_ex_table_start = .);
        *(__nofault_ex_table)
        PROVIDE(_nofault_ex_table_end = .);
    }
}
INSERT AFTER .data;
