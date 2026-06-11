/*
 * Test linker script that works for both architectures
 * The actual OUTPUT_ARCH will be determined by the target
 */

INCLUDE "link.x"
__SMP = {{SMP}};
AX_LINKER_LOAD_OFFSET = 0;

PROVIDE(_sbss = __bss_start);
PROVIDE(_ebss = __bss_stop);
PROVIDE(boot_stack = __cpu0_stack);
PROVIDE(boot_stack_top = __cpu0_stack_top);
_percpu_load_start = __percpu_start;
/*
 * ax-cpu x86_64 trap assembly uses the historical __PERCPU_TSS symbol as a
 * GS-relative offset. ax-percpu/custom-base exports the per-CPU object as TSS,
 * so provide the expected offset without forcing the .percpu VMA to zero.
 */
PROVIDE(__PERCPU_TSS = TSS - _percpu_load_start);

_skernel = VM_LOAD_ADDRESS;
_ekernel = __kernel_code_end;
