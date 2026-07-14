/*
 * cpu-concurrency: pure-CPU single-core cooperative-concurrency correctness carpet.
 *
 * StarryOS runs on one vCPU (SMP off by default). These sub-tests do NOT measure
 * throughput; they assert that the kernel's thread/sync primitives - clone/futex/
 * the RR scheduler - deliver POSIX-correct cooperative concurrency on a single core:
 * every parallel pattern's result must equal a deterministic sequential reference.
 *
 * Each C-library primitive used here (pthread_mutex/cond/rwlock/barrier, sem_t,
 * atomics) reduces on musl to the FUTEX_* + clone/nanosleep syscalls, so a failure
 * pinpoints a kernel gap, not a userspace one. Semantics are aligned to POSIX /
 * Linux: sem_t is a counting semaphore (sem_overview(7)), pthread_rwlock guarantees
 * readers never observe a torn write, condvar wait/signal has no lost-wakeup, and
 * sched_yield gives every runnable thread forward progress under SCHED_OTHER RR.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <semaphore.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <time.h>

/* linux/futex.h is not in the musl cross sysroot; the ABI values are stable. */
#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#endif

static int g_pass = 0;
static int g_fail = 0;

static void ok(const char *name, int cond) {
    if (cond) {
        g_pass++;
        printf("PASS %s\n", name);
    } else {
        g_fail++;
        printf("FAIL %s\n", name);
    }
    fflush(stdout);
}

/* ---- 1. Parallel reduction: 8 threads sum+max a partitioned range ---------- */
#define RED_THREADS 8
#define RED_N 1000000u

struct red_arg {
    uint32_t lo, hi;
    uint64_t sum;
    uint32_t max;
};

static void *red_worker(void *p) {
    struct red_arg *a = p;
    uint64_t s = 0;
    uint32_t m = 0;
    for (uint32_t i = a->lo; i < a->hi; i++) {
        s += i;
        if (i > m) m = i;
    }
    a->sum = s;
    a->max = m;
    return NULL;
}

static void test_parallel_reduction(void) {
    pthread_t th[RED_THREADS];
    struct red_arg arg[RED_THREADS];
    uint32_t chunk = RED_N / RED_THREADS;
    for (int t = 0; t < RED_THREADS; t++) {
        arg[t].lo = (uint32_t)t * chunk;
        arg[t].hi = (t == RED_THREADS - 1) ? RED_N : (uint32_t)(t + 1) * chunk;
        arg[t].sum = 0;
        arg[t].max = 0;
        pthread_create(&th[t], NULL, red_worker, &arg[t]);
    }
    uint64_t total = 0;
    uint32_t gmax = 0;
    for (int t = 0; t < RED_THREADS; t++) {
        pthread_join(th[t], NULL);
        total += arg[t].sum;
        if (arg[t].max > gmax) gmax = arg[t].max;
    }
    uint64_t ref = (uint64_t)(RED_N - 1) * RED_N / 2; /* Σ[0,N) = N(N-1)/2 */
    ok("reduction_sum", total == ref);
    ok("reduction_max", gmax == RED_N - 1);
}

/* ---- 2. Producer/consumer bounded queue (mutex + 2 condvars) --------------- */
#define PC_CAP 16
#define PC_PROD 4
#define PC_CONS 4
#define PC_PER_PROD 25000u        /* 4 * 25000 = 100000 items */
#define PC_TOTAL (PC_PROD * PC_PER_PROD)

struct pc_item { uint32_t producer; uint32_t seq; };

static struct {
    struct pc_item buf[PC_CAP];
    int head, tail, count;
    int closed;
    pthread_mutex_t mtx;
    pthread_cond_t not_full;
    pthread_cond_t not_empty;
} pcq;

static uint64_t pc_prod_checksum;   /* XOR-fold of every produced key, guarded by mtx */
static uint64_t pc_cons_checksum;
static uint32_t pc_consumed;
static uint32_t pc_last_seq[PC_PROD]; /* last seq seen per producer (FIFO check) */
static int pc_fifo_ok = 1;

static uint64_t pc_key(uint32_t prod, uint32_t seq) {
    return ((uint64_t)prod << 40) ^ (uint64_t)seq * 2654435761u;
}

static void *pc_producer(void *p) {
    uint32_t id = (uint32_t)(uintptr_t)p;
    for (uint32_t s = 0; s < PC_PER_PROD; s++) {
        pthread_mutex_lock(&pcq.mtx);
        while (pcq.count == PC_CAP)
            pthread_cond_wait(&pcq.not_full, &pcq.mtx);
        pcq.buf[pcq.tail] = (struct pc_item){ id, s };
        pcq.tail = (pcq.tail + 1) % PC_CAP;
        pcq.count++;
        pc_prod_checksum ^= pc_key(id, s);
        pthread_cond_signal(&pcq.not_empty);
        pthread_mutex_unlock(&pcq.mtx);
    }
    return NULL;
}

static void *pc_consumer(void *unused) {
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&pcq.mtx);
        while (pcq.count == 0 && !pcq.closed)
            pthread_cond_wait(&pcq.not_empty, &pcq.mtx);
        if (pcq.count == 0 && pcq.closed) {
            pthread_mutex_unlock(&pcq.mtx);
            return NULL;
        }
        struct pc_item it = pcq.buf[pcq.head];
        pcq.head = (pcq.head + 1) % PC_CAP;
        pcq.count--;
        pc_consumed++;
        pc_cons_checksum ^= pc_key(it.producer, it.seq);
        /* FIFO within a producer: seq must be strictly increasing per producer */
        if (it.producer < PC_PROD) {
            if (pc_consumed > 1 && pc_last_seq[it.producer] != UINT32_MAX &&
                it.seq != 0 && it.seq <= pc_last_seq[it.producer])
                pc_fifo_ok = 0;
            pc_last_seq[it.producer] = it.seq;
        }
        pthread_cond_signal(&pcq.not_full);
        pthread_mutex_unlock(&pcq.mtx);
    }
}

static void test_producer_consumer(void) {
    memset(&pcq, 0, sizeof(pcq));
    pthread_mutex_init(&pcq.mtx, NULL);
    pthread_cond_init(&pcq.not_full, NULL);
    pthread_cond_init(&pcq.not_empty, NULL);
    for (int i = 0; i < PC_PROD; i++) pc_last_seq[i] = UINT32_MAX;

    pthread_t prod[PC_PROD], cons[PC_CONS];
    for (int i = 0; i < PC_CONS; i++)
        pthread_create(&cons[i], NULL, pc_consumer, NULL);
    for (int i = 0; i < PC_PROD; i++)
        pthread_create(&prod[i], NULL, pc_producer, (void *)(uintptr_t)i);
    for (int i = 0; i < PC_PROD; i++) pthread_join(prod[i], NULL);

    pthread_mutex_lock(&pcq.mtx);
    pcq.closed = 1;
    pthread_cond_broadcast(&pcq.not_empty);
    pthread_mutex_unlock(&pcq.mtx);
    for (int i = 0; i < PC_CONS; i++) pthread_join(cons[i], NULL);

    ok("pc_count", pc_consumed == PC_TOTAL);
    ok("pc_checksum", pc_prod_checksum == pc_cons_checksum);
    ok("pc_fifo", pc_fifo_ok);
}

/* ---- 3. Futex barrier (raw SYS_futex) -------------------------------------- */
#define FB_THREADS 6
#define FB_ROUNDS 50

static int fb_futex;              /* generation counter, futex word */
static atomic_int fb_arrived;
static atomic_int fb_phase_err;
static atomic_int fb_in_phase2;   /* threads that passed the barrier this round */

static int futex_wait(int *addr, int expected) {
    return (int)syscall(SYS_futex, addr, FUTEX_WAIT, expected, NULL, NULL, 0);
}
static int futex_wake(int *addr, int n) {
    return (int)syscall(SYS_futex, addr, FUTEX_WAKE, n, NULL, NULL, 0);
}

static void *fb_worker(void *unused) {
    (void)unused;
    for (int r = 0; r < FB_ROUNDS; r++) {
        int gen = atomic_load(&fb_futex);
        int n = atomic_fetch_add(&fb_arrived, 1) + 1;
        if (n == FB_THREADS) {
            /* last arriver: no thread may be in phase2 before the barrier opens */
            if (atomic_load(&fb_in_phase2) != 0) atomic_store(&fb_phase_err, 1);
            atomic_store(&fb_arrived, 0);
            atomic_fetch_add(&fb_futex, 1);
            futex_wake(&fb_futex, FB_THREADS);
        } else {
            while (atomic_load(&fb_futex) == gen)
                futex_wait(&fb_futex, gen);
        }
        atomic_fetch_add(&fb_in_phase2, 1);
        /* drain: let all cross before resetting the phase2 gauge for next round */
        while (atomic_load(&fb_in_phase2) < FB_THREADS && atomic_load(&fb_in_phase2) != 0)
            sched_yield();
        if (n == FB_THREADS) atomic_store(&fb_in_phase2, 0);
        while (atomic_load(&fb_in_phase2) != 0 && n != FB_THREADS)
            sched_yield();
    }
    return NULL;
}

static void test_futex_barrier(void) {
    fb_futex = 0;
    atomic_store(&fb_arrived, 0);
    atomic_store(&fb_phase_err, 0);
    atomic_store(&fb_in_phase2, 0);
    pthread_t th[FB_THREADS];
    for (int i = 0; i < FB_THREADS; i++)
        pthread_create(&th[i], NULL, fb_worker, NULL);
    for (int i = 0; i < FB_THREADS; i++) pthread_join(th[i], NULL);
    ok("futex_barrier", atomic_load(&fb_phase_err) == 0);
}

/* ---- 4. Atomic counter contention ----------------------------------------- */
#define AT_THREADS 16
#define AT_ITERS 50000u

static atomic_ullong at_relaxed;
static atomic_ullong at_seqcst;

static void *at_worker(void *unused) {
    (void)unused;
    for (uint32_t i = 0; i < AT_ITERS; i++) {
        atomic_fetch_add_explicit(&at_relaxed, 1, memory_order_relaxed);
        atomic_fetch_add_explicit(&at_seqcst, 1, memory_order_seq_cst);
    }
    return NULL;
}

static void test_atomic_contention(void) {
    atomic_store(&at_relaxed, 0);
    atomic_store(&at_seqcst, 0);
    pthread_t th[AT_THREADS];
    for (int i = 0; i < AT_THREADS; i++)
        pthread_create(&th[i], NULL, at_worker, NULL);
    for (int i = 0; i < AT_THREADS; i++) pthread_join(th[i], NULL);
    unsigned long long ref = (unsigned long long)AT_THREADS * AT_ITERS;
    ok("atomic_relaxed", atomic_load(&at_relaxed) == ref);
    ok("atomic_seqcst", atomic_load(&at_seqcst) == ref);
}

/* ---- 5. Work-stealing task pool: fan-out 1000 tasks, each runs once -------- */
#define WS_WORKERS 8
#define WS_TASKS 1000

static struct {
    int next;
    pthread_mutex_t mtx;
    atomic_int done[WS_TASKS];
} ws;

static void *ws_worker(void *unused) {
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&ws.mtx);
        int idx = ws.next < WS_TASKS ? ws.next++ : -1;
        pthread_mutex_unlock(&ws.mtx);
        if (idx < 0) return NULL;
        atomic_fetch_add(&ws.done[idx], 1);
    }
}

static void test_work_pool(void) {
    ws.next = 0;
    pthread_mutex_init(&ws.mtx, NULL);
    for (int i = 0; i < WS_TASKS; i++) atomic_store(&ws.done[i], 0);
    pthread_t th[WS_WORKERS];
    for (int i = 0; i < WS_WORKERS; i++)
        pthread_create(&th[i], NULL, ws_worker, NULL);
    for (int i = 0; i < WS_WORKERS; i++) pthread_join(th[i], NULL);
    int all_once = 1;
    for (int i = 0; i < WS_TASKS; i++)
        if (atomic_load(&ws.done[i]) != 1) { all_once = 0; break; }
    ok("work_pool_each_once", all_once);
}

/* ---- 6. RW-lock: readers never see a torn (a+b) invariant ----------------- */
#define RW_READERS 6
#define RW_READS 100000u
#define RW_K 1000000

static struct {
    pthread_rwlock_t lock;
    long a, b;               /* invariant: a + b == RW_K under the lock */
    int stop;
} rw;

static atomic_int rw_torn;

static void *rw_reader(void *unused) {
    (void)unused;
    for (uint32_t i = 0; i < RW_READS; i++) {
        pthread_rwlock_rdlock(&rw.lock);
        if (rw.a + rw.b != RW_K) atomic_store(&rw_torn, 1);
        pthread_rwlock_unlock(&rw.lock);
    }
    return NULL;
}

static void *rw_writer(void *unused) {
    (void)unused;
    while (!__atomic_load_n(&rw.stop, __ATOMIC_ACQUIRE)) {
        pthread_rwlock_wrlock(&rw.lock);
        long d = (rw.a % 7) + 1;
        rw.a += d;
        rw.b -= d;               /* keeps a + b == RW_K between rd-lockable points */
        pthread_rwlock_unlock(&rw.lock);
        sched_yield();
    }
    return NULL;
}

static void test_rwlock(void) {
    pthread_rwlock_init(&rw.lock, NULL);
    rw.a = 0; rw.b = RW_K; rw.stop = 0;
    atomic_store(&rw_torn, 0);
    pthread_t rd[RW_READERS], wr;
    pthread_create(&wr, NULL, rw_writer, NULL);
    for (int i = 0; i < RW_READERS; i++)
        pthread_create(&rd[i], NULL, rw_reader, NULL);
    for (int i = 0; i < RW_READERS; i++) pthread_join(rd[i], NULL);
    __atomic_store_n(&rw.stop, 1, __ATOMIC_RELEASE);
    pthread_join(wr, NULL);
    ok("rwlock_no_torn_read", atomic_load(&rw_torn) == 0);
}

/* ---- 7. Counting semaphore: at most N in the critical section -------------- */
#define SEM_THREADS 12
#define SEM_PERMITS 3
#define SEM_ITERS 2000

static sem_t sem;
static atomic_int sem_inside;
static atomic_int sem_peak;

static void *sem_worker(void *unused) {
    (void)unused;
    for (int i = 0; i < SEM_ITERS; i++) {
        sem_wait(&sem);
        int cur = atomic_fetch_add(&sem_inside, 1) + 1;
        int peak = atomic_load(&sem_peak);
        while (cur > peak && !atomic_compare_exchange_weak(&sem_peak, &peak, cur))
            ;
        sched_yield();                 /* widen the interleave window on one core */
        atomic_fetch_sub(&sem_inside, 1);
        sem_post(&sem);
    }
    return NULL;
}

static void test_semaphore(void) {
    sem_init(&sem, 0, SEM_PERMITS);
    atomic_store(&sem_inside, 0);
    atomic_store(&sem_peak, 0);
    pthread_t th[SEM_THREADS];
    for (int i = 0; i < SEM_THREADS; i++)
        pthread_create(&th[i], NULL, sem_worker, NULL);
    for (int i = 0; i < SEM_THREADS; i++) pthread_join(th[i], NULL);
    int final_val = -1;
    sem_getvalue(&sem, &final_val);
    sem_destroy(&sem);
    ok("sem_peak_le_permits", atomic_load(&sem_peak) <= SEM_PERMITS && atomic_load(&sem_peak) >= 1);
    ok("sem_value_restored", final_val == SEM_PERMITS);
}

/* ---- 8. Thread-local isolation (__thread + pthread_key) -------------------- */
#define TL_THREADS 10

static __thread uint64_t tl_slot;
static pthread_key_t tl_key;
static atomic_int tl_bad;

static void *tl_worker(void *p) {
    uint64_t id = (uint64_t)(uintptr_t)p;
    tl_slot = id * id;
    pthread_setspecific(tl_key, (void *)(uintptr_t)(id + 100));
    for (int i = 0; i < 1000; i++) {
        sched_yield();
        if (tl_slot != id * id) atomic_store(&tl_bad, 1);
        if ((uintptr_t)pthread_getspecific(tl_key) != id + 100) atomic_store(&tl_bad, 1);
    }
    return NULL;
}

static void test_thread_local(void) {
    pthread_key_create(&tl_key, NULL);
    atomic_store(&tl_bad, 0);
    pthread_t th[TL_THREADS];
    for (int i = 0; i < TL_THREADS; i++)
        pthread_create(&th[i], NULL, tl_worker, (void *)(uintptr_t)i);
    for (int i = 0; i < TL_THREADS; i++) pthread_join(th[i], NULL);
    pthread_key_delete(tl_key);
    ok("thread_local_isolation", atomic_load(&tl_bad) == 0);
}

/* ---- 9. RR fairness: every runnable thread advances (no starvation) -------- */
#define RR_THREADS 8

static atomic_int rr_go;
static atomic_ullong rr_slot[RR_THREADS];

static void *rr_worker(void *p) {
    int id = (int)(intptr_t)p;
    while (!atomic_load(&rr_go)) sched_yield();
    struct timespec t0, now;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    for (;;) {
        atomic_fetch_add(&rr_slot[id], 1);
        sched_yield();
        clock_gettime(CLOCK_MONOTONIC, &now);
        double dt = (now.tv_sec - t0.tv_sec) + (now.tv_nsec - t0.tv_nsec) / 1e9;
        if (dt > 0.5) break;         /* fixed wall budget */
    }
    return NULL;
}

static void test_rr_fairness(void) {
    atomic_store(&rr_go, 0);
    for (int i = 0; i < RR_THREADS; i++) atomic_store(&rr_slot[i], 0);
    pthread_t th[RR_THREADS];
    for (int i = 0; i < RR_THREADS; i++)
        pthread_create(&th[i], NULL, rr_worker, (void *)(intptr_t)i);
    atomic_store(&rr_go, 1);
    for (int i = 0; i < RR_THREADS; i++) pthread_join(th[i], NULL);
    unsigned long long mn = ~0ull;
    for (int i = 0; i < RR_THREADS; i++) {
        unsigned long long v = atomic_load(&rr_slot[i]);
        if (v < mn) mn = v;
    }
    ok("rr_fairness_no_starvation", mn > 0);   /* every thread made progress */
}

int main(void) {
    printf("CPU_CONCURRENCY_START threads-on-1-vcpu (cooperative concurrency)\n");
    long nproc = sysconf(_SC_NPROCESSORS_ONLN);
    printf("cpu-concurrency: online CPUs = %ld (single-core cooperative model)\n", nproc);
    fflush(stdout);

    test_parallel_reduction();
    test_producer_consumer();
    test_futex_barrier();
    test_atomic_contention();
    test_work_pool();
    test_rwlock();
    test_semaphore();
    test_thread_local();
    test_rr_fairness();

    int total = g_pass + g_fail;
    printf("cpu-concurrency: %d/%d assertions passed\n", g_pass, total);
    if (g_fail == 0 && g_pass == 14) {
        printf("ALL PASS %d/%d\n", g_pass, total);
        printf("CPU_CONCURRENCY_PASSED\n");
        return 0;
    }
    printf("CPU_CONCURRENCY_FAILED\n");
    return 1;
}
