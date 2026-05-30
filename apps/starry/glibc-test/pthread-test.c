#define _GNU_SOURCE
#include <stdio.h>
#include <pthread.h>
#include <string.h>

static void *thread_func(void *arg) {
    const char *msg = (const char *)arg;
    printf("thread: %s\n", msg);
    return NULL;
}

int main(void) {
    pthread_t tid;
    int ret;

    ret = pthread_create(&tid, NULL, thread_func, "hello from thread");
    if (ret != 0) {
        printf("pthread_create failed: %s\n", strerror(ret));
        return 1;
    }

    pthread_join(tid, NULL);
    printf("pthread test OK\n");
    return 0;
}
