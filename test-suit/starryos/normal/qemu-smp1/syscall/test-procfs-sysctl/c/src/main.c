/*
 * test_procfs_sysctl.c — presence and plausibility of the common /proc/sys and
 * /proc/filesystems stub files.
 *
 * Several runtimes read these at startup (Elasticsearch/Lucene reads
 * max_map_count; servers read somaxconn / file-max; tools enumerate
 * /proc/filesystems). They used to be ENOENT on StarryOS; the fix adds them as
 * read-only stubs. The test asserts each is openable, non-empty and has a
 * plausible value, so it passes on both a real Linux host and StarryOS without
 * pinning the exact stub numbers.
 */

#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>

/* Read a whole small file into buf (NUL-terminated). Returns bytes read, or -1. */
static long slurp(const char *path, char *buf, size_t cap)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    return (long)n;
}

/* Assert `path` is readable and its content parses as a non-negative integer. */
static void check_uint_file(const char *path, int positive)
{
    char buf[64];
    long n = slurp(path, buf, sizeof(buf));
    CHECK(n > 0, path);
    if (n > 0) {
        char *end = NULL;
        long v = strtol(buf, &end, 10);
        CHECK(end != buf && v >= 0 && (!positive || v > 0),
              path /* value is a plausible non-negative integer */);
    }
}

int main(void)
{
    TEST_START("procfs sysctl stubs");

    /* /proc/filesystems: readable and lists ext4 (the rootfs type). */
    {
        char buf[512];
        long n = slurp("/proc/filesystems", buf, sizeof(buf));
        CHECK(n > 0, "/proc/filesystems readable");
        CHECK(n > 0 && strstr(buf, "ext4") != NULL,
              "/proc/filesystems lists ext4");
    }

    check_uint_file("/proc/sys/vm/overcommit_memory", 0);
    check_uint_file("/proc/sys/vm/max_map_count", 1);
    check_uint_file("/proc/sys/fs/file-max", 1);
    check_uint_file("/proc/sys/fs/nr_open", 1);
    check_uint_file("/proc/sys/net/core/somaxconn", 1);

    TEST_DONE();
}
