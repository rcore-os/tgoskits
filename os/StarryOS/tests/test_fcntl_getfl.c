#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>

int main() {
    int fd;
    int flags;
    int passed = 1;
    const char *test_file = "/tmp/test_fcntl_getfl";

    // Test 1: Open file O_RDONLY and check F_GETFL
    fd = open(test_file, O_CREAT | O_RDONLY, 0644);
    if (fd < 0) {
        printf("FAIL: Could not open test file O_RDONLY: %s\n", strerror(errno));
        return 1;
    }
    flags = fcntl(fd, F_GETFL);
    if ((flags & O_ACCMODE) == O_RDONLY) {
        printf("PASS: O_RDONLY file returns O_RDONLY from F_GETFL\n");
    } else {
        printf("FAIL: O_RDONLY file returns access mode %d, expected O_RDONLY (%d)\n",
               flags & O_ACCMODE, O_RDONLY);
        passed = 0;
    }
    close(fd);

    // Test 2: Open file O_WRONLY and check F_GETFL
    fd = open(test_file, O_WRONLY);
    if (fd < 0) {
        printf("FAIL: Could not open test file O_WRONLY: %s\n", strerror(errno));
        return 1;
    }
    flags = fcntl(fd, F_GETFL);
    if ((flags & O_ACCMODE) == O_WRONLY) {
        printf("PASS: O_WRONLY file returns O_WRONLY from F_GETFL\n");
    } else {
        printf("FAIL: O_WRONLY file returns access mode %d, expected O_WRONLY (%d)\n",
               flags & O_ACCMODE, O_WRONLY);
        passed = 0;
    }
    close(fd);

    // Test 3: Open file O_RDWR and check F_GETFL
    fd = open(test_file, O_RDWR);
    if (fd < 0) {
        printf("FAIL: Could not open test file O_RDWR: %s\n", strerror(errno));
        return 1;
    }
    flags = fcntl(fd, F_GETFL);
    if ((flags & O_ACCMODE) == O_RDWR) {
        printf("PASS: O_RDWR file returns O_RDWR from F_GETFL\n");
    } else {
        printf("FAIL: O_RDWR file returns access mode %d, expected O_RDWR (%d)\n",
               flags & O_ACCMODE, O_RDWR);
        passed = 0;
    }
    close(fd);

    // Test 4: Open file O_RDONLY | O_NONBLOCK and check both flags
    fd = open(test_file, O_RDONLY | O_NONBLOCK);
    if (fd < 0) {
        printf("FAIL: Could not open test file O_RDONLY|O_NONBLOCK: %s\n", strerror(errno));
        return 1;
    }
    flags = fcntl(fd, F_GETFL);
    if ((flags & O_ACCMODE) == O_RDONLY && (flags & O_NONBLOCK)) {
        printf("PASS: O_RDONLY|O_NONBLOCK file returns correct flags from F_GETFL\n");
    } else {
        printf("FAIL: O_RDONLY|O_NONBLOCK file returns flags 0x%x, expected O_RDONLY|O_NONBLOCK\n", flags);
        passed = 0;
    }
    close(fd);

    unlink(test_file);

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
