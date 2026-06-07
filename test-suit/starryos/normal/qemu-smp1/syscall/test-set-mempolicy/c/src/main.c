#include "test_framework.h"

#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_set_mempolicy
#if defined(__x86_64__)
#define SYS_set_mempolicy 238
#elif defined(__aarch64__)
#define SYS_set_mempolicy 237
#elif defined(__riscv)
#define SYS_set_mempolicy 237
#elif defined(__loongarch__)
#define SYS_set_mempolicy 237
#endif
#endif

#define MPOL_DEFAULT 0
#define MPOL_PREFERRED 1
#define MPOL_BIND 2
#define MPOL_INTERLEAVE 3
#define MPOL_LOCAL 4
#define MPOL_PREFERRED_MANY 5
#define MPOL_WEIGHTED_INTERLEAVE 6
#define MPOL_F_STATIC_NODES (1 << 15)

static long call_set_mempolicy(int mode, const unsigned long *nodemask,
                               unsigned long maxnode)
{
    return syscall(SYS_set_mempolicy, mode, nodemask, maxnode);
}

int main(void)
{
#ifndef SYS_set_mempolicy
    TEST_START("set_mempolicy unavailable on this libc");
    printf("  SKIP | SYS_set_mempolicy is not defined\n");
    TEST_DONE();
#else
    TEST_START("set_mempolicy single-node acceptance");

    unsigned long node0 = 1;
    CHECK_RET(call_set_mempolicy(MPOL_DEFAULT, NULL, 0), 0,
              "MPOL_DEFAULT without nodemask succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_PREFERRED, &node0, 8), 0,
              "MPOL_PREFERRED with node0 succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_BIND, &node0, 8), 0,
              "MPOL_BIND with node0 succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_INTERLEAVE, &node0, 8), 0,
              "MPOL_INTERLEAVE with node0 succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_LOCAL, NULL, 0), 0,
              "MPOL_LOCAL succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_PREFERRED_MANY, &node0, 8), 0,
              "MPOL_PREFERRED_MANY with node0 succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_WEIGHTED_INTERLEAVE, &node0, 8), 0,
              "MPOL_WEIGHTED_INTERLEAVE with node0 succeeds");
    CHECK_RET(call_set_mempolicy(MPOL_BIND | MPOL_F_STATIC_NODES, &node0, 8), 0,
              "mode flags are accepted with a valid policy");

    CHECK_ERR(call_set_mempolicy(-1, NULL, 0), EINVAL,
              "negative policy mode returns EINVAL");
    CHECK_ERR(call_set_mempolicy(99, NULL, 0), EINVAL,
              "unknown policy mode returns EINVAL");
    CHECK_ERR(call_set_mempolicy(MPOL_BIND, (void *)(uintptr_t)1, 8), EFAULT,
              "bad nodemask pointer returns EFAULT");

    TEST_DONE();
#endif
}
