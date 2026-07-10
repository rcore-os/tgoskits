/*
 * bug-mempolicy-validation: NUMA policy syscalls must reject invalid
 * arguments while still accepting single-node no-op policies.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_get_mempolicy
#if defined(__x86_64__)
#define SYS_get_mempolicy 239
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_get_mempolicy 236
#endif
#endif

#ifndef SYS_set_mempolicy
#if defined(__x86_64__)
#define SYS_set_mempolicy 238
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_set_mempolicy 237
#endif
#endif

#ifndef SYS_mbind
#if defined(__x86_64__)
#define SYS_mbind 237
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_mbind 235
#endif
#endif

#define MPOL_DEFAULT 0
#define MPOL_BIND 2
#define MPOL_F_NODE (1UL << 0)
#define MPOL_F_ADDR (1UL << 1)
#define MPOL_F_MEMS_ALLOWED (1UL << 2)

static int failures = 0;

static void expect_ret(long ret, long expected, const char *what)
{
    if (ret == expected) {
        printf("PASS: %s ret=%ld\n", what, ret);
    } else {
        printf("FAIL: %s expected=%ld got=%ld errno=%d (%s)\n",
               what, expected, ret, errno, strerror(errno));
        failures++;
    }
}

static void expect_errno(long ret, int expected, const char *what)
{
    if (ret == -1 && errno == expected) {
        printf("PASS: %s errno=%d\n", what, errno);
    } else {
        printf("FAIL: %s expected errno=%d got ret=%ld errno=%d (%s)\n",
               what, expected, ret, errno, strerror(errno));
        failures++;
    }
}

int main(void)
{
#if !defined(SYS_get_mempolicy) || !defined(SYS_set_mempolicy) || !defined(SYS_mbind)
    printf("SKIP: mempolicy syscall numbers unavailable\n");
    return 0;
#else
    int policy = -1;
    unsigned long node0 = 1;
    unsigned long mask = 0;
    long pagesize = sysconf(_SC_PAGESIZE);
    void *page = mmap(NULL, (size_t)pagesize, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        printf("FAIL: mmap errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    errno = 0;
    expect_ret(syscall(SYS_get_mempolicy, &policy, &mask, 8, NULL, 0), 0,
               "get_mempolicy default");
    if (policy != MPOL_DEFAULT || mask != 1UL) {
        printf("FAIL: expected default policy/node0, got policy=%d mask=%lu\n",
               policy, mask);
        failures++;
    }

    errno = 0;
    expect_ret(syscall(SYS_set_mempolicy, MPOL_BIND, &node0, 8), 0,
               "set_mempolicy MPOL_BIND node0");

    errno = 0;
    expect_ret(syscall(SYS_mbind, page, (unsigned long)pagesize,
                       MPOL_BIND, &node0, 8, 0),
               0, "mbind MPOL_BIND node0");

    errno = 0;
    expect_errno(syscall(SYS_get_mempolicy, &policy, &mask, 8, NULL, MPOL_F_NODE),
                 EINVAL, "get_mempolicy rejects MPOL_F_NODE without MPOL_F_ADDR");

    errno = 0;
    expect_errno(syscall(SYS_get_mempolicy, &policy, &mask, 8, NULL,
                         MPOL_F_MEMS_ALLOWED | MPOL_F_ADDR),
                 EINVAL, "get_mempolicy rejects combined MPOL_F_MEMS_ALLOWED");

    errno = 0;
    expect_errno(syscall(SYS_set_mempolicy, 99, NULL, 0),
                 EINVAL, "set_mempolicy rejects unknown mode");

    errno = 0;
    expect_errno(syscall(SYS_set_mempolicy, MPOL_BIND, (void *)(uintptr_t)1, 8),
                 EFAULT, "set_mempolicy rejects bad nodemask");

    errno = 0;
    expect_errno(syscall(SYS_mbind, (char *)page + 1, (unsigned long)pagesize,
                         MPOL_DEFAULT, NULL, 0, 0),
                 EINVAL, "mbind rejects unaligned address");

    errno = 0;
    expect_errno(syscall(SYS_mbind, page, (unsigned long)pagesize,
                         MPOL_BIND, (void *)(uintptr_t)1, 8, 0),
                 EFAULT, "mbind rejects bad nodemask");

    munmap(page, (size_t)pagesize);

    if (failures == 0) {
        printf("bug-mempolicy-validation: passed\n");
    }
    return failures == 0 ? 0 : 1;
#endif
}
