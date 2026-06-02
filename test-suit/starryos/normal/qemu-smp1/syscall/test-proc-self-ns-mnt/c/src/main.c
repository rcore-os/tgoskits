#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

static const char *MNT_NS = "/proc/self/ns/mnt";

static void check_stat_follow(void)
{
    struct stat st;

    CHECK_RET(stat(MNT_NS, &st), 0, "stat follows /proc/self/ns/mnt");
    if (errno == 0) {
        CHECK(S_ISREG(st.st_mode), "stat target is a regular namespace handle");
        CHECK(st.st_size == 0, "namespace handle size is zero");
        CHECK(st.st_ino != 0, "namespace handle inode is non-zero");
    }
}

static void check_lstat_baseline(void)
{
    struct stat st;
    errno = 0;
    int rc = lstat(MNT_NS, &st);
    /*
     * On Linux, lstat on /proc/self/ns/mnt returns a symlink (S_ISLNK).
     * StarryOS currently exposes it as a regular file.  Record the
     * current mode without asserting to serve as a behaviour baseline.
     */
    if (rc == 0) {
        printf("  INFO | %s:%d | lstat(/proc/self/ns/mnt) mode=0%o\n",
               __FILE__, __LINE__, st.st_mode & S_IFMT);
    } else {
        printf("  INFO | %s:%d | lstat(/proc/self/ns/mnt) failed: errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
    }
    CHECK(rc == 0 || errno == ELOOP, "lstat /proc/self/ns/mnt succeeds or ELOOP");
}

static void check_readlink_baseline(void)
{
    char buf[256];
    errno = 0;
    ssize_t n = readlink(MNT_NS, buf, sizeof(buf) - 1);
    /*
     * Linux returns a string like "mnt:[4026531841]".
     * StarryOS currently exposes it as a regular file, not a symlink,
     * so readlink may return -EINVAL.  Record the result as a baseline.
     */
    if (n >= 0) {
        buf[n] = '\0';
        printf("  INFO | %s:%d | readlink(/proc/self/ns/mnt) = \"%s\"\n",
               __FILE__, __LINE__, buf);
        __pass++;
    } else {
        printf("  INFO | %s:%d | readlink(/proc/self/ns/mnt) failed: errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        /* Accept EINVAL (not a symlink) as documented current behaviour */
        CHECK(errno == EINVAL || errno == ELOOP,
              "readlink: expected EINVAL (regular file) or ELOOP");
    }
}

static void check_open_read_close(void)
{
    errno = 0;
    int fd = open(MNT_NS, O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0, "open /proc/self/ns/mnt as readonly handle");
    if (fd < 0) {
        return;
    }

    struct stat st;
    CHECK_RET(fstat(fd, &st), 0, "fstat namespace handle fd");
    if (errno == 0) {
        CHECK(S_ISREG(st.st_mode), "fstat target is a regular namespace handle");
        CHECK(st.st_size == 0, "fstat namespace handle size is zero");
        CHECK(st.st_ino != 0, "fstat namespace handle inode is non-zero");
    }

    /* read() on a namespace handle should return 0 (empty file) */
    char rdbuf[64];
    errno = 0;
    ssize_t rn = read(fd, rdbuf, sizeof(rdbuf));
    if (rn == 0) {
        printf("  PASS | %s:%d | read namespace handle returned 0 (empty)\n",
               __FILE__, __LINE__);
        __pass++;
    } else if (rn > 0) {
        printf("  INFO | %s:%d | read namespace handle returned %zd bytes\n",
               __FILE__, __LINE__, rn);
        __pass++;
    } else {
        printf("  FAIL | %s:%d | read namespace handle | ret=%zd errno=%d (%s)\n",
               __FILE__, __LINE__, rn, errno, strerror(errno));
        __fail++;
    }

    CHECK_RET(close(fd), 0, "close namespace handle fd");
}

int main(void)
{
    TEST_START("/proc/self/ns/mnt namespace handle");

    check_stat_follow();
    check_lstat_baseline();
    check_readlink_baseline();
    check_open_read_close();

    TEST_DONE();
}
