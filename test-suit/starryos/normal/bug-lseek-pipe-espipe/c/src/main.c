#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

int main() {
    int passed = 1;
    int pipefd[2];

    // Test: lseek on a pipe should return ESPIPE
    if (pipe(pipefd) < 0) {
        printf("SKIP: pipe() failed: %s\n", strerror(errno));
        return 0;
    }

    off_t ret = lseek(pipefd[0], 0, SEEK_SET);
    if (ret == -1 && errno == ESPIPE) {
        printf("PASS: lseek on pipe returns ESPIPE\n");
    } else if (ret == -1 && errno == EPIPE) {
        printf("FAIL: lseek on pipe returns EPIPE (should be ESPIPE)\n");
        passed = 0;
    } else if (ret == -1) {
        printf("FAIL: lseek on pipe returns errno=%d (%s), expected ESPIPE (%d)\n",
               errno, strerror(errno), ESPIPE);
        passed = 0;
    } else {
        printf("FAIL: lseek on pipe succeeded (should have failed with ESPIPE)\n");
        passed = 0;
    }

    close(pipefd[0]);
    close(pipefd[1]);

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
