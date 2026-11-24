/* Deferred locking charges virtual pages without faulting them in. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Minimal Linux capability ABI; the test rootfs has no kernel headers. */
#define CAP_VERSION_3 0x20080522
#define CAP_IPC_LOCK 14
struct cap_header { uint32_t version; int32_t pid; };
struct cap_data { uint32_t effective, permitted, inheritable; };

#ifndef MLOCK_ONFAULT
#define MLOCK_ONFAULT 1
#endif

#define REQUIRE(condition, message) do { \
    if (!(condition)) { \
        fprintf(stderr, "FAIL: %s (errno=%d: %s)\n", \
                message, errno, strerror(errno)); \
        exit(1); \
    } \
} while (0)

static void expect_residency(void *base, size_t page, unsigned int mask)
{
    unsigned char vec[4] = {0};
    REQUIRE(syscall(SYS_mincore, base, 4 * page, vec) == 0, "mincore");
    for (unsigned int i = 0; i < 4; i++) {
        REQUIRE((vec[i] & 1) == ((mask >> i) & 1),
                "only explicitly accessed pages are resident");
    }
}

static void expect_limit(void *base, size_t page)
{
    errno = 0;
    REQUIRE(syscall(SYS_mlock2, base, page, MLOCK_ONFAULT) == -1 && errno == ENOMEM,
            "unfaulted locked pages consume the memlock limit");
}

int main(void)
{
    const long page_size = sysconf(_SC_PAGESIZE);
    REQUIRE(page_size > 0, "page size");
    const size_t page = (size_t)page_size;
    struct rlimit limit;
    REQUIRE(syscall(SYS_prlimit64, 0, RLIMIT_MEMLOCK, NULL, &limit) == 0,
            "read memlock limit");
    REQUIRE(limit.rlim_max >= 2 * page, "hard limit permits two pages");
    limit.rlim_cur = 2 * page;
    REQUIRE(syscall(SYS_prlimit64, 0, RLIMIT_MEMLOCK, &limit, NULL) == 0,
            "set two-page soft limit");

    /* Root must not bypass the limit. Only this short-lived process changes. */
    struct cap_header header = {CAP_VERSION_3, 0};
    struct cap_data caps[2];
    REQUIRE(syscall(SYS_capget, &header, caps) == 0, "read capabilities");
    caps[CAP_IPC_LOCK / 32].effective &= ~(1U << (CAP_IPC_LOCK % 32));
    REQUIRE(syscall(SYS_capset, &header, caps) == 0, "drop effective CAP_IPC_LOCK");

    unsigned char *base = (void *)syscall(SYS_mmap, NULL, 4 * page,
        PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(base != MAP_FAILED, "map untouched anonymous range");
    REQUIRE(syscall(SYS_madvise, base, 4 * page, MADV_NOHUGEPAGE) == 0,
            "keep per-page residency observable");
    expect_residency(base, page, 0);
    REQUIRE(syscall(SYS_mlock2, base, 2 * page, MLOCK_ONFAULT) == 0,
            "defer locking of two pages");
    expect_residency(base, page, 0);
    REQUIRE(syscall(SYS_mlock2, base + page, page, MLOCK_ONFAULT) == 0,
            "overlapping locks do not charge twice");
    expect_limit(base + 2 * page, page);
    REQUIRE(syscall(SYS_mprotect, base, page, PROT_READ) == 0,
            "split locked VMA permissions");
    expect_limit(base + 2 * page, page);
    expect_residency(base, page, 0);
    puts("PASS: MLOCK_ONFAULT stays nonresident and accounts overlapping/split VMAs");

    REQUIRE(syscall(SYS_munlock, base + page, page) == 0,
            "partial unlock releases one page of quota");
    REQUIRE(syscall(SYS_mlock2, base + 2 * page, page, MLOCK_ONFAULT) == 0,
            "reuse released quota");
    expect_limit(base + 3 * page, page);
    expect_residency(base, page, 0);
    REQUIRE(*(volatile unsigned char *)base == 0, "fault first locked page");
    base[2 * page] = 0x5a;
    expect_residency(base, page, 5);
    REQUIRE(base[2 * page] == 0x5a, "locked write preserves contents");
    REQUIRE(syscall(SYS_munmap, base, page) == 0, "unmap releases locked quota");
    REQUIRE(syscall(SYS_mlock2, base + 3 * page, page, MLOCK_ONFAULT) == 0,
            "reuse unmapped quota");
    expect_limit(base + page, page);
    REQUIRE(syscall(SYS_munlock, base + page, 3 * page) == 0,
            "unlock remaining range");
    REQUIRE(syscall(SYS_mlock2, base + page, page, 0) == 0,
            "ordinary mlock2 populates immediately");
    unsigned char resident = 0;
    REQUIRE(syscall(SYS_mincore, base + page, page, &resident) == 0 && (resident & 1),
            "ordinary lock is resident before the first access");
    REQUIRE(syscall(SYS_munlock, base + page, page) == 0, "unlock populated page");
    REQUIRE(syscall(SYS_munmap, base + page, 3 * page) == 0, "release test mapping");
    puts("PASS: fault, partial unlock and unmap preserve residency and quota semantics");
    return 0;
}
