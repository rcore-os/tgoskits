#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#if !defined(__riscv) && !defined(__x86_64__)
int main(void)
{
    puts("test-clone-fp-state is supported only on riscv64 and x86_64");
    return 0;
}
#else

static int pass_count;
static int fail_count;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        pass_count++;                                                   \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        fail_count++;                                                   \
    }                                                                   \
} while (0)

#ifdef __riscv

static long raw_clone_sigchld(void)
{
    register long a0 asm("a0") = SIGCHLD;
    register long a1 asm("a1") = 0;
    register long a2 asm("a2") = 0;
    register long a3 asm("a3") = 0;
    register long a4 asm("a4") = 0;
    register long a7 asm("a7") = SYS_clone;

    asm volatile(
        "ecall"
        : "+r"(a0)
        : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7)
        : "memory");

    return a0;
}

int main(void)
{
    puts("================================================");
    puts("  TEST: riscv64 clone inherits floating-point state");
    printf("  FILE: %s\n", __FILE__);
    puts("================================================");

    const double expected = 8192.5;
    asm volatile("fld ft0, %0" :: "m"(expected) : "ft0", "memory");

    long pid = raw_clone_sigchld();
    if (pid == 0) {
        double observed = 0.0;
        asm volatile("fsd ft0, %0" : "=m"(observed) :: "memory");
        _exit(observed == expected ? 0 : 77);
    }

    CHECK(pid > 0, "raw clone(SIGCHLD, NULL) creates a child");
    if (pid > 0) {
        int status = 0;
        pid_t waited = waitpid((pid_t)pid, &status, 0);
        CHECK(waited == (pid_t)pid, "waitpid returns cloned child");
        CHECK(WIFEXITED(status), "cloned child exits normally");
        if (WIFEXITED(status)) {
            CHECK(WEXITSTATUS(status) == 0,
                  "child observes inherited ft0 value at clone return");
        }
    }

    puts("------------------------------------------------");
    printf("  DONE: %d pass, %d fail\n", pass_count, fail_count);
    puts("================================================");
    return fail_count > 0 ? 1 : 0;
}

#else

struct x86_fp_observation {
    uint16_t fcw;
    uint32_t mxcsr;
};

static long raw_clone_sigchld(void)
{
    register long rax asm("rax") = SYS_clone;
    register long rdi asm("rdi") = SIGCHLD;
    register long rsi asm("rsi") = 0;
    register long rdx asm("rdx") = 0;
    register long r10 asm("r10") = 0;
    register long r8 asm("r8") = 0;

    asm volatile(
        "syscall"
        : "+r"(rax)
        : "r"(rdi), "r"(rsi), "r"(rdx), "r"(r10), "r"(r8)
        : "rcx", "r11", "memory");

    return rax;
}

__attribute__((noreturn)) static void raw_exit(int status)
{
    register long rax asm("rax") = SYS_exit;
    register long rdi asm("rdi") = status;

    asm volatile("syscall" :: "r"(rax), "r"(rdi) : "rcx", "r11", "memory");
    __builtin_unreachable();
}

int main(void)
{
    puts("================================================");
    puts("  TEST: x86_64 clone inherits floating-point state");
    printf("  FILE: %s\n", __FILE__);
    puts("================================================");

    struct x86_fp_observation *observed = mmap(
        NULL, sizeof(*observed), PROT_READ | PROT_WRITE,
        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(observed != MAP_FAILED, "shared clone observation mapping is available");
    if (observed == MAP_FAILED) {
        return 1;
    }

    const uint16_t expected_fcw = 0x077f;
    const uint32_t expected_mxcsr = 0x3f80;
    uint16_t original_fcw;
    uint32_t original_mxcsr;
    asm volatile(
        "fnstcw %0\n\t"
        "stmxcsr %1\n\t"
        "fldcw %2\n\t"
        "ldmxcsr %3"
        : "=m"(original_fcw), "=m"(original_mxcsr)
        : "m"(expected_fcw), "m"(expected_mxcsr)
        : "memory");

    long pid = raw_clone_sigchld();
    if (pid == 0) {
        asm volatile(
            "fnstcw %0\n\t"
            "stmxcsr %1"
            : "=m"(observed->fcw), "=m"(observed->mxcsr)
            :
            : "memory");
        raw_exit(observed->fcw == expected_fcw &&
                         observed->mxcsr == expected_mxcsr
                     ? 0
                     : 77);
    }

    asm volatile(
        "fldcw %0\n\t"
        "ldmxcsr %1"
        :
        : "m"(original_fcw), "m"(original_mxcsr)
        : "memory");

    CHECK(pid > 0, "raw clone(SIGCHLD, NULL) creates a child");
    if (pid > 0) {
        int status = 0;
        pid_t waited = waitpid((pid_t)pid, &status, 0);
        CHECK(waited == (pid_t)pid, "waitpid returns cloned child");
        CHECK(WIFEXITED(status), "cloned child exits normally");
        if (WIFEXITED(status)) {
            CHECK(WEXITSTATUS(status) == 0,
                  "child observes inherited x87 control and MXCSR state");
            if (WEXITSTATUS(status) != 0) {
                printf("  child state: fcw=%#x mxcsr=%#x; expected fcw=%#x mxcsr=%#x\n",
                       observed->fcw, observed->mxcsr, expected_fcw,
                       expected_mxcsr);
            }
        }
    }

    CHECK(munmap(observed, sizeof(*observed)) == 0,
          "shared clone observation mapping is released");
    puts("------------------------------------------------");
    printf("  DONE: %d pass, %d fail\n", pass_count, fail_count);
    puts("================================================");
    return fail_count > 0 ? 1 : 0;
}

#endif
#endif
