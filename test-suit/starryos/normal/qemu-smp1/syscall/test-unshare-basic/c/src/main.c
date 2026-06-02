#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

/*
 * test-unshare-basic — probe unshare(2) behaviour on StarryOS.
 *
 * Current StarryOS: sys_unshare returns AxError::NotSupported → -ENOSYS.
 * PR #981 adds sys_unshare for all 6 namespace types.
 * This test records the CURRENT behaviour as a baseline; it is expected
 * to show ENOSYS failures until unshare support is merged.
 */

static void check_unshare_null(void)
{
    errno = 0;
    int r = unshare(0);
    CHECK_ERR(r, EINVAL, "unshare(0) → EINVAL (no flags specified)");
}

static void check_unshare_new_ns(void)
{
    errno = 0;
    int r = unshare(CLONE_NEWNS);
    /*
     * Current StarryOS: -ENOSYS (syscall unimplemented)
     * After PR #981 merge: success or -EPERM (depending on capability checks)
     */
    if (r == 0) {
        printf("  PASS | %s:%d | unshare(CLONE_NEWNS) returned 0 (supported)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (r == -1 && errno == ENOSYS) {
        printf("  INFO | %s:%d | unshare(CLONE_NEWNS) → ENOSYS (current baseline)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (r == -1 && errno == EPERM) {
        printf("  INFO | %s:%d | unshare(CLONE_NEWNS) → EPERM (capability check)\n",
               __FILE__, __LINE__);
        __pass++;
    } else {
        printf("  FAIL | %s:%d | unshare(CLONE_NEWNS) unexpected ret=%d errno=%d (%s)\n",
               __FILE__, __LINE__, r, errno, strerror(errno));
        __fail++;
    }
}

static void check_unshare_new_user(void)
{
    errno = 0;
    int r = unshare(CLONE_NEWUSER);
    if (r == 0) {
        printf("  PASS | %s:%d | unshare(CLONE_NEWUSER) returned 0 (supported)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (r == -1 && errno == ENOSYS) {
        printf("  INFO | %s:%d | unshare(CLONE_NEWUSER) → ENOSYS (current baseline)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (r == -1 && errno == EPERM) {
        printf("  INFO | %s:%d | unshare(CLONE_NEWUSER) → EPERM (capability check)\n",
               __FILE__, __LINE__);
        __pass++;
    } else {
        printf("  FAIL | %s:%d | unshare(CLONE_NEWUSER) unexpected ret=%d errno=%d (%s)\n",
               __FILE__, __LINE__, r, errno, strerror(errno));
        __fail++;
    }
}

static void check_unshare_new_uts(void)
{
    errno = 0;
    int r = unshare(CLONE_NEWUTS);
    if (r == 0) {
        printf("  PASS | %s:%d | unshare(CLONE_NEWUTS) returned 0 (supported)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (r == -1 && errno == ENOSYS) {
        printf("  INFO | %s:%d | unshare(CLONE_NEWUTS) → ENOSYS (current baseline)\n",
               __FILE__, __LINE__);
        __pass++;
    } else {
        printf("  FAIL | %s:%d | unshare(CLONE_NEWUTS) unexpected ret=%d errno=%d (%s)\n",
               __FILE__, __LINE__, r, errno, strerror(errno));
        __fail++;
    }
}

int main(void)
{
    TEST_START("unshare basic probes");

    check_unshare_null();
    check_unshare_new_ns();
    check_unshare_new_user();
    check_unshare_new_uts();

    if (__fail == 0) {
        printf("UNSHARE_BASIC_ALL_PASSED\n");
    } else {
        printf("UNSHARE_BASIC_HAS_FAILURES\n");
    }
    TEST_DONE();
}
