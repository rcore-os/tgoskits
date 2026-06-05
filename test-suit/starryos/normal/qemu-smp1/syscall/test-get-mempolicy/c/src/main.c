#include "test_framework.h"

#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_get_mempolicy
#if defined(__x86_64__)
#define SYS_get_mempolicy 239
#elif defined(__aarch64__)
#define SYS_get_mempolicy 236
#elif defined(__riscv)
#define SYS_get_mempolicy 236
#elif defined(__loongarch__)
#define SYS_get_mempolicy 236
#endif
#endif

#define MPOL_DEFAULT 0
#define MPOL_F_NODE (1UL << 0)
#define MPOL_F_ADDR (1UL << 1)
#define MPOL_F_MEMS_ALLOWED (1UL << 2)

static long call_get_mempolicy(int *policy, unsigned long *nodemask,
                               unsigned long maxnode, void *addr,
                               unsigned long flags)
{
    return syscall(SYS_get_mempolicy, policy, nodemask, maxnode, addr, flags);
}

int main(void)
{
#ifndef SYS_get_mempolicy
    TEST_START("get_mempolicy unavailable on this libc");
    printf("  SKIP | SYS_get_mempolicy is not defined\n");
    TEST_DONE();
#else
    TEST_START("get_mempolicy single-node semantics");

    int policy = -1;
    unsigned long mask = 0;
    CHECK_RET(call_get_mempolicy(&policy, &mask, 8, NULL, 0), 0,
              "get_mempolicy returns current policy");
    CHECK(policy == MPOL_DEFAULT, "default policy is MPOL_DEFAULT");
    CHECK(mask == 1UL, "single-node nodemask contains node 0");

    policy = -1;
    CHECK_RET(call_get_mempolicy(&policy, NULL, 0, NULL,
                                 MPOL_F_NODE | MPOL_F_ADDR),
              0, "MPOL_F_NODE|MPOL_F_ADDR returns node id");
    CHECK(policy == 0, "address belongs to node 0");

    mask = 0;
    CHECK_RET(call_get_mempolicy(NULL, &mask, 8, NULL, MPOL_F_MEMS_ALLOWED),
              0, "MPOL_F_MEMS_ALLOWED returns allowed node mask");
    CHECK(mask == 1UL, "allowed node mask contains node 0");

    CHECK_RET(call_get_mempolicy(NULL, NULL, 0, NULL, 0), 0,
              "NULL outputs are accepted when nothing is written");
    CHECK_RET(call_get_mempolicy(NULL, &mask, 0, NULL, 0), 0,
              "maxnode=0 skips nodemask write");

    CHECK_ERR(call_get_mempolicy(&policy, &mask, 8, NULL, 0x100), EINVAL,
              "unknown get_mempolicy flags return EINVAL");
    CHECK_ERR(call_get_mempolicy(&policy, &mask, 8, NULL, MPOL_F_NODE), EINVAL,
              "MPOL_F_NODE without MPOL_F_ADDR returns EINVAL");
    CHECK_ERR(call_get_mempolicy(&policy, &mask, 8, NULL,
                                 MPOL_F_MEMS_ALLOWED | MPOL_F_ADDR),
              EINVAL, "MPOL_F_MEMS_ALLOWED cannot be combined");
    CHECK_ERR(call_get_mempolicy(NULL, (void *)(uintptr_t)1, 8, NULL, 0),
              EFAULT, "bad nodemask pointer returns EFAULT");

    TEST_DONE();
#endif
}
