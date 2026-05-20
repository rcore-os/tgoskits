/* 
 * Test linker script that works for both architectures
 * The actual OUTPUT_ARCH will be determined by the target
 */

INCLUDE "someboot.x"

PROVIDE(__someboot_secondary = __somehal_secondary_default);
PROVIDE(__somehal_secondary = __somehal_secondary_default);

SECTIONS
{
   .driver : ALIGN(4K) {
        __sdriver_register = .;
        KEEP(*(.driver.register*))
        __edriver_register = .;
    }
} INSERT AFTER .data;
