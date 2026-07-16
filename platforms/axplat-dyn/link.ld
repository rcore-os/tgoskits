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
_percpu_load_end = __percpu_end;
/*
 * x86_64 GS names the runtime CpuAreaHeader, so trap assembly must consume a
 * template-relative TSS offset even when the linked .percpu VMA is nonzero.
 */
PROVIDE(__PERCPU_TSS_OFFSET = __PERCPU_TSS - _percpu_load_start);

_skernel = VM_LOAD_ADDRESS;
_ekernel = __kernel_code_end;
