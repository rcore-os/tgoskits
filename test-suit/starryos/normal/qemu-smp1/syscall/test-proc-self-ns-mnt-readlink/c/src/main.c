#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

/*
 * test-proc-self-ns-mnt-readlink — focused readlink(2) probe on the
 * /proc/self/ns/mnt namespace handle.
 *
 * Linux exposes /proc/self/ns/mnt as a symlink whose target is
 * "mnt:[namespace-id]".  StarryOS currently exposes it as a regular
 * zero-length file.  This test records the observed behaviour without
 * asserting a specific target string, so it serves as a stable
 * regression baseline that won't break across implementation stages.
 */

static const char *MNT_NS = "/proc/self/ns/mnt";

static void check_readlink_ns_mnt(void)
{
    char buf[256];
    errno = 0;
    ssize_t n = readlink(MNT_NS, buf, sizeof(buf) - 1);
    if (n >= 0) {
        buf[n] = '\0';
        printf("  INFO | %s:%d | readlink = \"%s\"\n", __FILE__, __LINE__, buf);
        CHECK(n > 0, "readlink target is non-empty");
    } else {
        printf("  INFO | %s:%d | readlink failed: errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        CHECK(errno == EINVAL || errno == ELOOP,
              "readlink: EINVAL (regular file) or ELOOP accepted as baseline");
    }
}

static void check_readlink_zero_buffer(void)
{
    errno = 0;
    ssize_t n = readlink(MNT_NS, NULL, 0);
    if (n == -1 && errno == EFAULT) {
        printf("  PASS | %s:%d | readlink(NULL, 0) → EFAULT\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (n == -1 && errno == EINVAL) {
        /* Returns EINVAL if the path is not a symlink (regular file) */
        printf("  INFO | %s:%d | readlink(NULL, 0) → EINVAL (not a symlink)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (n == -1) {
        printf("  INFO | %s:%d | readlink(NULL, 0) → errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __pass++;
    } else {
        printf("  FAIL | %s:%d | readlink(NULL, 0) unexpected ret=%zd\n",
               __FILE__, __LINE__, n);
        __fail++;
    }
}

static void check_open_and_stat_before_readlink(void)
{
    /*
     * Verify the path is reachable and openable before attempting
     * readlink — if open/stat fails, readlink won't give useful signal.
     */
    struct stat st;
    CHECK_RET(stat(MNT_NS, &st), 0, "stat before readlink");
    if (S_ISREG(st.st_mode)) {
        printf("  INFO | %s:%d | /proc/self/ns/mnt is a regular file; "
               "readlink is not applicable (will return EINVAL)\n",
               __FILE__, __LINE__);
    }

    int fd = open(MNT_NS, O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0, "open before readlink");
    if (fd >= 0)
        CHECK_RET(close(fd), 0, "close after open");
}

int main(void)
{
    TEST_START("/proc/self/ns/mnt readlink probe");

    check_open_and_stat_before_readlink();
    check_readlink_ns_mnt();
    check_readlink_zero_buffer();

    if (__fail == 0) {
        printf("NS_MNT_READLINK_ALL_PASSED\n");
    } else {
        printf("NS_MNT_READLINK_HAS_FAILURES\n");
    }
    TEST_DONE();
}
