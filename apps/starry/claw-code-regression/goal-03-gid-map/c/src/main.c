#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>
#include <grp.h>

/*
 * /proc/self/gid_map test
 *
 * After unshare(CLONE_NEWUSER):
 *   1. /proc/self/gid_map should exist and be readable
 *   2. Writing "0 0 1" maps GID 0
 *   3. getgid()/getegid()/getresgid() return the mapped value (0)
 *   4. Writing gid_map must NOT affect UID side (getuid() stays 65534)
 */

static int call_unshare(int flags) {
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void) {
    TEST_START("gid_map");

    /* 1. /proc/self/gid_map should exist (no unshare needed) */
    {
        int fd = open("/proc/self/gid_map", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/gid_map should exist and be readable");
        if (fd >= 0) {
            char buf[256] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/gid_map should succeed");
            close(fd);
        }
    }

    /* 2. unshare(CLONE_NEWUSER) then write gid_map + assert getgid/getresgid */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");

        if (ret == 0) {
            /* Before writing gid_map, getgid() should return 65534 (nobody) */
            gid_t gid_before = getgid();
            printf("  INFO | gid before map write: %u\n", gid_before);
            CHECK(gid_before == 65534,
                  "before writing gid_map, getgid() should return 65534 (nobody)");

            /* UID side should also be 65534 */
            uid_t uid_before = getuid();
            printf("  INFO | uid before gid_map write: %u\n", uid_before);
            CHECK(uid_before == 65534,
                  "before writing gid_map, getuid() should return 65534 (nobody)");

            /* Write "0 0 1" to map namespace GID 0 to GID 0 */
            int fd = open("/proc/self/gid_map", O_WRONLY);
            CHECK(fd >= 0, "/proc/self/gid_map should be writable after unshare");
            if (fd >= 0) {
                const char *mapping = "0 0 1\n";
                ssize_t n = write(fd, mapping, strlen(mapping));
                CHECK(n > 0, "write to gid_map should succeed after unshare");
                close(fd);
            }

            /* After writing gid_map, getgid() should return 0 */
            gid_t gid_after = getgid();
            printf("  INFO | gid after map write: %u\n", gid_after);
            CHECK(gid_after == 0,
                  "after writing gid_map \"0 0 1\", getgid() should return 0");

            /* getegid() should also return 0 */
            gid_t egid_after = getegid();
            CHECK(egid_after == 0,
                  "after writing gid_map, getegid() should return 0");

            /* getresgid() should return 0 for all three */
            gid_t rgid, regid, rsgid;
            CHECK_RET(getresgid(&rgid, &regid, &rsgid), 0, "getresgid should succeed");
            CHECK(rgid == 0 && regid == 0 && rsgid == 0,
                  "after writing gid_map, getresgid() should return (0,0,0)");

            /* UID side must NOT be affected by gid_map write */
            uid_t uid_after = getuid();
            printf("  INFO | uid after gid_map write: %u\n", uid_after);
            CHECK(uid_after == 65534,
                  "after writing gid_map, getuid() should STILL return 65534 (not affected by gid_map)");

            /* geteuid() should also still return 65534 */
            uid_t euid_after = geteuid();
            CHECK(euid_after == 65534,
                  "after writing gid_map, geteuid() should STILL return 65534");

            /* getresuid() should still return 65534 for all three */
            uid_t ruid, reuid, rsuid;
            CHECK_RET(getresuid(&ruid, &reuid, &rsuid), 0, "getresuid should succeed");
            CHECK(ruid == 65534 && reuid == 65534 && rsuid == 65534,
                  "after writing gid_map, getresuid() should return (65534,65534,65534)");
        }
    }

    TEST_DONE();
}
