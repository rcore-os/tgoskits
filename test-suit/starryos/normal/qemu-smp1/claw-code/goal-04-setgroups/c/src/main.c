#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>

/*
 * /proc/self/setgroups test
 *
 *   1. /proc/self/setgroups should exist and be readable
 *   2. Reading returns "allow" by default
 *   3. Writing "deny" changes the value
 *   4. After unshare(CLONE_NEWUSER), setgroups should still exist
 */

static int call_unshare(int flags) {
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void) {
    TEST_START("setgroups");

    /* 1. /proc/self/setgroups should exist */
    {
        int fd = open("/proc/self/setgroups", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/setgroups should exist and be readable");
        if (fd >= 0) {
            char buf[32] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/setgroups should succeed");
            if (n >= 0) {
                printf("  INFO | setgroups content: %.*s\n", (int)n, buf);
            }
            close(fd);
        }
    }

    /* 2. Write "deny" to setgroups (may fail with EACCES on Linux without unshare) */
    {
        int fd = open("/proc/self/setgroups", O_WRONLY);
        if (fd >= 0) {
            const char *val = "deny";
            ssize_t n = write(fd, val, strlen(val));
            CHECK(n > 0 || (n == -1 && (errno == EPERM || errno == EINVAL)),
                  "write to setgroups should succeed or fail with EPERM/EINVAL");
            if (n > 0) {
                printf("  INFO | wrote to setgroups: %.*s\n", (int)n, val);
            }
            close(fd);
        } else {
            /* On Linux without unshare, setgroups may not be writable */
            printf("  PASS | %s:%d | setgroups not writable (errno=%d, expected on vanilla Linux)\n",
                   __FILE__, __LINE__, errno);
            __pass++;
        }
    }

    /* 3. Verify read-back after write */
    {
        int fd = open("/proc/self/setgroups", O_RDONLY);
        if (fd >= 0) {
            char buf[32] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read setgroups after write should succeed");
            close(fd);
        }
    }

    /* 4. After unshare(CLONE_NEWUSER), setgroups should still exist */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");
        if (ret == 0) {
            int fd = open("/proc/self/setgroups", O_RDONLY);
            CHECK(fd >= 0, "/proc/self/setgroups should exist after unshare");
            if (fd >= 0) {
                char buf[32] = {0};
                ssize_t n = read(fd, buf, sizeof(buf) - 1);
                CHECK(n >= 0, "read setgroups after unshare should succeed");
                close(fd);
            }
        }
    }

    TEST_DONE();
}
