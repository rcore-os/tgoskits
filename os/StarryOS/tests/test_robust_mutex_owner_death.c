#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>

static pthread_mutex_t robust_mutex;

void *owner_thread(void *arg) {
    pthread_mutex_lock(&robust_mutex);
    return NULL;
}

int main() {
    pthread_mutexattr_t attr;
    pthread_t thread;
    int ret;
    int passed = 1;

    pthread_mutexattr_init(&attr);
    pthread_mutexattr_setrobust(&attr, PTHREAD_MUTEX_ROBUST);
    pthread_mutex_init(&robust_mutex, &attr);
    pthread_mutexattr_destroy(&attr);

    pthread_create(&thread, NULL, owner_thread, NULL);
    pthread_join(thread, NULL);

    ret = pthread_mutex_lock(&robust_mutex);
    if (ret == EOWNERDEAD) {
        printf("PASS: pthread_mutex_lock returned EOWNERDEAD after owner died\n");
        pthread_mutex_consistent(&robust_mutex);
        pthread_mutex_unlock(&robust_mutex);
    } else if (ret == 0) {
        printf("FAIL: pthread_mutex_lock succeeded (should have returned EOWNERDEAD)\n");
        passed = 0;
        pthread_mutex_unlock(&robust_mutex);
    } else {
        printf("FAIL: pthread_mutex_lock returned %d (%s), expected EOWNERDEAD (%d)\n",
               ret, strerror(ret), EOWNERDEAD);
        passed = 0;
    }

    pthread_mutex_destroy(&robust_mutex);

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
