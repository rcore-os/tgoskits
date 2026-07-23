#include "test_framework.h"
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

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

static long committed_as_kb(void)
{
    char buf[4096];
    long n = slurp("/proc/meminfo", buf, sizeof(buf));
    if (n <= 0) {
        return -1;
    }
    char *line = strstr(buf, "Committed_AS:");
    return line == NULL ? -1 : strtol(line + strlen("Committed_AS:"), NULL, 10);
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

    {
        char buf[64];
        long n = slurp("/proc/sys/vm/overcommit_memory", buf, sizeof(buf));
        char *end = NULL;
        long mode = n > 0 ? strtol(buf, &end, 10) : -1;
        CHECK(n > 0 && end != buf && mode == 1,
              "default Starry build reports overcommit_memory=1");
    }
    check_uint_file("/proc/sys/vm/max_map_count", 1);
    check_uint_file("/proc/sys/fs/file-max", 1);
    check_uint_file("/proc/sys/fs/nr_open", 1);
    check_uint_file("/proc/sys/net/core/somaxconn", 1);

    {
        const size_t page = 4096;
        long baseline = committed_as_kb();
        void *private_ro = mmap(NULL, page, PROT_READ,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(baseline >= 0, "/proc/meminfo reports Committed_AS");
        CHECK(private_ro != MAP_FAILED, "private anonymous mmap succeeds");
        if (baseline >= 0 && private_ro != MAP_FAILED) {
            CHECK(committed_as_kb() == baseline,
                  "read-only private anonymous mmap does not consume commit");
            CHECK(mprotect(private_ro, page, PROT_READ | PROT_WRITE) == 0 &&
                      committed_as_kb() == baseline + (long)(page / 1024),
                  "writable private anonymous mmap consumes per-process commit");
            CHECK(mprotect(private_ro, page, PROT_READ) == 0 &&
                      committed_as_kb() == baseline,
                  "removing private write permission releases commit");
            CHECK(munmap(private_ro, page) == 0 && committed_as_kb() == baseline,
                  "unmapping read-only private anonymous memory preserves commit");
        }

        void *shared_ro = mmap(NULL, page, PROT_READ,
                               MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        CHECK(shared_ro != MAP_FAILED, "shared anonymous mmap succeeds");
        if (baseline >= 0 && shared_ro != MAP_FAILED) {
            CHECK(committed_as_kb() == baseline + (long)(page / 1024),
                  "shared anonymous owner consumes one commit charge");
            CHECK(munmap(shared_ro, page) == 0 && committed_as_kb() == baseline,
                  "last shared anonymous owner releases its commit charge");
        }
    }

    TEST_DONE();
}
