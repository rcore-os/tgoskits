#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#define MCL_CURRENT 1
#define MCL_FUTURE  2
#define MCL_ONFAULT 4

#if defined(__x86_64__)
#define SYS_MLOCKALL_NR   151
#define SYS_MUNLOCKALL_NR 152
#define SYS_MUNLOCK_NR    150
#elif defined(__riscv) || defined(__aarch64__) || defined(__loongarch64)
#define SYS_MLOCKALL_NR   230
#define SYS_MUNLOCKALL_NR 231
#define SYS_MUNLOCK_NR    229
#else
#error "unsupported architecture for mlock-family test"
#endif

int main(void)
{
    TEST_START("mlockall / munlockall / munlock");

    /* Valid mlockall flag combinations. */
    CHECK_RET(syscall(SYS_MLOCKALL_NR, MCL_CURRENT), 0,
              "mlockall(MCL_CURRENT)");
    CHECK_RET(syscall(SYS_MLOCKALL_NR, MCL_CURRENT | MCL_FUTURE), 0,
              "mlockall(MCL_CURRENT|MCL_FUTURE)");
    CHECK_RET(syscall(SYS_MLOCKALL_NR, MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT), 0,
              "mlockall(CURRENT|FUTURE|ONFAULT)");

    /* Invalid flags. */
    CHECK_ERR(syscall(SYS_MLOCKALL_NR, 0x80000000), EINVAL,
              "mlockall(bad high bit) -> EINVAL");
    CHECK_ERR(syscall(SYS_MLOCKALL_NR, MCL_ONFAULT), EINVAL,
              "mlockall(MCL_ONFAULT alone) -> EINVAL");

    /* munlockall. */
    CHECK_RET(syscall(SYS_MUNLOCKALL_NR), 0, "munlockall()");

    /* munlock: zero length no-op, valid range succeeds, hole -> ENOMEM. */
    CHECK_RET(syscall(SYS_MUNLOCK_NR, 0, 0), 0, "munlock(0, 0)");
    {
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "anonymous mmap for munlock");
        if (p != MAP_FAILED) {
            CHECK_RET(syscall(SYS_MUNLOCK_NR, (unsigned long)p, 4096), 0,
                      "munlock(mapped range)");
            munmap(p, 4096);
        }
    }
    CHECK_ERR(syscall(SYS_MUNLOCK_NR, 0x100000000000ULL, 4096), ENOMEM,
              "munlock(unmapped range) -> ENOMEM");
    CHECK_ERR(syscall(SYS_MUNLOCK_NR, 0xffffffffffffffffULL, 4096), EINVAL,
              "munlock(overflow) -> EINVAL");

    TEST_DONE();
}
