// Correctness guard for kernel write protection on secondary CPUs.
//
// A forked COW page is present but read-only in every address space. When a
// secondary CPU's kernel writes into it through the syscall user-copy path
// (read() from a pipe), that write must take the COW fault and stay private
// to the child. On x86_64 this depends on CR0.WP being active on every CPU:
// Linux loads CR0_STATE (which includes X86_CR0_WP) on both the BSP
// (head_64.S) and the AP real-mode trampoline (trampoline_64.S), so a ring-0
// write can never bypass a read-only PTE. A boot path that misses WP on one
// CPU turns every kernel user-copy on that CPU into a write through the
// shared frame, corrupting the parent silently.
//
// The child is pinned to the lowest allowed secondary CPU and reports the CPU
// it actually ran on, making the otherwise scheduling-dependent corruption
// deterministic.
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

// Returns the lowest allowed CPU with an index above the boot CPU, or -1 when
// the current affinity mask has no secondary CPU.
static int lowest_allowed_secondary_cpu(void)
{
    cpu_set_t allowed;
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        return -1;
    }
    for (int cpu = 1; cpu < CPU_SETSIZE; cpu++) {
        if (CPU_ISSET(cpu, &allowed)) {
            return cpu;
        }
    }
    return -1;
}

// Probes CR0.WP on every allowed CPU from user mode. A kernel write into a
// clean read-only mapping must fault (EFAULT): with CR0.WP set the CPU
// honors the read-only PTE even for ring 0; with WP clear the same write
// silently lands through the PTE. Returns the number of CPUs found without
// write protection.
static int probe_write_protect_per_cpu(void)
{
    int unprotected = 0;
    cpu_set_t allowed;
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        note_fail("write-protect probe", "sched_getaffinity failed");
        return -1;
    }

    int pfd[2];
    if (pipe(pfd) != 0) {
        note_fail("write-protect probe", strerror(errno));
        return -1;
    }

    char marker[96];
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        if (!CPU_ISSET(cpu, &allowed)) {
            continue;
        }
        // One byte per probe: an EFAULT result may or may not consume the
        // byte depending on where the fault aborts the copy.
        if (write(pfd[1], "w", 1) != 1) {
            snprintf(marker, sizeof(marker), "cpu %d: pipe write: %s", cpu,
                     strerror(errno));
            note_fail("write-protect probe", marker);
            continue;
        }
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(cpu, &set);
        if (sched_setaffinity(0, sizeof(set), &set) != 0) {
            snprintf(marker, sizeof(marker), "cpu %d: pin failed: %s", cpu,
                     strerror(errno));
            note_fail("write-protect probe", marker);
            continue;
        }

        char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (page == MAP_FAILED) {
            snprintf(marker, sizeof(marker), "cpu %d: mmap: %s", cpu,
                     strerror(errno));
            note_fail("write-protect probe", marker);
            continue;
        }
        page[0] = 'K';
        if (mprotect(page, 4096, PROT_READ) != 0) {
            snprintf(marker, sizeof(marker), "cpu %d: mprotect: %s", cpu,
                     strerror(errno));
            note_fail("write-protect probe", marker);
            munmap(page, 4096);
            continue;
        }

        errno = 0;
        ssize_t n = read(pfd[0], page, 1);
        int cpu_now = sched_getcpu();
        if (n == -1 && errno == EFAULT) {
            snprintf(marker, sizeof(marker), "cpu %d honored read-only PTE (WP=1)",
                     cpu_now);
            note_pass(marker);
        } else {
            snprintf(marker, sizeof(marker),
                     "cpu %d kernel write went through read-only PTE (n=%zd errno=%d page[0]='%c')",
                     cpu_now, n, errno, page[0]);
            note_fail("write-protect probe", marker);
            unprotected++;
        }
        munmap(page, 4096);
    }

    close(pfd[0]);
    close(pfd[1]);
    return unprotected;
}

static void ap_cow_write_isolation(void)
{
    int target_cpu = lowest_allowed_secondary_cpu();
    if (target_cpu < 0) {
        // Regression coverage needs a secondary CPU; single-CPU environments
        // cannot exercise the per-CPU write-protect invariant.
        printf("SKIP: ap-cow-write-protect needs a secondary CPU\n");
        return;
    }

    int pfd[2];
    if (pipe(pfd) != 0) {
        note_fail("pipe", strerror(errno));
        return;
    }

    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        note_fail("mmap cow page", strerror(errno));
        close(pfd[0]);
        close(pfd[1]);
        return;
    }
    page[0] = 'P'; // fault in; fork() below marks this page COW read-only

    static const char payload[] = "AP-COW-OK";
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("fork", strerror(errno));
        munmap(page, 4096);
        close(pfd[0]);
        close(pfd[1]);
        return;
    }
    if (pid == 0) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(target_cpu, &set);
        if (sched_setaffinity(0, sizeof(set), &set) != 0) {
            printf("CHILD-AFFINITY-FAILED: %s\n", strerror(errno));
            _exit(4);
        }
        int cpu = sched_getcpu();
        if (cpu != target_cpu) {
            printf("CHILD-RAN-ON-CPU: %d\n", cpu);
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

    // Parent: feed the payload, wait, then verify the child's kernel-mode
    // write stayed private to the child's COW copy.
    close(pfd[0]);
    if (write(pfd[1], payload, sizeof(payload)) != (ssize_t)sizeof(payload)) {
        note_fail("parent write to pipe", strerror(errno));
    }
    close(pfd[1]);

    int st = 0;
    waitpid(pid, &st, 0);

    char detail[128];
    if (!WIFEXITED(st)) {
        snprintf(detail, sizeof(detail), "child did not exit cleanly (st=%d)", st);
        note_fail("AP COW child exit", detail);
    } else if (WEXITSTATUS(st) == 5) {
        note_fail("AP COW child pinning", "child did not run on the pinned secondary CPU");
    } else if (WEXITSTATUS(st) != 0) {
        snprintf(detail, sizeof(detail), "child exit=%d", WEXITSTATUS(st));
        note_fail("AP COW child payload", detail);
    } else if (page[0] != 'P') {
        snprintf(detail, sizeof(detail),
                 "parent page corrupted (page[0]='%c'): kernel write on CPU %d "
                 "bypassed the read-only COW PTE",
                 page[0], target_cpu);
        note_fail("AP COW isolation", detail);
    } else {
        snprintf(detail, sizeof(detail), "child wrote on CPU %d via read()", target_cpu);
        note_pass(detail);
    }

    munmap(page, 4096);
}

int main(void)
{
    printf("=== ap-cow-write-protect ===\n");

    probe_write_protect_per_cpu();
    ap_cow_write_isolation();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
