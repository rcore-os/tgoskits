# riscv64 static-pie segfault

## Reproduction

- Minimal C program (printf only)
- Built with `riscv64-linux-musl-gcc -static-pie`
- ELF Type: DYN (Position-Independent Executable)
- QEMU on StarryOS riscv64
- Result: segfault, RC=139

```c
#include <stdio.h>
int main(void) {
    printf("static-pie test OK\n");
    return 0;
}
```

## Crash

```
pc(sepc)=0x00000000000003b0
Segmentation fault (core dumped)
```

## Root cause

PC=0x3b0 is the start of `.plt` section, NOT `.text` (0x3f0).

The binary has unresolved PLT relocations (from `readelf -r`):

- `.rela.plt`: `R_RISCV_JUMP_SLOT` for `puts` and `__libc_start_main`
- `.rela.dyn`: `R_RISCV_RELATIVE` entries for data pointers

The StarryOS ELF loader (`os/StarryOS/kernel/src/mm/loader.rs`) maps segments
and jumps to entry, but does NOT process `.rela.dyn` or `.rela.plt` relocations.
The unresolved PLT entries cause a jump to 0x3b0 (PLT stub) which segfaults.

aarch64/x86_64 static-pie works because their musl CRT handles relocations
differently, or their PLT stubs happen to work without relocation processing.

## Conclusion

Not a llama.cpp issue. The ELF loader needs relocation processing for
static-PIE (Type=DYN) binaries. The fix requires:
1. Parsing `.rela.dyn` and `.rela.plt` sections
2. Applying R_RISCV_RELATIVE and R_RISCV_JUMP_SLOT relocations
3. Then jumping to the entry point

This is a kernel loader enhancement, tracked separately.
