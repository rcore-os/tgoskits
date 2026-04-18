#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/ioctl.h>
#include <errno.h>
#include <string.h>

int main() {
    int passed = 1;
    pid_t pgid;

    // Test: tcsetpgrp should accept our own process group
    pgid = getpgrp();
    if (tcsetpgrp(STDIN_FILENO, pgid) == 0) {
        printf("PASS: tcsetpgrp with own pgid (%d) succeeded\n", pgid);
    } else {
        printf("FAIL: tcsetpgrp with own pgid (%d) failed: %s\n", pgid, strerror(errno));
        passed = 0;
    }

    // Test: tcsetpgrp with invalid pgid should return error
    if (tcsetpgrp(STDIN_FILENO, 99999) == -1) {
        printf("PASS: tcsetpgrp with invalid pgid returns error (errno=%d=%s)\n",
               errno, strerror(errno));
    } else {
        printf("FAIL: tcsetpgrp with invalid pgid succeeded (should have failed)\n");
        passed = 0;
    }

    // Test: tcgetpgrp should return the pgid we set
    pid_t ret_pgid = tcgetpgrp(STDIN_FILENO);
    if (ret_pgid == pgid) {
        printf("PASS: tcgetpgrp returns expected pgid (%d)\n", ret_pgid);
    } else {
        printf("FAIL: tcgetpgrp returned %d, expected %d\n", ret_pgid, pgid);
        passed = 0;
    }

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
