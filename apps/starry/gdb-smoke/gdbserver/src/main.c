#include <sys/auxv.h>
#include <stdio.h>

#if defined(__riscv)
#define RISCV_HWCAP_ISA_D (1UL << ('D' - 'A'))
#endif

__attribute__((noinline)) static int compute_value(void)
{
    return 40 + 2;
}

int main(void)
{
    unsigned long hwcap = getauxval(AT_HWCAP);
#if defined(__riscv)
    if ((hwcap & RISCV_HWCAP_ISA_D) == 0) {
        printf("gdbserver-smoke-target missing riscv D hwcap: %#lx\n", hwcap);
        return 1;
    }
#endif

    int value = compute_value();
    printf("gdbserver-smoke-target value=%d hwcap=%#lx\n", value, hwcap);
    return value == 42 ? 0 : 1;
}
