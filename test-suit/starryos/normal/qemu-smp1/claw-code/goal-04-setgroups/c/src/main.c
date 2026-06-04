#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>
#include <grp.h>

/*
 * /proc/self/setgroups test
 *
 *   1. /proc/self/setgroups should exist and be readable
 *   2. Reading returns "allow" by default
 *   3. Writing "deny" changes the value
 *   4. After unshare(CLONE_NEWUSER), writing "deny" should cause
 *      setgroups(2) to return EPERM
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

    /* 2. unshare(CLONE_NEWUSER), then write "deny" to setgroups,
     *    then assert setgroups(2) returns EPERM */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");

        if (ret == 0) {
            /* Before deny, setgroups with empty list should not fail with EPERM */
            errno = 0;
            long before_ret = syscall(SYS_setgroups, 0, NULL);
            int before_errno = errno;
            printf("  INFO | setgroups(0, NULL) before deny: ret=%ld errno=%d\n",
                   before_ret, before_errno);
            CHECK(before_errno != EPERM,
                  "before writing deny, setgroups(2) should NOT return EPERM");

            /* Write "deny" to /proc/self/setgroups */
            int fd = open("/proc/self/setgroups", O_WRONLY);
            CHECK(fd >= 0, "/proc/self/setgroups should be writable after unshare");
            if (fd >= 0) {
                const char *val = "deny";
                ssize_t n = write(fd, val, strlen(val));
                CHECK(n > 0, "write \"deny\" to setgroups should succeed after unshare");
                close(fd);
            }

            /* Verify read-back */
            fd = open("/proc/self/setgroups", O_RDONLY);
            if (fd >= 0) {
                char buf[32] = {0};
                ssize_t n = read(fd, buf, sizeof(buf) - 1);
                CHECK(n >= 0, "read setgroups after write should succeed");
                if (n >= 0) {
                    int matches_deny = (n == 4 && memcmp(buf, "deny", 4) == 0)
                        || (n == 5 && memcmp(buf, "deny\n", 5) == 0);
                    CHECK(matches_deny, "reading setgroups should return \"deny\"");
                }
                close(fd);
            }

            /* setgroups(2) must now return EPERM */
            errno = 0;
            long after_ret = syscall(SYS_setgroups, 0, NULL);
            int after_errno = errno;
            printf("  INFO | setgroups(0, NULL) after deny: ret=%ld errno=%d\n",
                   after_ret, after_errno);
            CHECK(after_ret == -1 && after_errno == EPERM,
                  "after writing \"deny\", setgroups(2) should return EPERM");
        }
    }

    TEST_DONE();
}
