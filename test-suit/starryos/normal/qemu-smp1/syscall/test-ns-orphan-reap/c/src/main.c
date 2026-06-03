/*
 * test-ns-orphan-reap — verify PID namespace orphan reaping behavior.
 *
 * Topology tested:
 *   parent (root ns)
 *     └── child (clone3 CLONE_NEWPID → PID 1 in new ns)
 *            └── grandchild (fork → PID 2 in new ns, sleep(10))
 *
 * Key: Linux unshare(CLONE_NEWPID) does NOT put the caller into a new
 * PID ns; only children get the new ns. So we use clone3(CLONE_NEWPID)
 * to create the ns-init child directly.
 *
 * Actions:
 *   1. Parent: clone3(CLONE_NEWPID) → child
 *   2. Child (PID 1 in new ns): fork grandchild (sleep 10)
 *   3. Child signals parent ready via pipe
 *   4. Parent: kill(child, SIGKILL) → waitpid(child)
 *   5. Parent: scan /proc for orphans (PPid == dead child)
 *   6. Parent: waitpid(-1, WNOHANG) loop — check for zombie grandchild
 *
 * Expected Linux: zap_pid_ns_processes() SIGKILLs grandchild → reapable zombie
 * StarryOS suspect: no zap → grandchild survives → gap confirmed
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <dirent.h>
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* ---- clone3 helpers ---- */
#ifndef __NR_clone3
#define __NR_clone3 435
#endif

struct clone3_args {
    unsigned long long flags;
    unsigned long long pidfd;
    unsigned long long child_tid;
    unsigned long long parent_tid;
    unsigned long long exit_signal;
    unsigned long long stack;
    unsigned long long stack_size;
    unsigned long long tls;
    unsigned long long set_tid;
    unsigned long long set_tid_size;
    unsigned long long cgroup;
};

static pid_t clone3_newpid_child(void)
{
    struct clone3_args args;
    memset(&args, 0, sizeof(args));
    args.flags = CLONE_NEWPID;
    args.exit_signal = SIGCHLD;
    return (pid_t)syscall(__NR_clone3, &args, sizeof(args));
}

/* ---- /proc scanner ---- */
static int find_proc_with_ppid(pid_t target_ppid, pid_t *found_pid, int max_results)
{
    DIR *d = opendir("/proc");
    if (!d) return 0;

    int count = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL && count < max_results) {
        if (ent->d_name[0] < '0' || ent->d_name[0] > '9') continue;
        pid_t pid = (pid_t)atoi(ent->d_name);
        if (pid <= 1) continue;

        char stat_path[64];
        snprintf(stat_path, sizeof(stat_path), "/proc/%d/stat", pid);
        FILE *f = fopen(stat_path, "r");
        if (!f) continue;

        char comm[256];
        char state;
        pid_t ppid;
        int n = fscanf(f, "%d %255s %c %d", &pid, comm, &state, &ppid);
        fclose(f);
        if (n >= 4 && ppid == target_ppid) {
            found_pid[count++] = pid;
            printf("  INFO | orphan pid=%d comm=%.250s state=%c ppid=%d\n",
                   pid, comm, state, ppid);
        }
    }
    closedir(d);
    return count;
}

/* ---- waitpid timeout poll ---- */
static pid_t waitpid_any_timeout(int *status, int timeout_sec)
{
    time_t start = time(NULL);
    while (time(NULL) - start < timeout_sec) {
        pid_t got = waitpid(-1, status, WNOHANG);
        if (got > 0) return got;
        if (got < 0 && errno == ECHILD) return -1;
        struct timespec ts = {0, 100 * 1000 * 1000};
        nanosleep(&ts, NULL);
    }
    errno = ETIMEDOUT;
    return -1;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("PID namespace orphan reaping via clone3 CLONE_NEWPID");

    int ready_pipe[2];
    CHECK(pipe(ready_pipe) == 0, "create ready pipe");

    /* ---- Step 1: clone3(CLONE_NEWPID) → child becomes PID ns init ---- */
    pid_t child = clone3_newpid_child();
    CHECK(child >= 0, "clone3 CLONE_NEWPID creates PID ns init");
    printf("  INFO | parent: clone3 returned child global pid = %d\n", child);

    if (child == 0) {
        /* ========== CHILD (PID 1 in new ns) ========== */
        close(ready_pipe[0]);

        pid_t local_pid = getpid();
        printf("  INFO | child local pid = %d (expect 1)\n", local_pid);
        if (local_pid != 1) {
            printf("  FAIL | expected local pid 1, got %d\n", local_pid);
            write(ready_pipe[1], "\x01", 1);
            close(ready_pipe[1]);
            _exit(1);
        }

        /* Fork grandchild (PID 2 in new ns) */
        pid_t gc = fork();
        if (gc < 0) {
            write(ready_pipe[1], "\x02", 1);
            close(ready_pipe[1]);
            _exit(2);
        }

        if (gc == 0) {
            /* ====== GRANDCHILD (PID 2 in new ns) ====== */
            pid_t gc_local = getpid();
            printf("  INFO | grandchild local pid = %d (expect 2)\n", gc_local);
            sleep(10);
            _exit(0);
        }

        /* Signal parent: ready */
        write(ready_pipe[1], "\x00", 1);
        close(ready_pipe[1]);

        /* Wait — parent will kill us */
        for (;;) sleep(10);
    }

    /* ========== PARENT (root ns) ========== */
    close(ready_pipe[1]);

    char sig_byte;
    ssize_t n = read(ready_pipe[0], &sig_byte, 1);
    close(ready_pipe[0]);

    if (n != 1 || sig_byte != 0) {
        CHECK(0, "child setup failed");
        waitpid(child, NULL, 0);
        TEST_DONE();
        return 1;
    }

    printf("  INFO | child reported ready — new PID ns is active\n");

    /* Find grandchildren in /proc */
    {
        pid_t orphans[16];
        int n_orphans = find_proc_with_ppid(child, orphans, 16);
        printf("  INFO | found %d process(es) with PPid=%d (ns grandchildren)\n",
               n_orphans, child);
    }

    /* Kill child (PID ns init) */
    CHECK(kill(child, SIGKILL) == 0, "kill PID ns init with SIGKILL");

    int status = 0;
    pid_t reaped = waitpid(child, &status, 0);
    CHECK_RET(reaped, child, "waitpid reaps killed ns init");
    CHECK(WIFSIGNALED(status), "ns init terminated by signal");
    if (WIFSIGNALED(status))
        CHECK(WTERMSIG(status) == SIGKILL, "ns init got SIGKILL");

    printf("  INFO | PID namespace init killed and reaped\n");

    /* Wait for ns teardown */
    sleep(2);

    /* Check for zombie grandchild */
    {
        int zstatus = 0;
        pid_t zgot = waitpid(-1, &zstatus, WNOHANG);
        if (zgot > 0)
            printf("  INFO | zombie found: pid=%d status=%d\n", zgot, zstatus);
        else if (zgot == 0)
            printf("  INFO | no zombie — grandchild likely still alive\n");
        else
            printf("  INFO | waitpid(-1) → %d errno=%d\n", zgot, errno);
    }

    /* Scan /proc: orphans with dead ns init as parent */
    {
        pid_t orphans[16];
        int n = find_proc_with_ppid(child, orphans, 16);
        if (n > 0) {
            printf("  WARN | %d process(es) still have dead ns init as parent!\n", n);
            printf("  WARN | PID namespace orphan reaping gap CONFIRMED.\n");
        } else {
            printf("  INFO | no orphans with dead ns init as parent\n");
        }
    }

    /* Also scan for reparented children to init */
    {
        pid_t newkids[16];
        int n = find_proc_with_ppid(1, newkids, 16);
        printf("  INFO | processes reparented to init (pid 1): %d\n", n);
    }

    /* Poll waitpid for late zombies */
    {
        int wstatus = 0;
        pid_t wgot = waitpid_any_timeout(&wstatus, 5);
        if (wgot > 0)
            printf("  INFO | late zombie: pid=%d status=%d\n", wgot, wstatus);
        else
            printf("  INFO | no late zombie within 5s (no ns zap confirmed)\n");
    }

    printf("NS_ORPHAN_REAP_DIAG_DONE\n");
    TEST_DONE();
}
