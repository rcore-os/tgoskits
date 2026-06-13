/*
 * SMP work-stealing stress test.
 *
 * Spawns more CPU-bound threads than available CPUs so that idle CPUs
 * must steal runnable tasks from remote run queues.  Workers contend on
 * a shared mutex to verify that work-stealing and mutex wait/wake paths
 * co-exist without kernel panics or lost wakeups.
 *
 * A successful run is simply surviving without a kernel panic and having
 * every worker complete all iterations.
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <sys/mman.h>
#include <unistd.h>

#define N_WORKERS      8
#define N_CPU_BOUND    2    /* workers pinned to specific CPUs */
#define N_FREE         (N_WORKERS - N_CPU_BOUND)
#define ITERATIONS     5000

/* Per-worker completion flag plus shared mutex and counter. */
struct shared {
    pthread_mutex_t lock;
    unsigned int    counter;
    volatile int    worker_done[N_WORKERS];
};

struct worker_arg {
    int             id;
    int             cpu;        /* -1 = unbound */
    struct shared  *s;
    volatile int    ok;
};

static void pin_to_cpu(int cpu)
{
    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(cpu, &cpuset);
    (void)sched_setaffinity(0, sizeof(cpuset), &cpuset);
}

static void *worker(void *arg)
{
    struct worker_arg *wa = (struct worker_arg *)arg;
    wa->ok = 0;

    if (wa->cpu >= 0)
        pin_to_cpu(wa->cpu);

    /* CPU-bound work with occasional mutex contention. */
    for (int i = 0; i < ITERATIONS; i++) {
        /*
         * Heavy computation without locks — keeps the worker on the
         * run queue and generates load that triggers work-stealing.
         */
        volatile unsigned long dummy = 0;
        for (int j = 0; j < 200; j++)
            dummy = dummy * 1103515245 + 12345;

        /* Brief mutex section to exercise mutex + steal interaction. */
        if ((i & 31) == 0) {
            pthread_mutex_lock(&wa->s->lock);
            wa->s->counter++;
            pthread_mutex_unlock(&wa->s->lock);
        }

        /* Periodic yield to increase scheduling churn. */
        if ((i & 127) == 0)
            sched_yield();
    }

    /* Final mutex round. */
    pthread_mutex_lock(&wa->s->lock);
    wa->s->counter++;
    wa->s->worker_done[wa->id] = 1;
    pthread_mutex_unlock(&wa->s->lock);

    wa->ok = 1;
    return NULL;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("work-stealing under SMP load");

    struct shared *s = mmap(NULL, sizeof(*s),
                            PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(s != MAP_FAILED, "mmap shared state");

    memset(s, 0, sizeof(*s));
    CHECK(pthread_mutex_init(&s->lock, NULL) == 0, "init shared mutex");

    pthread_t threads[N_WORKERS];
    struct worker_arg args[N_WORKERS];
    int created = 0;

    /* Workers pinned to specific CPUs. */
    for (int i = 0; i < N_CPU_BOUND; i++) {
        args[i] = (struct worker_arg){
            .id   = i,
            .cpu  = i,
            .s    = s,
            .ok   = 0,
        };
        if (pthread_create(&threads[i], NULL, worker, &args[i]) != 0)
            break;
        created++;
    }

    /* Free workers — may run anywhere, exercising work-stealing. */
    for (int i = N_CPU_BOUND; i < N_WORKERS; i++) {
        args[i] = (struct worker_arg){
            .id   = i,
            .cpu  = -1,
            .s    = s,
            .ok   = 0,
        };
        if (pthread_create(&threads[i], NULL, worker, &args[i]) != 0)
            break;
        created++;
    }
    CHECK(created == N_WORKERS, "spawn all workers");

    for (int i = 0; i < created; i++) {
        CHECK(pthread_join(threads[i], NULL) == 0, "join worker");
    }

    int all_ok = 1;
    int done_count = 0;
    for (int i = 0; i < created; i++) {
        all_ok &= args[i].ok;
        done_count += s->worker_done[i];
    }
    CHECK(all_ok, "all workers completed successfully");
    CHECK(done_count == N_WORKERS, "all workers marked done");

    printf("shared counter = %u\n", s->counter);

    pthread_mutex_destroy(&s->lock);
    munmap(s, sizeof(*s));
    TEST_DONE();
}
