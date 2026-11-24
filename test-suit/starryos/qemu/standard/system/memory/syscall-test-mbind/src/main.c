#include "test_framework.h"

#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_mbind
#if defined(__x86_64__)
#define SYS_mbind 237
#elif defined(__aarch64__)
#define SYS_mbind 235
#elif defined(__riscv)
#define SYS_mbind 235
#elif defined(__loongarch__)
#define SYS_mbind 235
#endif
#endif

#define MPOL_DEFAULT 0
#define MPOL_BIND 2
#define MPOL_F_STATIC_NODES (1 << 15)
#define MPOL_MF_STRICT (1U << 0)
#define MPOL_MF_MOVE (1U << 1)
#define MPOL_MF_MOVE_ALL (1U << 2)

static long call_mbind(void *addr, unsigned long len, int mode,
                       const unsigned long *nodemask, unsigned long maxnode,
                       unsigned int flags)
{
    return syscall(SYS_mbind, addr, len, mode, nodemask, maxnode, flags);
}

int main(void)
{
#ifndef SYS_mbind
    TEST_START("mbind unavailable on this libc");
    printf("  SKIP | SYS_mbind is not defined\n");
    TEST_DONE();
#else
    TEST_START("mbind single-node acceptance");

    long pagesize = sysconf(_SC_PAGESIZE);
    CHECK(pagesize > 0, "sysconf _SC_PAGESIZE succeeds");

    void *page = mmap(NULL, (size_t)pagesize, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(page != MAP_FAILED, "mmap one page for mbind");
    if (page == MAP_FAILED) {
        TEST_DONE();
    }

    unsigned long node0 = 1;
    CHECK_RET(call_mbind(page, (unsigned long)pagesize, MPOL_DEFAULT, NULL, 0, 0), 0,
              "MPOL_DEFAULT binding succeeds");
    CHECK_RET(call_mbind(page, (unsigned long)pagesize, MPOL_BIND, &node0, 8, 0), 0,
              "MPOL_BIND node0 succeeds");
    CHECK_RET(call_mbind(page, (unsigned long)pagesize,
                         MPOL_BIND | MPOL_F_STATIC_NODES, &node0, 8,
                         MPOL_MF_STRICT | MPOL_MF_MOVE | MPOL_MF_MOVE_ALL),
              0, "valid mode flags and mbind flags succeed");

    CHECK_ERR(call_mbind((char *)page + 1, (unsigned long)pagesize,
                         MPOL_DEFAULT, NULL, 0, 0),
              EINVAL, "unaligned addr returns EINVAL");
    CHECK_ERR(call_mbind(page, 0, MPOL_DEFAULT, NULL, 0, 0),
              EINVAL, "zero length returns EINVAL");
    CHECK_ERR(call_mbind(page, (unsigned long)pagesize, 99, NULL, 0, 0),
              EINVAL, "unknown mode returns EINVAL");
    CHECK_ERR(call_mbind(page, (unsigned long)pagesize, MPOL_DEFAULT,
                         NULL, 0, 0x80000000U),
              EINVAL, "unknown mbind flags return EINVAL");
    CHECK_ERR(call_mbind(page, (unsigned long)pagesize, MPOL_BIND,
                         (void *)(uintptr_t)1, 8, 0),
              EFAULT, "bad nodemask pointer returns EFAULT");

    munmap(page, (size_t)pagesize);
    TEST_DONE();
#endif
}
