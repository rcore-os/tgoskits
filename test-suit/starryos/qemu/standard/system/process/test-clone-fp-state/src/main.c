#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

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

static long raw_clone_sigchld(void)
{
#if defined(__riscv)
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
#elif defined(__x86_64__)
    register long r10 asm("r10") = 0;
    register long r8 asm("r8") = 0;
    long result;
    asm volatile("syscall" : "=a"(result)
                 : "a"(SYS_clone), "D"((long)SIGCHLD), "S"(0L), "d"(0L), "r"(r10), "r"(r8)
                 : "rcx", "r11", "memory");
    return result;
#elif defined(__aarch64__)
    register long x0 asm("x0") = SIGCHLD;
    register long x1 asm("x1") = 0;
    register long x2 asm("x2") = 0;
    register long x3 asm("x3") = 0;
    register long x4 asm("x4") = 0;
    register long x8 asm("x8") = SYS_clone;
    asm volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8) : "memory");
    return x0;
#elif defined(__loongarch64)
    register long a0 asm("$a0") = SIGCHLD;
    register long a1 asm("$a1") = 0;
    register long a2 asm("$a2") = 0;
    register long a3 asm("$a3") = 0;
    register long a4 asm("$a4") = 0;
    register long a7 asm("$a7") = SYS_clone;
    asm volatile("syscall 0" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7) : "memory");
    return a0;
#else
#error Unsupported clone register ABI
#endif
}

int main(void)
{
    puts("================================================");
    puts("  TEST: clone inherits floating-point state");
    printf("  FILE: %s\n", __FILE__);
    puts("================================================");

    const double expected = 8192.5;
#if defined(__riscv)
    asm volatile("fld ft0, %0" :: "m"(expected) : "ft0", "memory");
#elif defined(__x86_64__)
    asm volatile("movsd %0, %%xmm15" :: "m"(expected) : "xmm15", "memory");
#elif defined(__aarch64__)
    asm volatile("ldr d31, [%0]" :: "r"(&expected) : "v31", "memory");
#elif defined(__loongarch64)
    asm volatile("fld.d $f31, %0, 0" :: "r"(&expected) : "$f31", "memory");
#endif

    long pid = raw_clone_sigchld();
    if (pid == 0) {
        double observed = 0.0;
#if defined(__riscv)
        asm volatile("fsd ft0, %0" : "=m"(observed) :: "memory");
#elif defined(__x86_64__)
        asm volatile("movsd %%xmm15, %0" : "=m"(observed) :: "memory");
#elif defined(__aarch64__)
        asm volatile("str d31, [%0]" :: "r"(&observed) : "memory");
#elif defined(__loongarch64)
        asm volatile("fst.d $f31, %0, 0" :: "r"(&observed) : "memory");
#endif
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
                  "child observes inherited FP register at clone return");
        }
    }

    puts("------------------------------------------------");
    printf("  DONE: %d pass, %d fail\n", pass_count, fail_count);
    puts("================================================");
    return fail_count > 0 ? 1 : 0;
}
