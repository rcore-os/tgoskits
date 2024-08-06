#include <pthread.h>
#include <stdint.h>
#include <stdio.h>

#define __MAX_THREADS       16
#define __THREAD_STACK_SIZE (4096 * 4)

static char THREAD_STACKS[__MAX_THREADS][__THREAD_STACK_SIZE] __attribute__((__aligned__(16)));
static int THREAD_COUNT = 0;

extern int __clone(void *(*entry)(void *), void *stack, void *arg);

int pthread_create(pthread_t *restrict res, const void *restrict attrp, void *(*entry)(void *),
                   void *restrict arg)
{
    int thread_id = THREAD_COUNT++;
    void *newsp = THREAD_STACKS[thread_id] + __THREAD_STACK_SIZE;
    int tid = __clone(entry, arg, newsp);
    if (tid < 0) {
        return tid;
    }
    *res = tid;
    return 0;
}
