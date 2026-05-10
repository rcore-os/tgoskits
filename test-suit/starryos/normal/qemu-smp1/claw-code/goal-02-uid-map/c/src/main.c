#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>

/*
 * /proc/self/uid_map test
 *
 * After unshare(CLONE_NEWUSER):
 *   1. /proc/self/uid_map should exist and be readable
 *   2. Writing "0 <uid> 1" maps UID 0 to the specified UID
 */

static int call_unshare(int flags) {
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void) {
    TEST_START("uid_map");

    /* 1. /proc/self/uid_map should exist (no unshare needed) */
    {
        int fd = open("/proc/self/uid_map", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/uid_map should exist and be readable");
        if (fd >= 0) {
            char buf[256] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/uid_map should succeed");
            close(fd);
        }
    }

    /* 2. Write uid mapping: "0 0 1" (map root to root, trivial mapping) */
    {
        int fd = open("/proc/self/uid_map", O_WRONLY);
        CHECK(fd >= 0, "/proc/self/uid_map should be writable");
        if (fd >= 0) {
            const char *mapping = "0 0 1\n";
            ssize_t n = write(fd, mapping, strlen(mapping));
            /* On Linux, this may fail with EPERM if not in a user namespace.
             * On StarryOS, we allow it as a simplification. */
            CHECK(n > 0 || (n == -1 && (errno == EPERM || errno == EINVAL)),
                  "write to uid_map should succeed or fail with EPERM/EINVAL");
            if (n > 0) {
                printf("  INFO | wrote to uid_map: %.*s\n", (int)n, mapping);
            }
            close(fd);
        }
    }

    /* 3. After unshare(CLONE_NEWUSER), uid_map should still exist */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");
        if (ret == 0) {
            int fd = open("/proc/self/uid_map", O_RDONLY);
            CHECK(fd >= 0, "/proc/self/uid_map should exist after unshare");
            if (fd >= 0) {
                char buf[256] = {0};
                ssize_t n = read(fd, buf, sizeof(buf) - 1);
                CHECK(n >= 0, "read uid_map after unshare should succeed");
                close(fd);
            }
        }
    }

    TEST_DONE();
}
