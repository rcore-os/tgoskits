/*
 * bug-fallocate-zero-punch: zero-range and punch-hole modes must zero
 * existing bytes instead of rejecting the supported mode.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures = 0;

static void expect(int cond, const char *what)
{
    if (cond) {
        printf("PASS: %s\n", what);
    } else {
        printf("FAIL: %s errno=%d (%s)\n", what, errno, strerror(errno));
        failures++;
    }
}

int main(void)
{
    char tmpl[] = "/tmp/bug-fallocate-XXXXXX";
    int fd = mkstemp(tmpl);
    if (fd < 0) {
        printf("FAIL: mkstemp errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    expect(write(fd, "abcdefghij", 10) == 10, "write fixture");
    expect(fallocate(fd, FALLOC_FL_ZERO_RANGE, 2, 4) == 0,
           "FALLOC_FL_ZERO_RANGE succeeds");

    char buf[11] = {0};
    expect(pread(fd, buf, 10, 0) == 10, "read after ZERO_RANGE");
    expect(memcmp(buf, "ab\0\0\0\0ghij", 10) == 0,
           "ZERO_RANGE zeros the requested bytes");

    expect(pwrite(fd, "1234567890", 10, 0) == 10, "restore fixture");
    expect(fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, 0, 5) == 0,
           "PUNCH_HOLE|KEEP_SIZE succeeds");

    struct stat st;
    expect(fstat(fd, &st) == 0 && st.st_size == 10,
           "PUNCH_HOLE keeps file size");
    const char expected[10] = {
        0, 0, 0, 0, 0, '6', '7', '8', '9', '0',
    };
    memset(buf, 0, sizeof(buf));
    expect(pread(fd, buf, 10, 0) == 10, "read after PUNCH_HOLE");
    expect(memcmp(buf, expected, sizeof(expected)) == 0,
           "PUNCH_HOLE zeros the requested bytes");

    close(fd);
    unlink(tmpl);
    if (failures == 0) {
        printf("bug-fallocate-zero-punch: passed\n");
    }
    return failures == 0 ? 0 : 1;
}
