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

_skernel = VM_LOAD_ADDRESS;
_ekernel = __kernel_code_end;
