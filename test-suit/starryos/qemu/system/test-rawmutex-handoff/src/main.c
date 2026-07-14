/*
 * Stress StarryOS SMP mutex wait/wake handoff through the process address
 * space lock.
 *
 * The M6 guest self-build exposed a kernel panic where RawMutex handed
 * ownership directly to a sleeping waiter before the waiter returned a guard.
 * Under SMP load this left a transient owner id naming a task that could reach
 * another page-fault/aspace-lock path and panic as "tried to acquire mutex it
 * already owns".
 *
 * This test keeps several threads in one process contending on the same
 * address-space mutex by repeatedly creating, faulting, protecting, and
 * unmapping anonymous VMAs. A buggy handoff path can surface as a kernel panic;
 * success is surviving the stress with every worker completing.
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifdef __linux__
#include <sched.h>
#include <sys/syscall.h>
#endif

#define N_WORKERS 4
#define ROUNDS 160
#define PAGES_PER_MAP 8
#define PAGE_SIZE 4096
#define MAP_SIZE ((size_t)PAGES_PER_MAP * PAGE_SIZE)

struct worker_arg {
    int cpu;
    int id;
    struct start_barrier *barrier;
    int ok;
};

struct start_barrier {
    pthread_mutex_t lock;
    pthread_cond_t cond;
    int count;
    int trip;
};

static int start_barrier_init(struct start_barrier *barrier, int trip)
{
    memset(barrier, 0, sizeof(*barrier));
    barrier->trip = trip;
    if (pthread_mutex_init(&barrier->lock, NULL) != 0) {
        return -1;
    }
    if (pthread_cond_init(&barrier->cond, NULL) != 0) {
        pthread_mutex_destroy(&barrier->lock);
        return -1;
    }
    return 0;
}

static void start_barrier_wait(struct start_barrier *barrier)
{
    pthread_mutex_lock(&barrier->lock);
    barrier->count++;
    if (barrier->count == barrier->trip) {
        pthread_cond_broadcast(&barrier->cond);
    } else {
        while (barrier->count < barrier->trip) {
            pthread_cond_wait(&barrier->cond, &barrier->lock);
        }
    }
    pthread_mutex_unlock(&barrier->lock);
}

static void start_barrier_destroy(struct start_barrier *barrier)
{
    pthread_cond_destroy(&barrier->cond);
    pthread_mutex_destroy(&barrier->lock);
}

static pid_t my_tid(void)
{
#ifdef __linux__
    return (pid_t)syscall(SYS_gettid);
#else
    return getpid();
#endif
}

static void pin_to_cpu(int cpu)
{
#ifdef __linux__
    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(cpu, &cpuset);
    (void)sched_setaffinity(0, sizeof(cpuset), &cpuset);
#else
    (void)cpu;
#endif
}

static void touch_pages(unsigned char *p, int id, int round)
{
    for (int page = 0; page < PAGES_PER_MAP; page++) {
        size_t off = (size_t)page * PAGE_SIZE;
        p[off] = (unsigned char)(id * 17 + round + page);
    }
}

static void *worker(void *arg)
{
    struct worker_arg *wa = (struct worker_arg *)arg;
    wa->ok = 0;

    pin_to_cpu(wa->cpu);

    start_barrier_wait(wa->barrier);

    for (int round = 0; round < ROUNDS; round++) {
        unsigned char *p = mmap(NULL, MAP_SIZE, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            printf("worker %d tid %d mmap failed at round %d\n",
                   wa->id, my_tid(), round);
            return NULL;
        }

        touch_pages(p, wa->id, round);

        if (mprotect(p, MAP_SIZE, PROT_READ) != 0) {
            printf("worker %d tid %d mprotect R failed at round %d\n",
                   wa->id, my_tid(), round);
            munmap(p, MAP_SIZE);
            return NULL;
        }

        volatile unsigned int sum = 0;
        for (int page = 0; page < PAGES_PER_MAP; page++) {
            sum += p[(size_t)page * PAGE_SIZE];
        }
        (void)sum;

        if (mprotect(p, MAP_SIZE, PROT_READ | PROT_WRITE) != 0) {
            printf("worker %d tid %d mprotect RW failed at round %d\n",
                   wa->id, my_tid(), round);
            munmap(p, MAP_SIZE);
            return NULL;
        }

        touch_pages(p, wa->id, round + 1);

        if (munmap(p, MAP_SIZE) != 0) {
            printf("worker %d tid %d munmap failed at round %d\n",
                   wa->id, my_tid(), round);
            return NULL;
        }

        if ((round & 7) == 0) {
            sched_yield();
        }
    }

    wa->ok = 1;
    return NULL;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("rawmutex handoff under SMP aspace contention");

    struct start_barrier barrier;
    CHECK(start_barrier_init(&barrier, N_WORKERS) == 0,
          "create start barrier");

    pthread_t threads[N_WORKERS];
    struct worker_arg args[N_WORKERS];
    int created = 0;
    for (int i = 0; i < N_WORKERS; i++) {
        args[i] = (struct worker_arg) {
            .cpu = i,
            .id = i,
            .barrier = &barrier,
            .ok = 0,
        };
        if (pthread_create(&threads[i], NULL, worker, &args[i]) != 0) {
            break;
        }
        created++;
    }
    CHECK(created == N_WORKERS, "spawn all mmap stress workers");

    for (int i = 0; i < created; i++) {
        CHECK(pthread_join(threads[i], NULL) == 0, "join worker");
    }

    int all_ok = 1;
    for (int i = 0; i < created; i++) {
        all_ok &= args[i].ok;
    }
    CHECK(all_ok, "all workers completed mmap/protect/unmap stress");

    start_barrier_destroy(&barrier);
    TEST_DONE();
}
