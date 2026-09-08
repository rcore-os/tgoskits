#include <pthread.h>
#include <stdint.h>
#include <stdio.h>

__attribute__((noinline)) static uintptr_t workload_leaf(void)
{
    volatile uintptr_t value = 1;
    for (unsigned int i = 0; i < 2000000; ++i) {
        value = value * 33 + i;
    }
    return value;
}

__attribute__((noinline)) static void *workload_thread(void *unused)
{
    (void)unused;
    return (void *)workload_leaf();
}

int main(void)
{
    pthread_t thread;
    void *result;
    if (pthread_create(&thread, NULL, workload_thread, NULL) != 0) {
        return 1;
    }
    void *expected = workload_thread(NULL);
    if (pthread_join(thread, &result) != 0 || result != expected) {
        return 2;
    }
    puts("QPERF_WORKLOAD_DONE");
    return 0;
}
