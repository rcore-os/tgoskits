SECTIONS
{
    . = ALIGN(4K);
    .percpu : {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
    }
}
INSERT AFTER .data;
