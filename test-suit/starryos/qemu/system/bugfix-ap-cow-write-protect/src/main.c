// Correctness guard for kernel write protection on secondary CPUs.
//
// A forked COW page is present but read-only in every address space. When the
// kernel writes into it through read() user-copy, the CPU must fault so the
// child receives a private page. On x86_64 this requires CR0.WP on every CPU.
// Linux loads one complete CR0_STATE on both the BSP and APs; a boot path that
// misses WP silently writes through the shared frame and corrupts the parent.
//
// The COW probe runs once on every CPU in the process affinity mask and checks
// the CPU after pinning, making a per-AP boot-state regression deterministic.
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

// This is a software user-memory permission check, not proof of CR0.WP: the
// access validator may reject the destination before attempting a store.
static void read_only_destination_is_rejected(void)
{
    int pfd[2];
    if (pipe(pfd) != 0) {
        note_fail("read-only destination setup", strerror(errno));
        return;
    }

    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        note_fail("read-only destination mmap", strerror(errno));
        close(pfd[0]);
        close(pfd[1]);
        return;
    }
    page[0] = 'R';

    if (mprotect(page, 4096, PROT_READ) != 0 || write(pfd[1], "w", 1) != 1) {
        note_fail("read-only destination setup", strerror(errno));
        munmap(page, 4096);
        close(pfd[0]);
        close(pfd[1]);
        return;
    }

    errno = 0;
    ssize_t n = read(pfd[0], page, 1);
    if (n == -1 && errno == EFAULT && page[0] == 'R') {
        note_pass("read() rejects an mprotect(PROT_READ) destination");
    } else {
        char detail[96];
        snprintf(detail, sizeof(detail), "n=%zd errno=%d page[0]=%d", n,
                 errno, page[0]);
        note_fail("read-only destination", detail);
    }

    munmap(page, 4096);
    close(pfd[0]);
    close(pfd[1]);
}

static void cow_write_isolated_on_cpu(int target_cpu)
{
    int pfd[2];
    if (pipe(pfd) != 0) {
        note_fail("COW pipe", strerror(errno));
        return;
    }

    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        note_fail("COW mmap", strerror(errno));
        close(pfd[0]);
        close(pfd[1]);
        return;
    }
    page[0] = 'P'; // Fault in the page before fork marks both PTEs read-only.

    static const char payload[] = "AP-COW-OK";
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("COW fork", strerror(errno));
        munmap(page, 4096);
        close(pfd[0]);
        close(pfd[1]);
        return;
    }
    if (pid == 0) {
        cpu_set_t target;
        CPU_ZERO(&target);
        CPU_SET(target_cpu, &target);
        if (sched_setaffinity(0, sizeof(target), &target) != 0) {
            _exit(4);
        }
        if (sched_getcpu() != target_cpu) {
            _exit(5);
        }

        close(pfd[1]);
        size_t got = 0;
        while (got < sizeof(payload)) {
            ssize_t n = read(pfd[0], page + got, sizeof(payload) - got);
            if (n <= 0) {
                _exit(2);
            }
            got += (size_t)n;
        }
        _exit(memcmp(page, payload, sizeof(payload)) == 0 ? 0 : 3);
    }

    close(pfd[0]);
    if (write(pfd[1], payload, sizeof(payload)) != (ssize_t)sizeof(payload)) {
        note_fail("COW parent pipe write", strerror(errno));
    }
    close(pfd[1]);

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        note_fail("COW waitpid", strerror(errno));
        munmap(page, 4096);
        return;
    }

    char detail[128];
    if (!WIFEXITED(status)) {
        snprintf(detail, sizeof(detail), "CPU %d child status=%d", target_cpu,
                 status);
        note_fail("COW child exit", detail);
    } else if (WEXITSTATUS(status) == 4) {
        snprintf(detail, sizeof(detail), "sched_setaffinity CPU %d failed",
                 target_cpu);
        note_fail("COW CPU pinning", detail);
    } else if (WEXITSTATUS(status) == 5) {
        snprintf(detail, sizeof(detail), "child did not run on CPU %d",
                 target_cpu);
        note_fail("COW CPU pinning", detail);
    } else if (WEXITSTATUS(status) != 0) {
        snprintf(detail, sizeof(detail), "CPU %d child exit=%d", target_cpu,
                 WEXITSTATUS(status));
        note_fail("COW user-copy payload", detail);
    } else if (page[0] != 'P') {
        snprintf(detail, sizeof(detail),
                 "CPU %d corrupted parent page: page[0]=%d", target_cpu,
                 page[0]);
        note_fail("COW isolation", detail);
    } else {
        snprintf(detail, sizeof(detail),
                 "CPU %d read() write faulted into a private COW page",
                 target_cpu);
        note_pass(detail);
    }

    munmap(page, 4096);
}

static void cow_write_isolated_on_every_cpu(void)
{
    cpu_set_t allowed;
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        note_fail("COW CPU enumeration", strerror(errno));
        return;
    }

    int cpu_count = 0;
    int secondary_count = 0;
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        if (!CPU_ISSET(cpu, &allowed)) {
            continue;
        }
        cpu_count++;
        if (cpu > 0) {
            secondary_count++;
        }
        cow_write_isolated_on_cpu(cpu);
    }

    if (cpu_count == 0) {
        note_fail("COW CPU enumeration", "affinity mask has no CPUs");
    }
    if (secondary_count == 0) {
        note_fail("AP COW coverage", "affinity mask has no secondary CPU");
    }
}

int main(void)
{
    printf("=== ap-cow-write-protect ===\n");

    read_only_destination_is_rejected();
    cow_write_isolated_on_every_cpu();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
