#include <sys/shm.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <string.h>

int main() {
    int passed = 1;

    void *addr = shmat(99999, NULL, 0);
    if (addr == (void *)-1) {
        printf("PASS: shmat with invalid shmid returns error (errno=%d=%s)\n",
               errno, strerror(errno));
    } else {
        printf("FAIL: shmat with invalid shmid succeeded (should have failed)\n");
        passed = 0;
    }

    addr = shmat(0, NULL, 0);
    if (addr == (void *)-1) {
        printf("PASS: shmat with shmid=0 returns error\n");
    } else {
        printf("FAIL: shmat with shmid=0 succeeded (should have failed)\n");
        passed = 0;
    }

    addr = shmat(-1, NULL, 0);
    if (addr == (void *)-1) {
        printf("PASS: shmat with negative shmid returns error\n");
    } else {
        printf("FAIL: shmat with negative shmid succeeded (should have failed)\n");
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
