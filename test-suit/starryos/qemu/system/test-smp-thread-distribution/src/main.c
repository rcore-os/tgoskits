#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

enum { WORKERS = 2, LOAD_WORKERS = 4 };

struct observation {
    atomic_int ready;
    unsigned cpu;
    int error;
};

static atomic_int release_workers;

static void *worker(void *argument)
{
    struct observation *result = argument;
    if (syscall(SYS_getcpu, &result->cpu, NULL, NULL) != 0)
        result->error = errno;
    atomic_store_explicit(&result->ready, 1, memory_order_release);

    /* Keep earlier workers runnable while subsequent workers are admitted.
     * Observe each worker's entry once instead of sampling its later migration.
     * This checks user-visible distribution; balancing before entry can also
     * contribute, so it does not isolate the initial CPU selection policy. */
    while (!atomic_load_explicit(&release_workers, memory_order_acquire)) {
        atomic_signal_fence(memory_order_seq_cst);
    }
    return NULL;
}

static void *pinned_load(void *argument)
{
    struct observation *result = argument;
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(result->cpu, &affinity);
    if (syscall(SYS_sched_setaffinity, 0, sizeof(affinity), &affinity) != 0) {
        result->error = errno;
    } else {
        unsigned current;
        if (syscall(SYS_getcpu, &current, NULL, NULL) != 0)
            result->error = errno;
        else if (current != result->cpu)
            result->error = EINVAL;
    }
    atomic_store_explicit(&result->ready, 1, memory_order_release);
    while (!atomic_load_explicit(&release_workers, memory_order_acquire)) {
        atomic_signal_fence(memory_order_seq_cst);
    }
    return NULL;
}

int main(void)
{
    cpu_set_t original, allowed;
    if (syscall(SYS_sched_getaffinity, 0, sizeof(original), &original) < 0) {
        perror("FAIL: sched_getaffinity");
        return 1;
    }
    CPU_ZERO(&allowed);
    for (int cpu = 0; cpu < CPU_SETSIZE && CPU_COUNT(&allowed) < WORKERS; cpu++) {
        if (CPU_ISSET(cpu, &original))
            CPU_SET(cpu, &allowed);
    }
    if (CPU_COUNT(&allowed) != WORKERS) {
        printf("FAIL: SMP distribution requires two allowed CPUs\n");
        return 1;
    }
    if (syscall(SYS_sched_setaffinity, 0, sizeof(allowed), &allowed) != 0) {
        perror("FAIL: restrict test to two allowed CPUs");
        return 1;
    }

    pthread_t threads[WORKERS];
    struct observation results[WORKERS] = {0};
    pthread_t load_threads[LOAD_WORKERS];
    struct observation load[LOAD_WORKERS] = {0};
    int created = 0;
    int load_created = 0;
    int failed = 0;
    for (int index = 0; index < WORKERS; index++) {
        int error = pthread_create(&threads[index], NULL, worker, &results[index]);
        if (error != 0) {
            printf("FAIL: pthread_create: %s\n", strerror(error));
            failed = 1;
            break;
        }
        created++;
        while (!atomic_load_explicit(&results[index].ready, memory_order_acquire))
            syscall(SYS_sched_yield);

        if (index == 0) {
            if (results[0].error != 0 || results[0].cpu >= CPU_SETSIZE
                || !CPU_ISSET(results[0].cpu, &allowed)) {
                failed = 1;
                break;
            }
            /* The parent also contributes to placement demand. Load the first
             * worker's CPU so the next admission has a strictly lighter peer. */
            for (int extra = 0; extra < LOAD_WORKERS; extra++) {
                load[extra].cpu = results[0].cpu;
                error = pthread_create(&load_threads[extra], NULL, pinned_load, &load[extra]);
                if (error != 0) {
                    printf("FAIL: pthread_create load: %s\n", strerror(error));
                    failed = 1;
                    break;
                }
                load_created++;
                while (!atomic_load_explicit(&load[extra].ready, memory_order_acquire))
                    syscall(SYS_sched_yield);
                if (load[extra].error != 0) {
                    printf("FAIL: pin load to CPU %u: %s\n",
                           load[extra].cpu, strerror(load[extra].error));
                    failed = 1;
                    break;
                }
            }
            if (failed)
                break;
        }
    }

    atomic_store_explicit(&release_workers, 1, memory_order_release);
    cpu_set_t observed;
    CPU_ZERO(&observed);
    for (int index = 0; index < created; index++) {
        int error = pthread_join(threads[index], NULL);
        if (error != 0 || results[index].error != 0
            || results[index].cpu >= CPU_SETSIZE
            || !CPU_ISSET(results[index].cpu, &allowed)) {
            printf("FAIL: worker %d: join=%d getcpu=%d cpu=%u\n",
                   index, error, results[index].error, results[index].cpu);
            failed = 1;
        } else {
            CPU_SET(results[index].cpu, &observed);
        }
    }
    for (int index = 0; index < load_created; index++) {
        int error = pthread_join(load_threads[index], NULL);
        if (error != 0 || load[index].error != 0) {
            printf("FAIL: load worker %d: join=%d affinity=%d\n",
                   index, error, load[index].error);
            failed = 1;
        }
    }
    if (syscall(SYS_sched_setaffinity, 0, sizeof(original), &original) != 0) {
        perror("FAIL: restore affinity");
        failed = 1;
    }
    if (CPU_COUNT(&observed) != WORKERS) {
        printf("FAIL: new workers used %d distinct CPUs, expected %d\n",
               CPU_COUNT(&observed), WORKERS);
        failed = 1;
    }
    if (!failed)
        printf("DONE: new workers entered on two affinity-allowed CPUs\n");
    return failed;
}
