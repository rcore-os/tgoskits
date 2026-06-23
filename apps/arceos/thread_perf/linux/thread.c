// thread_overhead_bench.c
// Measure Linux pthread creation/join overhead and pthread context-switch-like overhead.
//
// This benchmark intentionally mirrors apps/arceos/thread_perf:
//   - fixed iteration counts
//   - fixed warmup count
//   - create/join loop
//   - two-thread Atomic + sched_yield ping-pong
//
// Build:
//   gcc -O2 -pthread thread.c -o thread_overhead_bench
//
// Run:
//   taskset -c 0 ./thread_overhead_bench
//
// Notes:
//   1. Run with taskset to reduce CPU migration noise.
//   2. The switch test uses atomic polling plus sched_yield, not futex.
//   3. The reported context switch value is estimated as round-trip / 2.
//      It includes atomic polling, scheduler yield/wakeup, and thread switching cost.

#define _GNU_SOURCE

#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define CREATE_ITERS 100000ULL
#define SWITCH_ITERS 1000000ULL
#define WARMUP_ITERS 1000ULL

static atomic_uint turn = 0;
static atomic_bool ready = false;

struct switch_args {
    uint64_t total_iters;
};

static uint64_t nsec_now(void)
{
    struct timespec ts;

    if (clock_gettime(CLOCK_MONOTONIC_RAW, &ts) != 0) {
        perror("clock_gettime");
        exit(EXIT_FAILURE);
    }

    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static void wait_until(atomic_uint *var, unsigned int expected)
{
    while (atomic_load_explicit(var, memory_order_acquire) != expected) {
        sched_yield();
    }
}

static void *empty_thread_fn(void *arg)
{
    (void)arg;
    return NULL;
}

static void run_create_join_loop(uint64_t iters)
{
    for (uint64_t i = 0; i < iters; i++) {
        pthread_t tid;
        int ret;

        ret = pthread_create(&tid, NULL, empty_thread_fn, NULL);
        if (ret != 0) {
            fprintf(stderr, "pthread_create failed: %s\n", strerror(ret));
            exit(EXIT_FAILURE);
        }

        ret = pthread_join(tid, NULL);
        if (ret != 0) {
            fprintf(stderr, "pthread_join failed: %s\n", strerror(ret));
            exit(EXIT_FAILURE);
        }
    }
}

static void bench_thread_create(uint64_t iters, uint64_t warmup)
{
    uint64_t start;
    uint64_t end;
    double avg_ns;

    if (warmup > 0) {
        run_create_join_loop(warmup);
    }

    start = nsec_now();
    run_create_join_loop(iters);
    end = nsec_now();

    avg_ns = (double)(end - start) / (double)iters;

    printf("[thread create/join]\n");
    printf("iters: %llu\n", (unsigned long long)iters);
    printf("total: %.3f ms\n", (double)(end - start) / 1000000.0);
    printf("avg pthread_create + pthread_join: %.2f ns\n", avg_ns);
    printf("avg pthread_create + pthread_join: %.3f us\n\n", avg_ns / 1000.0);
}

static void *switch_worker_fn(void *arg)
{
    struct switch_args *args = (struct switch_args *)arg;

    atomic_store_explicit(&ready, true, memory_order_release);

    for (uint64_t i = 0; i < args->total_iters; i++) {
        wait_until(&turn, 1);
        atomic_store_explicit(&turn, 0, memory_order_release);
    }

    return NULL;
}

static void do_one_pingpong_round(void)
{
    atomic_store_explicit(&turn, 1, memory_order_release);
    wait_until(&turn, 0);
}

static void bench_thread_switch(uint64_t iters, uint64_t warmup)
{
    pthread_t tid;
    struct switch_args args;
    uint64_t start;
    uint64_t end;
    double roundtrip_ns;
    double estimated_switch_ns;
    int ret;

    atomic_store_explicit(&turn, 0, memory_order_release);
    atomic_store_explicit(&ready, false, memory_order_release);

    args.total_iters = warmup + iters;

    ret = pthread_create(&tid, NULL, switch_worker_fn, &args);
    if (ret != 0) {
        fprintf(stderr, "pthread_create failed: %s\n", strerror(ret));
        exit(EXIT_FAILURE);
    }

    while (!atomic_load_explicit(&ready, memory_order_acquire)) {
        sched_yield();
    }

    for (uint64_t i = 0; i < warmup; i++) {
        do_one_pingpong_round();
    }

    start = nsec_now();
    for (uint64_t i = 0; i < iters; i++) {
        do_one_pingpong_round();
    }
    end = nsec_now();

    ret = pthread_join(tid, NULL);
    if (ret != 0) {
        fprintf(stderr, "pthread_join failed: %s\n", strerror(ret));
        exit(EXIT_FAILURE);
    }

    roundtrip_ns = (double)(end - start) / (double)iters;
    estimated_switch_ns = roundtrip_ns / 2.0;

    printf("[thread yield ping-pong]\n");
    printf("iters: %llu\n", (unsigned long long)iters);
    printf("total: %.3f ms\n", (double)(end - start) / 1000000.0);
    printf("avg round-trip A->B->A: %.2f ns\n", roundtrip_ns);
    printf("estimated avg one context switch: %.2f ns\n", estimated_switch_ns);
    printf("estimated avg one context switch: %.3f us\n\n", estimated_switch_ns / 1000.0);
}

int main(void)
{
    printf("=== Linux thread performance benchmark ===\n\n");
    printf("fixed create/join iters: %llu\n", (unsigned long long)CREATE_ITERS);
    printf("fixed switch iters: %llu\n", (unsigned long long)SWITCH_ITERS);
    printf("fixed warmup iters: %llu\n", (unsigned long long)WARMUP_ITERS);
    printf("note: switch test uses atomic + sched_yield ping-pong, not Linux futex.\n\n");

    bench_thread_create(CREATE_ITERS, WARMUP_ITERS);
    bench_thread_switch(SWITCH_ITERS, WARMUP_ITERS);

    printf("=== thread performance benchmark complete ===\n");

    return EXIT_SUCCESS;
}
