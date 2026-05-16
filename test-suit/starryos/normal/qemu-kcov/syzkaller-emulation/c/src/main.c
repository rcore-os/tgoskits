/* Emulate syzkaller's exact kcov usage pattern.
 *
 * Syzkaller's executor (executor_linux.h) uses kcov in the following way:
 *   1. cover_open()  — open /dev/kcov, INIT_TRACE with kCoverSize (512K),
 *                       mmap the buffer (with guard pages in newer versions)
 *   2. cover_enable() — KCOV_ENABLE with KCOV_TRACE_PC
 *   3. For each test program:
 *        cover_reset()    — write 0 to buf[0] (NOT a DISABLE/ENABLE cycle)
 *        execute syscalls
 *        cover_collect()  — read buf[0] to get count, check overflow
 *   4. cover_disable() — KCOV_DISABLE
 */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_RESET_TRACE _IO('c', 104)
#define KCOV_TRACE_PC 0
#define KCOV_TRACE_CMP 1

/* Syzkaller's default coverage size (entries, not bytes). */
#define SYZ_KCOVER_SIZE (512 * 1024)
/* Our maximum — used when the test needs a guaranteed-accepted size. */
#define KCOV_MAX_ENTRIES (1024 * 1024)

/* Architecture kernel-space PC floor (matches pc-values test). */
#if defined(__x86_64__) || defined(__amd64__)
#define KERNEL_PC_MIN 0xffff800000000000ULL
#elif defined(__aarch64__)
#define KERNEL_PC_MIN 0xffff000000000000ULL
#elif defined(__riscv) && __riscv_xlen == 64
#define KERNEL_PC_MIN 0xffffffc000000000ULL
#else
#define KERNEL_PC_MIN 0x8000000000000000ULL
#endif

/* ---- helpers ---- */

static void burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        getpid();
        getuid();
        getppid();
    }
}

static void heavy_burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        /* Mix of syscalls to exercise diverse kernel paths. */
        getpid();
        getuid();
        getppid();
        struct stat st;
        stat("/", &st);
        stat("/dev", &st);
        int fd = open("/dev/null", O_RDONLY);
        if (fd >= 0)
            close(fd);
    }
}

/* ---- syzkaller-like API ---- */

typedef struct {
    int fd;
    uint64_t *data;
    uint64_t *data_end;  /* one past the last valid u64 */
    uint64_t size;       /* number of u64 entries (the cover_size arg) */
    uint32_t overflow;
    uint32_t collected;
} cover_t;

static void cover_open(cover_t *cov, uint64_t n_entries) {
    memset(cov, 0, sizeof(*cov));
    cov->fd = open("/dev/kcov", O_RDWR);
    if (cov->fd < 0) {
        printf("FATAL: cover_open: open failed (errno=%d)\n", errno);
        exit(1);
    }

    if (ioctl(cov->fd, KCOV_INIT_TRACE, n_entries) != 0) {
        printf("FATAL: cover_open: INIT_TRACE(%lu) failed (errno=%d)\n",
               n_entries, errno);
        exit(1);
    }

    size_t mmap_size = n_entries * sizeof(uint64_t);
    cov->data = (uint64_t *)mmap(NULL, mmap_size,
                                 PROT_READ | PROT_WRITE, MAP_SHARED,
                                 cov->fd, 0);
    if (cov->data == MAP_FAILED) {
        printf("FATAL: cover_open: mmap failed (errno=%d)\n", errno);
        exit(1);
    }

    cov->data_end = cov->data + n_entries;
    cov->size = n_entries;
}

static void cover_enable(cover_t *cov) {
    if (ioctl(cov->fd, KCOV_ENABLE, KCOV_TRACE_PC) != 0) {
        printf("FATAL: cover_enable failed (errno=%d)\n", errno);
        exit(1);
    }
}

static void cover_disable(cover_t *cov) {
    ioctl(cov->fd, KCOV_DISABLE, 0);
}

static void cover_close(cover_t *cov) {
    if (cov->fd >= 0) {
        munmap(cov->data, cov->size * sizeof(uint64_t));
        close(cov->fd);
        cov->fd = -1;
    }
}

/* Syzkaller's exact "reset" — just zero the count word, NOT a DISABLE/ENABLE. */
static void cover_reset(cover_t *cov) {
    cov->data[0] = 0;
}

/* Syzkaller's cover_collect: read count, check overflow.
 * Syzkaller's overflow check: (data + (count + 2) * sizeof(cover_data_t)) > data_end
 * The +2 accounts for the header and ensures at least one PC slot exists.
 */
static void cover_collect(cover_t *cov) {
    cov->collected = (uint32_t)cov->data[0];
    cov->overflow =
        (cov->data + (cov->collected + 2)) > cov->data_end ? 1 : 0;
}

/* ---- Test scenarios ---- */

/* §1: Syzkaller's default buffer size (512K entries = 4MB).
 *      Must be accepted after our KCOV_MAX_ENTRIES bump. */
static void syz_default_buffer_size(void) {
    cover_t cov;
    cover_open(&cov, SYZ_KCOVER_SIZE);
    printf("  INFO: syzkaller 512K-entry buffer open OK\n");
    CHECK(cov.data != NULL, "512K buffer mmap OK");
    cover_close(&cov);
}

/* §2: cover_reset pattern — zero count word without DISABLE.
 *      This is how syzkaller resets coverage between test programs. */
static void syz_reset_without_disable(void) {
    cover_t cov;
    cover_open(&cov, 4096);
    cover_enable(&cov);

    /* Generate some coverage. */
    burst(100);
    cover_collect(&cov);
    CHECK(cov.collected >= 1, "pre-reset: coverage collected");
    uint64_t saved = cov.collected;

    /* Syzkaller reset: just zero the count word. */
    cover_reset(&cov);
    CHECK(cov.data[0] == 0, "reset: count word is 0");

    /* Execute more syscalls — coverage should accumulate again. */
    burst(50);
    cover_collect(&cov);
    CHECK(cov.collected >= 1, "post-reset: new coverage collected");
    printf("  INFO: reset cycle: %lu → 0 → %u\n", saved, cov.collected);

    cover_disable(&cov);
    cover_close(&cov);
}

/* §3: Multiple collect/reset cycles in a single ENABLE session.
 *      Syzkaller runs many test programs per enable, resetting between each. */
static void syz_multi_reset_cycles(void) {
    cover_t cov;
    cover_open(&cov, 4096);
    cover_enable(&cov);

    int cycles = 20;
    for (int i = 0; i < cycles; i++) {
        cover_reset(&cov);
        burst(30);
        cover_collect(&cov);
        if (cov.collected < 1) {
            printf("  WARN: cycle %d collected %u entries\n", i, cov.collected);
        }
    }
    CHECK(cov.collected >= 1, "last cycle collected coverage");
    printf("  INFO: %d reset/collect cycles in one ENABLE session\n", cycles);

    cover_disable(&cov);
    cover_close(&cov);
}

/* §4: Overflow detection using syzkaller's exact formula.
 *      Syzkaller checks: (data + (count + 2) * 8) > data_end */
static void syz_overflow_detection(void) {
    /* Use a tiny buffer so we can trigger overflow. */
    cover_t cov;
    cover_open(&cov, 64);
    cover_enable(&cov);

    /* Heavy burst should fill the buffer. */
    heavy_burst(10000);
    cover_collect(&cov);

    CHECK(cov.collected > 0, "overflow: count > 0");
    CHECK(cov.collected <= 64, "overflow: count ≤ capacity");
    printf("  INFO: 64-entry buffer, heavy burst → %u entries, overflow=%u\n",
           cov.collected, cov.overflow);

    cover_disable(&cov);
    cover_close(&cov);
}

/* §5: Syzkaller-style PC validation.
 *      All PCs must be valid kernel-space addresses. */
static void syz_pc_validation(void) {
    cover_t cov;
    cover_open(&cov, 4096);
    cover_enable(&cov);
    heavy_burst(500);
    cover_disable(&cov);

    cover_collect(&cov);
    uint32_t n = cov.collected;
    printf("  INFO: collected %u PCs for validation\n", n);

    int all_valid = 1;
    int all_nonzero = 1;
    int diverse = 0;
    uint64_t first_pc = 0;
    for (uint32_t i = 1; i <= n && i <= 50; i++) {
        uint64_t pc = cov.data[i];
        if (pc == 0)
            all_nonzero = 0;
        if (pc < KERNEL_PC_MIN)
            all_valid = 0;
        if (i == 1)
            first_pc = pc;
        else if (pc != first_pc)
            diverse = 1;
    }

    CHECK(all_valid, "PC validation: all PCs in kernel address range");
    CHECK(all_nonzero, "PC validation: no zero PCs");
    CHECK(diverse, "PC validation: multiple distinct PCs observed");

    cover_close(&cov);
}

/* §6: Re-enable after disable (syzkaller reuses fds).
 *      Syzkaller may disable and re-enable on the same fd for a new
 *      batch of test programs. */
static void syz_reuse_fd(void) {
    cover_t cov;
    cover_open(&cov, 4096);

    for (int cycle = 0; cycle < 5; cycle++) {
        cover_enable(&cov);
        burst(50);
        cover_disable(&cov);

        /* Syzkaller reads coverage between disable and re-enable. */
        cover_collect(&cov);
        if (cycle == 0)
            CHECK(cov.collected >= 1, "reuse: coverage in cycle 0");
    }
    printf("  INFO: 5 ENABLE/DISABLE cycles on same fd OK\n");

    cover_close(&cov);
}

/* §7: Syzkaller-style fork worker.
 *      Syzkaller forks worker processes, each needing independent kcov. */
static void syz_worker_fork(void) {
    cover_t parent_cov;
    cover_open(&parent_cov, 1024);
    cover_enable(&parent_cov);
    burst(20);
    cover_disable(&parent_cov);

    /* Capture parent coverage baseline before fork. */
    uint64_t parent_before = parent_cov.data[0];

    pid_t pid = fork();
    if (pid == 0) {
        /* Child: must create its own kcov (fork resets per-thread state).
         * Linux: "Coverage collection is disabled in child after fork()."
         * The fd is inherited but thread state is clean. */
        cover_t child_cov;
        cover_open(&child_cov, 1024);
        cover_enable(&child_cov);
        burst(30);
        cover_disable(&child_cov);

        cover_collect(&child_cov);
        if (child_cov.collected < 1)
            _exit(10);

        cover_close(&child_cov);
        _exit(0);
    }

    CHECK(pid > 0, "fork worker");
    int wstatus;
    CHECK_RET(waitpid(pid, &wstatus, 0), pid, "waitpid fork worker");
    CHECK(WIFEXITED(wstatus), "fork worker exited normally");
    CHECK_RET(WEXITSTATUS(wstatus), 0, "fork worker kcov OK");

    /* Parent's coverage should be unaffected by fork. */
    CHECK(parent_cov.data[0] == parent_before,
          "fork: parent coverage unchanged");

    cover_close(&parent_cov);
}

/* §8: KCOV_RESET_TRACE ioctl (alternative reset path for read-only coverage).
 *      Syzkaller uses this when `flag_read_only_coverage` is set, instead of
 *      writing 0 to buf[0] directly.  Our implementation supports both paths. */
static void syz_reset_trace_ioctl(void) {
    cover_t cov;
    cover_open(&cov, 4096);
    cover_enable(&cov);

    /* Generate initial coverage. */
    burst(100);
    cover_collect(&cov);
    CHECK(cov.collected >= 1, "reset-trace: pre-reset coverage > 0");
    uint64_t before = cov.collected;

    /* KCOV_RESET_TRACE — the kernel zeroes buf[0] then immediately the ioctl
     * return path (which is itself instrumented) records a few new PCs, so
     * buf[0] will be non-zero but very small.  Linux semantics: tracing
     * continues after reset (no DISABLE). */
    if (ioctl(cov.fd, KCOV_RESET_TRACE, 0) != 0) {
        printf("FATAL: KCOV_RESET_TRACE failed (errno=%d)\n", errno);
        exit(1);
    }
    CHECK(cov.data[0] < 100, "reset-trace: count near 0 after RESET_TRACE (ioctl return path may add a few)");

    /* Generate more coverage — should accumulate again. */
    burst(50);
    cover_collect(&cov);
    CHECK(cov.collected > 100, "reset-trace: post-reset new coverage > 100");
    printf("  INFO: RESET_TRACE cycle: %lu → 0 → %u\n", before, cov.collected);

    cover_disable(&cov);
    cover_close(&cov);
}

/* §9: Multiple syzkaller-like workers (threads).
 *      Each worker has its own kcov fd, independent coverage. */
typedef struct {
    int id;
    uint32_t count;
    int ok;
} worker_t;

static void *worker_thread(void *arg) {
    worker_t *w = (worker_t *)arg;
    w->ok = 0;

    cover_t cov;
    cover_open(&cov, 2048);
    cover_enable(&cov);
    heavy_burst(300);
    cover_disable(&cov);

    cover_collect(&cov);
    w->count = cov.collected;
    cover_close(&cov);
    w->ok = 1;
    return NULL;
}

static void syz_worker_threads(void) {
#define NWORKERS 8
    pthread_t t[NWORKERS];
    worker_t r[NWORKERS];

    for (int i = 0; i < NWORKERS; i++) {
        r[i].id = i;
        pthread_create(&t[i], NULL, worker_thread, &r[i]);
    }
    int all_ok = 1;
    uint64_t total = 0;
    for (int i = 0; i < NWORKERS; i++) {
        pthread_join(t[i], NULL);
        if (!r[i].ok)
            all_ok = 0;
        total += r[i].count;
        printf("  INFO: worker %d → %u PCs\n", r[i].id, r[i].count);
    }
    CHECK(all_ok, "all 8 workers completed OK");
    CHECK(total > 0, "total coverage across workers > 0");
#undef NWORKERS
}

/* ---- main ---- */

int main(void) {
    TEST_START("Syzkaller kcov emulation");

    syz_default_buffer_size();
    syz_reset_without_disable();
    syz_multi_reset_cycles();
    syz_overflow_detection();
    syz_pc_validation();
    syz_reuse_fd();
    syz_worker_fork();
    syz_reset_trace_ioctl();
    syz_worker_threads();

    TEST_DONE();
}
