#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __riscv
int main(void)
{
    puts("test-clone-fp-state is riscv64-only");
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

#endif
