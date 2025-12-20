/* Newer linkers prohibit VMA lower than image base (typically 0x20_0000), even
 * for NOLOAD sections. So we place the percpu section at a high VMA which does
 * not overlap with other sections. This address is chosen arbitrarily, but it's
 * okay as it's never actually used.
 *
 * This is ONLY necessary for test_percpu.x, normal kernels SHOULD place percpu
 * sections in their linker scripts with 0x0 VMA and no NOLOAD attribute as
 * before. See the linker script snippet used in ArceOS as an example:
 *
 *  . = ALIGN(4K);
 *  _percpu_start = .;
 *  _percpu_end = _percpu_start + SIZEOF(.percpu);
 *  .percpu 0x0 : AT(_percpu_start) {
 *      _percpu_load_start = .;
 *      *(.percpu .percpu.*)
 *      _percpu_load_end = .;
 *      . = _percpu_load_start + ALIGN(64) * 4;
 *  }
 *  . = _percpu_end;
 *
 */
PERCPU_LOAD = 0x2000000;
CPU_NUM = 4;

SECTIONS
{
    . = ALIGN(4K);
    _percpu_start = .;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    .percpu PERCPU_LOAD (NOLOAD) : AT(_percpu_start) {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        _percpu_load_end_aligned = ALIGN(64);
        . = _percpu_load_start + (_percpu_load_end_aligned - _percpu_load_start) * CPU_NUM;
    }
    . = _percpu_end;
}
INSERT AFTER .bss;
