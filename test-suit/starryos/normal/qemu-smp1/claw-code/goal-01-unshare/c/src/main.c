#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>

/*
 * unshare(2) syscall test
 *
 *   int unshare(int flags);
 *
 * Key semantics:
 *   1. CLONE_NEWUSER creates a new user namespace
 *   2. Returns 0 on success, -1 on error
 *   3. EINVAL for invalid/unsupported flags
 *   4. EPERM when caller lacks CAP_SYS_ADMIN (for non-user namespaces)
 *   5. After CLONE_NEWUSER, uid/gid in new namespace start as 65534 (nobody)
 */

static int call_unshare(int flags)
{
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void)
{
    TEST_START("unshare");

    /* 1. unshare(CLONE_NEWUSER) — create new user namespace */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");

        /* After unshare(CLONE_NEWUSER), uid in new namespace is 65534 */
        uid_t uid = getuid();
        CHECK(uid == 65534, "after unshare(NEWUSER), getuid() should be 65534 (nobody)");
    }

    /* 2. unshare(0) — flags=0 is valid and a no-op */
    {
        int ret = call_unshare(0);
        CHECK(ret == 0, "unshare(0) should return 0 (no-op)");
    }

    /* 3. unshare with invalid flag (0xdeadbeef) — should get EINVAL */
    {
        int ret = call_unshare(0xdeadbeef);
        CHECK(ret == -1 && errno == EINVAL,
              "unshare(0xdeadbeef) should fail with EINVAL");
    }

    TEST_DONE();
}
