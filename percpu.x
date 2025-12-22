CPU_NUM = 4;

SECTIONS
{
    . = ALIGN(4K);
    _percpu_start = .;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    .percpu (NOLOAD) : AT(_percpu_start) {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        _percpu_load_end_aligned = ALIGN(64);
        . = _percpu_load_start + (_percpu_load_end_aligned - _percpu_load_start) * CPU_NUM;
    }
    . = _percpu_end;
}
INSERT AFTER .bss;