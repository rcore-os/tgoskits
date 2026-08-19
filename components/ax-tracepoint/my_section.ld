SECTIONS {
    .tracepoint :
    {
        __start_tracepoint = .;
        KEEP(*(.tracepoint))
        __stop_tracepoint = .;
    }
}
INSERT AFTER .data;
