/*
 * bug-proc-maps-lseek-refresh: /proc/self/maps should reflect the latest VMA
 * layout when the same fd is rewound with lseek(fd, 0, SEEK_SET) and read
 * again after mmap/mprotect/munmap.
 *
 * The test primes the seq-file cache by reading /proc/self/maps once, then
 * performs:
 *   1. mmap() a guarded 3-page RW range
 *   2. mprotect() the middle page to force a VMA split
 *   3. munmap() the middle page to create a hole
 *
 * Each step reuses the same opened maps fd, seeks back to 0, and verifies that
 * the refreshed contents reflect the new VMA boundaries and permissions.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

enum {
    MAPS_BUF_SIZE = 128 * 1024,
    TEST_PAGES = 3,
    GUARD_PAGES = 2,
};

static ssize_t read_all_current(int fd, char *buf, size_t buf_size)
{
    size_t total = 0;

    while (total + 1 < buf_size) {
        ssize_t read_size = read(fd, buf + total, buf_size - 1 - total);
        if (read_size < 0) {
            return -1;
        }
        if (read_size == 0) {
            buf[total] = '\0';
            return (ssize_t)total;
        }
        total += (size_t)read_size;
    }

    errno = ENOSPC;
    return -1;
}

static ssize_t reread_from_start(int fd, char *buf, size_t buf_size)
{
    if (lseek(fd, 0, SEEK_SET) != 0) {
        return -1;
    }
    return read_all_current(fd, buf, buf_size);
}

static int has_vma_prefix(
    const char *maps,
    uintptr_t start,
    uintptr_t end,
    const char *perms
)
{
    char needle[64];
    const int needle_len = snprintf(
        needle,
        sizeof(needle),
        "%08lx-%08lx %s",
        (unsigned long)start,
        (unsigned long)end,
        perms
    );
    const char *line = maps;

    if (needle_len <= 0 || (size_t)needle_len >= sizeof(needle)) {
        return 0;
    }

    while (*line != '\0') {
        const char *newline = strchr(line, '\n');
        size_t line_len = newline ? (size_t)(newline - line) : strlen(line);

        if (line_len >= (size_t)needle_len &&
            memcmp(line, needle, (size_t)needle_len) == 0) {
            return 1;
        }

        if (newline == NULL) {
            break;
        }
        line = newline + 1;
    }

    return 0;
}

static void print_maps_on_failure(const char *maps)
{
    printf("---- /proc/self/maps ----\n%s", maps);
    if (maps[0] != '\0' && maps[strlen(maps) - 1] != '\n') {
        putchar('\n');
    }
    printf("-------------------------\n");
}

int main(void)
{
    static char maps_buf[MAPS_BUF_SIZE];

    const long page_size = sysconf(_SC_PAGESIZE);
    const size_t total_pages = TEST_PAGES + GUARD_PAGES;
    const size_t total_size = (size_t)page_size * total_pages;
    int maps_fd = -1;
    void *reserved = MAP_FAILED;
    unsigned char *mapping;
    unsigned char *middle;
    unsigned char *right;

    printf("=== bug-proc-maps-lseek-refresh ===\n");
    printf("Expected: one opened /proc/self/maps fd is refreshed by lseek(0)\n");
    printf("          after mmap/mprotect/munmap change the VMA layout.\n\n");

    if (page_size != 4096) {
        printf("FAIL: unexpected page size %ld, expected 4096\n", page_size);
        printf("TEST FAILED\n");
        return 1;
    }

    maps_fd = open("/proc/self/maps", O_RDONLY);
    if (maps_fd < 0) {
        printf("FAIL: open(/proc/self/maps): %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    if (read_all_current(maps_fd, maps_buf, sizeof(maps_buf)) < 0) {
        printf("FAIL: initial read(/proc/self/maps): %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    reserved = mmap(NULL, total_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reserved == MAP_FAILED) {
        printf("FAIL: reserve guard region: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    mapping = mmap(
        (unsigned char *)reserved + page_size,
        (size_t)page_size * TEST_PAGES,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
        -1,
        0
    );
    if (mapping == MAP_FAILED || mapping != (unsigned char *)reserved + page_size) {
        printf("FAIL: mmap test range: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    middle = mapping + page_size;
    right = middle + page_size;
    mapping[0] = 0x11;
    middle[0] = 0x22;
    right[0] = 0x33;

    if (reread_from_start(maps_fd, maps_buf, sizeof(maps_buf)) < 0) {
        printf("FAIL: reread maps after mmap: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }
    if (!has_vma_prefix(
            maps_buf,
            (uintptr_t)mapping,
            (uintptr_t)(mapping + page_size * TEST_PAGES),
            "rw-p")) {
        printf("FAIL: mmap result not reflected on same maps fd after lseek\n");
        print_maps_on_failure(maps_buf);
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    if (mprotect(middle, (size_t)page_size, PROT_READ) != 0) {
        printf("FAIL: mprotect middle page: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    if (reread_from_start(maps_fd, maps_buf, sizeof(maps_buf)) < 0) {
        printf("FAIL: reread maps after mprotect: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }
    if (!has_vma_prefix(maps_buf, (uintptr_t)mapping, (uintptr_t)middle, "rw-p") ||
        !has_vma_prefix(
            maps_buf,
            (uintptr_t)middle,
            (uintptr_t)(middle + page_size),
            "r--p") ||
        !has_vma_prefix(
            maps_buf,
            (uintptr_t)right,
            (uintptr_t)(right + page_size),
            "rw-p") ||
        has_vma_prefix(
            maps_buf,
            (uintptr_t)mapping,
            (uintptr_t)(mapping + page_size * TEST_PAGES),
            "rw-p")) {
        printf("FAIL: mprotect split not reflected on same maps fd after lseek\n");
        print_maps_on_failure(maps_buf);
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    if (munmap(middle, (size_t)page_size) != 0) {
        printf("FAIL: munmap middle page: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    if (reread_from_start(maps_fd, maps_buf, sizeof(maps_buf)) < 0) {
        printf("FAIL: reread maps after munmap: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }
    if (!has_vma_prefix(maps_buf, (uintptr_t)mapping, (uintptr_t)middle, "rw-p") ||
        !has_vma_prefix(
            maps_buf,
            (uintptr_t)right,
            (uintptr_t)(right + page_size),
            "rw-p") ||
        has_vma_prefix(
            maps_buf,
            (uintptr_t)middle,
            (uintptr_t)(middle + page_size),
            "r--p")) {
        printf("FAIL: munmap result not reflected on same maps fd after lseek\n");
        print_maps_on_failure(maps_buf);
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    if (munmap(mapping, (size_t)page_size) != 0 || munmap(right, (size_t)page_size) != 0) {
        printf("FAIL: munmap remaining test pages: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    if (reread_from_start(maps_fd, maps_buf, sizeof(maps_buf)) < 0) {
        printf("FAIL: reread maps after full cleanup: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }
    if (has_vma_prefix(maps_buf, (uintptr_t)mapping, (uintptr_t)middle, "rw-p") ||
        has_vma_prefix(
            maps_buf,
            (uintptr_t)right,
            (uintptr_t)(right + page_size),
            "rw-p")) {
        printf("FAIL: cleaned-up VMAs still visible on same maps fd after lseek\n");
        print_maps_on_failure(maps_buf);
        printf("TEST FAILED\n");
        close(maps_fd);
        return 1;
    }

    close(maps_fd);
    printf("PASS: same /proc/self/maps fd refreshed after mmap/mprotect/munmap\n");
    printf("TEST PASSED\n");
    return 0;
}
