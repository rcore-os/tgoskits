#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>

int main() {
    int passed = 1;
    char buf[256];

    // Test: read() on a directory should return EISDIR
    int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        // Try / if /tmp doesn't work
        fd = open("/", O_RDONLY | O_DIRECTORY);
    }
    if (fd < 0) {
        printf("SKIP: Could not open directory: %s\n", strerror(errno));
        return 0;
    }

    ssize_t ret = read(fd, buf, sizeof(buf));
    if (ret < 0 && errno == EISDIR) {
        printf("PASS: read() on directory returns EISDIR\n");
    } else if (ret < 0 && errno == EBADF) {
        printf("FAIL: read() on directory returns EBADF (should be EISDIR)\n");
        passed = 0;
    } else if (ret < 0) {
        printf("INFO: read() on directory returns errno=%d (%s)\n", errno, strerror(errno));
        // Some systems allow reading directory entries, so this isn't necessarily wrong
    } else {
        printf("INFO: read() on directory succeeded (some systems allow this for getdents)\n");
    }

    // Test: write() on a directory should return EISDIR
    ret = write(fd, "hello", 5);
    if (ret < 0 && errno == EISDIR) {
        printf("PASS: write() on directory returns EISDIR\n");
    } else if (ret < 0 && errno == EBADF) {
        printf("FAIL: write() on directory returns EBADF (should be EISDIR)\n");
        passed = 0;
    } else if (ret < 0) {
        printf("INFO: write() on directory returns errno=%d (%s)\n", errno, strerror(errno));
    } else {
        printf("FAIL: write() on directory succeeded (should have failed)\n");
        passed = 0;
    }

    close(fd);

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
