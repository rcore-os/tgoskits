#include <stdio.h>

#if defined(__x86_64__)

/* Linux exec replaces both the task-owned FPU image and the physical register
 * state. Control words survive ordinary integer startup code, so they expose
 * a stale pre-exec image without depending on compiler-owned XMM registers. */

#include <stdint.h>
#include <string.h>
#include <unistd.h>

#define X86_INIT_FCW 0x037fu
#define X86_INIT_MXCSR 0x1f80u
#define X86_PARENT_FCW 0x077fu
#define X86_PARENT_MXCSR 0x3f80u

static uint16_t read_fcw(void)
{
    uint16_t value;
    __asm__ volatile("fnstcw %0" : "=m"(value));
    return value;
}

static uint32_t read_mxcsr(void)
{
    uint32_t value;
    __asm__ volatile("stmxcsr %0" : "=m"(value));
    return value;
}

static void write_fcw(uint16_t value)
{
    __asm__ volatile("fldcw %0" : : "m"(value));
}

static void write_mxcsr(uint32_t value)
{
    __asm__ volatile("ldmxcsr %0" : : "m"(value));
}

static int run_exec_child(void)
{
    uint16_t fcw = read_fcw();
    uint32_t mxcsr = read_mxcsr();

    __asm__ volatile("fninit");
    uint32_t init_mxcsr = X86_INIT_MXCSR;
    write_mxcsr(init_mxcsr);

    if (fcw != X86_INIT_FCW || mxcsr != X86_INIT_MXCSR) {
        fprintf(stderr,
                "FAIL: exec inherited pre-exec FPU control state: "
                "fcw=%#x mxcsr=%#x expected_fcw=%#x expected_mxcsr=%#x\n",
                fcw,
                mxcsr,
                X86_INIT_FCW,
                X86_INIT_MXCSR);
        return 1;
    }

    puts("PASS: exec entered the new image with default x87 and MXCSR state");
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--exec-child") == 0) {
        return run_exec_child();
    }

    uint16_t parent_fcw = X86_PARENT_FCW;
    uint32_t parent_mxcsr = X86_PARENT_MXCSR;
    write_fcw(parent_fcw);
    write_mxcsr(parent_mxcsr);

    if (read_fcw() != parent_fcw || read_mxcsr() != parent_mxcsr) {
        fputs("FAIL: could not establish the pre-exec FPU control state\n", stderr);
        return 1;
    }

    char *const child_argv[] = {argv[0], "--exec-child", NULL};
    execv(argv[0], child_argv);
    perror("execv self");
    return 1;
}

#else

int main(void)
{
    puts("SKIP: x86 exec FPU reset test is x86_64-only");
    return 0;
}

#endif
