#define _GNU_SOURCE
#include <errno.h>
/* Linux include/uapi/linux/membarrier.h; the freestanding musl sysroot
 * intentionally does not ship Linux UAPI headers. */
enum {
    MEMBARRIER_CMD_QUERY = 0,
    MEMBARRIER_CMD_PRIVATE_EXPEDITED = 1 << 3,
    MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED = 1 << 4,
};
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* Linux membarrier scenarios A/C: one full syscall barrier must pair with
 * compiler ordering on the remote side, including a return from idle sleep.
 * Round boundaries reset observations, but no pthread synchronization occurs
 * between the tested stores and loads. QEMU is a runtime witness, not exhaustive
 * weak-memory model checking. */
static pthread_barrier_t start_round, end_round;
static atomic_int x, y;
static int remote_read, remote_cpu;
enum { ROUNDS = 512 };

static void fail(const char *operation)
{
    perror(operation);
    exit(1);
}

static void rendezvous(pthread_barrier_t *barrier)
{
    int error = pthread_barrier_wait(barrier);
    if (error != 0 && error != PTHREAD_BARRIER_SERIAL_THREAD) {
        errno = error;
        fail("pthread_barrier_wait");
    }
}

static void bind_cpu(int cpu)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    if (sched_setaffinity(0, sizeof(set), &set) != 0)
        fail("sched_setaffinity");
}

static void *remote(void *unused)
{
    (void)unused;
    bind_cpu(remote_cpu);
    for (int round = 0; round < ROUNDS; ++round) {
        rendezvous(&start_round);
        if (round & 1) {
            struct timespec delay = {.tv_nsec = 1000};
            while (nanosleep(&delay, &delay) != 0) {
                if (errno != EINTR)
                    fail("nanosleep");
            }
        }
        atomic_store_explicit(&y, 1, memory_order_relaxed);
        atomic_signal_fence(memory_order_seq_cst);
        remote_read = atomic_load_explicit(&x, memory_order_relaxed);
        rendezvous(&end_round);
    }
    return NULL;
}

int test_membarrier_user_return(void)
{
    cpu_set_t original;
    if (sched_getaffinity(0, sizeof(original), &original) != 0)
        fail("sched_getaffinity");
    int local_cpu = -1;
    remote_cpu = -1;
    for (int cpu = 0; cpu < CPU_SETSIZE; ++cpu) {
        if (!CPU_ISSET(cpu, &original))
            continue;
        if (local_cpu == -1)
            local_cpu = cpu;
        else { remote_cpu = cpu; break; }
    }
    if (remote_cpu == -1) {
        fprintf(stderr, "membarrier lifecycle requires two online CPUs\n");
        return 1;
    }
    long supported = syscall(SYS_membarrier, MEMBARRIER_CMD_QUERY, 0, 0);
    if (supported < 0 || !(supported & MEMBARRIER_CMD_PRIVATE_EXPEDITED)
        || !(supported & MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED)) {
        fprintf(stderr, "private expedited membarrier is unavailable\n");
        return 1;
    }
    if (syscall(SYS_membarrier, MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 0, 0) != 0)
        fail("register private membarrier");
    bind_cpu(local_cpu);
    int error = pthread_barrier_init(&start_round, NULL, 2);
    if (error) { errno = error; fail("start barrier init"); }
    error = pthread_barrier_init(&end_round, NULL, 2);
    if (error) { errno = error; fail("end barrier init"); }
    pthread_t thread;
    error = pthread_create(&thread, NULL, remote, NULL);
    if (error) { errno = error; fail("pthread_create"); }
    for (int round = 0; round < ROUNDS; ++round) {
        atomic_store_explicit(&x, 0, memory_order_relaxed);
        atomic_store_explicit(&y, 0, memory_order_relaxed);
        rendezvous(&start_round);
        atomic_store_explicit(&x, 1, memory_order_relaxed);
        if (syscall(SYS_membarrier, MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0, 0) != 0)
            fail("private expedited membarrier");
        int local_read = atomic_load_explicit(&y, memory_order_relaxed);
        rendezvous(&end_round);
        if (local_read == 0 && remote_read == 0) {
            fprintf(stderr, "forbidden membarrier result in round %d\n", round);
            exit(1);
        }
    }
    error = pthread_join(thread, NULL);
    if (error) { errno = error; fail("pthread_join"); }
    pthread_barrier_destroy(&start_round);
    pthread_barrier_destroy(&end_round);
    if (sched_setaffinity(0, sizeof(original), &original) != 0)
        fail("restore affinity");
    puts("MM_MEMBARRIER_USER_RETURN_PASSED");
    return 0;
}
