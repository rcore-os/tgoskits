/*
 * test-pty-fork-echo — NIXPKGS-SANDBOX-001:
 * Reproduces nix daemon→builder PTY communication failure.
 *
 * Nix uses a pseudoterminal (PTY) pair for daemon↔builder communication:
 * the daemon opens BOTH master and slave, then forks the builder child.
 * The child dup2's the inherited slave fd to stderr, then enters the sandbox
 * (clone namespaces + mount operations), then writes \2\n to stderr to
 * signal success.
 *
 * Test cases:
 *   A. Basic: openpty → fork → child opens slave → write → parent reads
 *   B. Inherited fd: parent opens both master+slave → fork → child dup2 slave
 *      to stderr → write → parent reads
 *   C. Inherited fd + unshare(CLONE_NEWNS): like B but child calls unshare
 *      before writing (simulates nix sandbox entry)
 *
 * On StarryOS (bug): case C fails — PTY slave writes after unshare(NEWNS)
 *   don't reach the master.
 * On Linux (correct): all cases pass.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PTY_TIMEOUT_MS  5000
#define PTY_MARKER_A    "PTY_ECHO_A"
#define PTY_MARKER_B    "PTY_ECHO_B"
#define PTY_MARKER_C    "PTY_ECHO_C"

static int test_basic_pty(void)
{
    printf("\n--- Case A: basic PTY (child opens slave) ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    pid_t child = fork();
    if (child < 0) { close(master); return 1; }
    if (child == 0) {
        close(master);
        int slave = open(slave_name, O_RDWR);
        if (slave >= 0) {
            write(slave, PTY_MARKER_A "\n", sizeof(PTY_MARKER_A));
            close(slave);
        }
        _exit(0);
    }

    char buf[64];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_A) != NULL; }
    printf("  Case A: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) printf("  DIAG A: '%s'\n", n > 0 ? buf : "(EOF)");

    waitpid(child, NULL, 0);
    close(master);
    return ok ? 0 : 1;
}

static int test_inherited_pty(void)
{
    printf("\n--- Case B: inherited PTY fd (parent opens slave) ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    /* Parent opens slave (nix pattern: daemon opens both sides) */
    int parent_slave = open(slave_name, O_RDWR);
    if (parent_slave < 0) {
        printf("  FAIL parent open slave: errno=%d\n", errno);
        close(master); return 1;
    }

    pid_t child = fork();
    if (child < 0) { close(master); close(parent_slave); return 1; }
    if (child == 0) {
        close(master);
        /* dup2 inherited slave fd to stderr (nix pattern) */
        dup2(parent_slave, STDERR_FILENO);
        close(parent_slave);
        write(STDERR_FILENO, PTY_MARKER_B "\n", sizeof(PTY_MARKER_B));
        _exit(0);
    }

    close(parent_slave);

    char buf[64];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_B) != NULL; }
    printf("  Case B: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) printf("  DIAG B: '%s'\n", n > 0 ? buf : "(EOF)");

    waitpid(child, NULL, 0);
    close(master);
    return ok ? 0 : 1;
}

static int test_inherited_pty_unshare(void)
{
    printf("\n--- Case C: inherited PTY fd + unshare(CLONE_NEWNS) ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    int parent_slave = open(slave_name, O_RDWR);
    if (parent_slave < 0) {
        printf("  FAIL parent open slave: errno=%d\n", errno);
        close(master); return 1;
    }

    pid_t child = fork();
    if (child < 0) { close(master); close(parent_slave); return 1; }
    if (child == 0) {
        close(master);
        dup2(parent_slave, STDERR_FILENO);
        close(parent_slave);

        /* Enter new mount namespace — simulates nix sandbox entry */
        if (unshare(CLONE_NEWNS) != 0) {
            dprintf(STDERR_FILENO, "CASE_C_UNSHARE_FAILED errno=%d\n", errno);
            _exit(1);
        }

        /* Write marker AFTER namespace transition (nix pattern) */
        write(STDERR_FILENO, PTY_MARKER_C "\n", sizeof(PTY_MARKER_C));
        _exit(0);
    }

    close(parent_slave);

    char buf[64];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_C) != NULL; }
    printf("  Case C: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) {
        printf("  DIAG C: '%s'\n", n > 0 ? buf : (n == 0 ? "(EOF)" : "(error)"));
        if (n == 0) printf("  ROOT CAUSE: PTY slave fd (opened in parent NS) "
                           "does not work after child unshare(CLONE_NEWNS)\n");
    }

    waitpid(child, NULL, 0);
    close(master);
    return ok ? 0 : 1;
}

/* Stack for clone() child — must be page-aligned */
static char clone_stack[65536] __attribute__((aligned(4096)));

static int clone_child_pty(void *arg)
{
    int slave_fd = *(int *)arg;
    dup2(slave_fd, STDERR_FILENO);
    close(slave_fd);
    write(STDERR_FILENO, PTY_MARKER_C "D\n", sizeof(PTY_MARKER_C) + 1);
    _exit(0);
    return 0;
}

static int test_clone_pty(void)
{
    printf("\n--- Case D: clone(NEWNS|NEWNET|NEWPID|NEWIPC|NEWUTS) + inherited PTY fd ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    int parent_slave = open(slave_name, O_RDWR);
    if (parent_slave < 0) {
        printf("  FAIL parent open slave: errno=%d\n", errno);
        close(master); return 1;
    }

    /* nix's clone flags: NEWNS|NEWNET|NEWPID|NEWIPC|NEWUTS|PARENT|SIGCHLD */
    int flags = CLONE_NEWNS | CLONE_NEWNET | CLONE_NEWPID
              | CLONE_NEWIPC | CLONE_NEWUTS | SIGCHLD;
    pid_t child = clone(clone_child_pty,
                         clone_stack + sizeof(clone_stack),
                         flags, &parent_slave, NULL, NULL, NULL);
    if (child < 0) {
        printf("  FAIL clone: errno=%d (%s)\n", errno, strerror(errno));
        close(master); close(parent_slave); return 1;
    }
    close(parent_slave);

    char buf[64];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_C) != NULL; }
    printf("  Case D: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) {
        printf("  DIAG D: '%s'\n", n > 0 ? buf : (n == 0 ? "(EOF)" : "(error)"));
        if (n == 0) printf("  ROOT CAUSE: PTY slave fd does not work after clone(NEWNS|...)\n");
    }

    waitpid(child, NULL, 0);
    close(master);
    return ok ? 0 : 1;
}

/* Stack for clone() child — must be page-aligned */
static char clone_stack2[65536] __attribute__((aligned(4096)));
static int clone_master_fd;
static int clone_slave_arg;

static int clone_child_mount_then_write(void *arg __attribute__((unused)))
{
    int slave_fd = clone_slave_arg;
    int master_copy __attribute__((unused)) = clone_master_fd;
    dup2(slave_fd, STDERR_FILENO);
    close(slave_fd);

    /* Simulate what nix does after clone: mount /proc in new namespace */
    if (mount("proc", "/proc", "proc", 0, "") != 0) {
        dprintf(STDERR_FILENO, "CASE_E_MOUNT_PROC_FAILED errno=%d\n", errno);
        _exit(1);
    }

    dprintf(STDERR_FILENO, PTY_MARKER_C "E\n");
    _exit(0);
    return 0;
}

static int test_clone_mount_pty(void)
{
    printf("\n--- Case E: clone(NEWNS|NEWNET|NEWPID|NEWIPC|NEWUTS) + mount /proc + PTY ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    int parent_slave = open(slave_name, O_RDWR);
    if (parent_slave < 0) {
        printf("  FAIL parent open slave: errno=%d\n", errno);
        close(master); return 1;
    }

    clone_master_fd = master;
    clone_slave_arg = parent_slave;

    int flags = CLONE_NEWNS | CLONE_NEWNET | CLONE_NEWPID
              | CLONE_NEWIPC | CLONE_NEWUTS | SIGCHLD;
    pid_t child = clone(clone_child_mount_then_write,
                         clone_stack2 + sizeof(clone_stack2),
                         flags, NULL, NULL, NULL, NULL);
    if (child < 0) {
        printf("  FAIL clone: errno=%d (%s)\n", errno, strerror(errno));
        close(master); close(parent_slave); return 1;
    }
    close(parent_slave);

    char buf[128];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_C) != NULL; }
    printf("  Case E: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) {
        printf("  DIAG E: '%s'\n", n > 0 ? buf : (n == 0 ? "(EOF)" : "(error)"));
        if (n == 0) printf("  ROOT CAUSE: PTY breaks after clone + mount /proc\n");
    }

    waitpid(child, NULL, 0);
    close(master);
    return ok ? 0 : 1;
}

/* ---- Case F: clone with CLONE_PARENT (nix uses this) ---- */
static char clone_stackF[65536] __attribute__((aligned(4096)));
static int cloneF_slave;

static int clone_childF(void *arg __attribute__((unused)))
{
    int slave_fd = cloneF_slave;
    /* Prove child executed: touch a file */
    int probe = open("/tmp/pty-casef-child-ran", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (probe >= 0) {
        dprintf(probe, "CHILD_RAN pid=%d ppid=%d\n", getpid(), getppid());
        close(probe);
    }
    dup2(slave_fd, STDERR_FILENO);
    close(slave_fd);
    dprintf(STDERR_FILENO, PTY_MARKER_C "F pid=%d ppid=%d\n", getpid(), getppid());
    _exit(0);
    return 0;
}

static int test_clone_parent_flag(void)
{
    printf("\n--- Case F: clone(NEWNS|NEWPID|CLONE_PARENT|SIGCHLD) + PTY ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    int parent_slave = open(slave_name, O_RDWR);
    if (parent_slave < 0) {
        printf("  FAIL parent open slave: errno=%d\n", errno);
        close(master); return 1;
    }
    cloneF_slave = parent_slave;

    /* Spawn a HELPER that does the clone (nix pattern: daemon → helper → clone) */
    pid_t helper = fork();
    if (helper < 0) { close(master); close(parent_slave); return 1; }
    if (helper == 0) {
        /* Helper process: do the actual clone */
        int flags = CLONE_NEWNS | CLONE_NEWPID | CLONE_PARENT | SIGCHLD;
        pid_t child = clone(clone_childF,
                             clone_stackF + sizeof(clone_stackF),
                             flags, NULL, NULL, NULL, NULL);
        if (child < 0) {
            printf("  FAIL clone in helper: errno=%d\n", errno);
            _exit(1);
        }
        /* Helper exits — child's parent becomes the original process (our parent PID) */
        _exit(0);
    }
    waitpid(helper, NULL, 0);
    close(parent_slave);

    char buf[128];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_C) != NULL; }
    printf("  Case F: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) {
        printf("  DIAG F: '%s'\n", n > 0 ? buf : (n == 0 ? "(EOF)" : "(error)"));
        if (n == 0) printf("  ROOT CAUSE: CLONE_PARENT breaks PTY communication\n");
    }

    /* Wait for the cloned child (now our direct child due to CLONE_PARENT) */
    int status;
    waitpid(-1, &status, 0);

    /* Confirm child actually ran */
    printf("  DIAG F: child-alive probe: ");
    int probe_fd = open("/tmp/pty-casef-child-ran", O_RDONLY);
    if (probe_fd >= 0) {
        char probe_buf[128];
        ssize_t pn = read(probe_fd, probe_buf, sizeof(probe_buf) - 1);
        if (pn > 0) { probe_buf[pn] = 0; printf("%s", probe_buf); }
        else { printf("(empty)\n"); }
        close(probe_fd);
        unlink("/tmp/pty-casef-child-ran");
    } else {
        printf("(child did NOT run!)\n");
    }

    close(master);
    return ok ? 0 : 1;
}

/* ---- Case G: clone(NEWNS) + second unshare(NEWNS) (nix does this) ---- */
static char clone_stackG[65536] __attribute__((aligned(4096)));
static int cloneG_slave, cloneG_master;

static int clone_childG(void *arg __attribute__((unused)))
{
    int slave_fd = cloneG_slave;
    dup2(slave_fd, STDERR_FILENO);
    close(slave_fd);

    /* Second mount namespace unshare (nix enterChroot does this) */
    if (unshare(CLONE_NEWNS) != 0) {
        dprintf(STDERR_FILENO, "CASE_G_UNSHARE2_FAILED errno=%d\n", errno);
        _exit(1);
    }

    dprintf(STDERR_FILENO, PTY_MARKER_C "G pid=%d\n", getpid());
    _exit(0);
    return 0;
}

static int test_double_unshare_pty(void)
{
    printf("\n--- Case G: clone(NEWNS) + second unshare(NEWNS) + PTY ---\n");
    int ok = 1;

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) { printf("  FAIL posix_openpt\n"); return 1; }
    grantpt(master);
    unlockpt(master);
    const char *slave_name = ptsname(master);
    if (!slave_name) { close(master); return 1; }

    int parent_slave = open(slave_name, O_RDWR);
    if (parent_slave < 0) {
        printf("  FAIL parent open slave: errno=%d\n", errno);
        close(master); return 1;
    }
    cloneG_slave = parent_slave;
    cloneG_master = master;

    int flags = CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWIPC | CLONE_NEWUTS | SIGCHLD;
    pid_t child = clone(clone_childG,
                         clone_stackG + sizeof(clone_stackG),
                         flags, NULL, NULL, NULL, NULL);
    if (child < 0) {
        printf("  FAIL clone: errno=%d (%s)\n", errno, strerror(errno));
        close(master); close(parent_slave); return 1;
    }
    close(parent_slave);

    char buf[128];
    struct pollfd pfd = { .fd = master, .events = POLLIN };
    if (poll(&pfd, 1, PTY_TIMEOUT_MS) <= 0) { ok = 0; }
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n <= 0) { ok = 0; }
    else { buf[n] = 0; ok = strstr(buf, PTY_MARKER_C) != NULL; }
    printf("  Case G: %s (read=%zd)\n", ok ? "PASS" : "FAIL", n);
    if (!ok) {
        printf("  DIAG G: '%s'\n", n > 0 ? buf : (n == 0 ? "(EOF)" : "(error)"));
        if (n == 0) printf("  ROOT CAUSE: second unshare(NEWNS) breaks PTY\n");
    }

    waitpid(child, NULL, 0);
    close(master);
    return ok ? 0 : 1;
}

/* ---- Case H: CLONE_PARENT without PTY (isolate clone bug) ---- */
static char clone_stackH[65536] __attribute__((aligned(4096)));
static volatile int cloneH_ran = 0;

static int clone_childH(void *arg __attribute__((unused)))
{
    /* Touch a file to prove the child executed */
    int fd = open("/tmp/pty-caseh-child-ran", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) {
        dprintf(fd, "CLONE_PARENT_CHILD pid=%d ppid=%d\n", getpid(), getppid());
        close(fd);
    }
    cloneH_ran = 1;
    _exit(0);
    return 0;
}

static int test_clone_parent_standalone(void)
{
    printf("\n--- Case H: clone(NEWPID|CLONE_PARENT|SIGCHLD) — no PTY, file probe only ---\n");
    int ok = 1;
    unlink("/tmp/pty-caseh-child-ran");

    /* Spawn helper that does the clone */
    pid_t helper = fork();
    if (helper < 0) { return 1; }
    if (helper == 0) {
        int flags = CLONE_NEWPID | CLONE_PARENT | SIGCHLD;
        pid_t child = clone(clone_childH,
                             clone_stackH + sizeof(clone_stackH),
                             flags, NULL, NULL, NULL, NULL);
        if (child < 0) {
            printf("  FAIL clone in helper: errno=%d (%s)\n", errno, strerror(errno));
            _exit(1);
        }
        printf("  DIAG H: helper cloned child pid=%d, helper exiting\n", child);
        _exit(0);
    }
    waitpid(helper, NULL, 0);

    /* Wait for the cloned child (it becomes our child due to CLONE_PARENT) */
    int status = 0;
    pid_t waited = waitpid(-1, &status, WNOHANG);
    if (waited > 0) {
        printf("  DIAG H: collected child pid=%d status=%d\n", waited, status);
    } else if (waited == 0) {
        /* Child might still be running or was reaped by init */
        printf("  DIAG H: no child ready (WNOHANG), trying blocking wait\n");
        waited = waitpid(-1, &status, 0);
        if (waited > 0) printf("  DIAG H: collected child pid=%d status=%d\n", waited, status);
    }

    /* Check if child actually ran */
    int probe_fd = open("/tmp/pty-caseh-child-ran", O_RDONLY);
    if (probe_fd >= 0) {
        char buf[128];
        ssize_t n = read(probe_fd, buf, sizeof(buf) - 1);
        if (n > 0) { buf[n] = 0; printf("  DIAG H: %s", buf); ok = 1; }
        else { ok = 0; }
        close(probe_fd);
        unlink("/tmp/pty-caseh-child-ran");
    } else {
        ok = 0;
    }

    printf("  Case H: %s (child %s)\n", ok ? "PASS" : "FAIL",
           ok ? "ran" : "did NOT run");
    if (!ok) {
        printf("  ROOT CAUSE: clone(CLONE_PARENT) creates non-viable child\n");
        printf("             The child process never executes its callback.\n");
        printf("             Nix uses this flag for sandbox builder — builder never starts.\n");
        printf("             Fix: implement CLONE_PARENT in StarryOS clone() syscall.\n");
    }
    return ok ? 0 : 1;
}

int main(void)
{
    printf("NIX_DEBUG_PTY_FORK_ECHO_BEGIN\n");
    TEST_START("pty-fork-echo: PTY communication after fork + unshare(NEWNS)");

    int failed = 0;
    failed |= test_basic_pty();
    failed |= test_inherited_pty();
    failed |= test_inherited_pty_unshare();
    failed |= test_clone_pty();
    failed |= test_clone_mount_pty();
    failed |= test_clone_parent_flag();
    failed |= test_double_unshare_pty();
    failed |= test_clone_parent_standalone();

    CHECK(!failed, "All PTY communication cases pass");

    if (!failed) {
        printf("NIX_DEBUG_PTY_FORK_ECHO_PASSED\n");
    }
    return failed;
}

