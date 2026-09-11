#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef OPEN_TREE_CLONE
#define OPEN_TREE_CLONE 1
#endif

static void check(int ok, const char *what)
{
    if (!ok) {
        fprintf(stderr, "FAIL: %s errno=%d (%s)\n", what, errno, strerror(errno));
        exit(1);
    }
}

static void signal_handler(int sig) { (void)sig; }

static void install_handler(int sig)
{
    struct sigaction sa = {.sa_handler = signal_handler};
    sigemptyset(&sa.sa_mask);
    check(sigaction(sig, &sa, NULL) == 0, "install interrupting handler");
}

static void full_fd_table(void)
{
    const char *path = "/tmp/fifo-emfile";
    unlink(path);
    check(mkfifo(path, 0600) == 0, "create EMFILE FIFO");
    struct rlimit limit = {.rlim_cur = 32, .rlim_max = 32};
    check(setrlimit(RLIMIT_NOFILE, &limit) == 0, "limit descriptor table");
    while (syscall(SYS_openat, AT_FDCWD, "/dev/null", O_RDONLY, 0) >= 0) {}
    check(errno == EMFILE, "fill descriptor table");
    install_handler(SIGALRM);
    struct itimerval timer = {
        .it_interval = {.tv_usec = 100000},
        .it_value = {.tv_usec = 100000},
    };
    check(setitimer(ITIMER_REAL, &timer, NULL) == 0, "arm EMFILE timeout");
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    int error = errno;
    timer = (struct itimerval){0};
    check(setitimer(ITIMER_REAL, &timer, NULL) == 0, "disarm EMFILE timeout");
    unlink(path);
    if (fd != -1 || error != EMFILE) {
        fprintf(stderr, "FAIL: FIFO descriptor reservation precedes wait: fd=%d errno=%d expected=%d\n",
                fd, error, EMFILE);
        exit(1);
    }
}

static void shared_mount_identity(void)
{
    int context = syscall(SYS_fsopen, "ramfs", 1);
    check(context >= 0, "create filesystem context");
    check(syscall(SYS_fsconfig, context, 6, NULL, NULL, 0) == 0, "create filesystem");
    int first = syscall(SYS_fsmount, context, 1, 0);
    check(first >= 0, "create first mount alias");
    int second = syscall(SYS_fsmount, context, 1, 0);
    /* Linux consumes the context; clone its mount when reuse returns EBUSY. */
    if (second < 0 && errno == EBUSY)
        second = syscall(SYS_open_tree, first, "", OPEN_TREE_CLONE | AT_EMPTY_PATH | O_CLOEXEC);
    check(second >= 0, "create second mount alias");
    check(mkfifoat(first, "channel", 0600) == 0, "create shared FIFO inode");
    int reader = syscall(SYS_openat, first, "channel", O_RDONLY | O_NONBLOCK, 0);
    check(reader >= 0, "open reader through first mount");
    int writer = syscall(SYS_openat, second, "channel", O_WRONLY | O_NONBLOCK, 0);
    int error = errno;
    if (writer < 0) {
        fprintf(stderr, "FAIL: mount aliases share FIFO channel: writer=%d errno=%d\n", writer, error);
        exit(1);
    }
    char byte = 0;
    check(syscall(SYS_write, writer, "x", 1) == 1 &&
          syscall(SYS_read, reader, &byte, 1) == 1 && byte == 'x', "mount aliases transfer FIFO data");
    close(writer);
    close(reader);
    close(second);
    close(first);
    close(context);
}

static void wait_for_pending_channel(const char *path)
{
    int probe = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY | O_NONBLOCK, 0);
    check(probe >= 0, "open FIFO capacity probe");
    int initial = fcntl(probe, F_GETPIPE_SZ);
    long page = sysconf(_SC_PAGESIZE);
    check(initial > 0 && page > 0, "read initial FIFO capacity");
    int marker = initial > page ? page : 2 * page;
    check(fcntl(probe, F_SETPIPE_SZ, marker) == marker, "mark pending FIFO channel");
    close(probe);
    struct timespec start, now;
    check(clock_gettime(CLOCK_MONOTONIC, &start) == 0, "read synchronization deadline");
    for (;;) {
        probe = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY | O_NONBLOCK, 0);
        check(probe >= 0, "reopen FIFO capacity probe");
        int capacity = fcntl(probe, F_GETPIPE_SZ);
        check(capacity > 0, "read reopened FIFO capacity");
        if (capacity == marker) {
            /* Only the pending child can retain the channel across our close. */
            close(probe);
            return;
        }
        check(fcntl(probe, F_SETPIPE_SZ, marker) == marker, "mark newly created FIFO channel");
        close(probe);
        check(clock_gettime(CLOCK_MONOTONIC, &now) == 0, "read synchronization clock");
        check(now.tv_sec - start.tv_sec < 5, "pending open retains FIFO channel");
        sched_yield();
    }
}

static void pending_open_busy(void)
{
    const char *mount_path = "/tmp/fifo-pending-mount";
    const char *path = "/tmp/fifo-pending-mount/channel";
    check(mkdir(mount_path, 0700) == 0, "create mount point");
    check(mount("ramfs", mount_path, "ramfs", 0, NULL) == 0, "mount FIFO filesystem");
    check(mkfifo(path, 0600) == 0, "create pending-open FIFO");
    int ready[2];
    check(pipe(ready) == 0, "create open-entry notification pipe");
    install_handler(SIGUSR1);
    pid_t child = fork();
    check(child >= 0, "fork pending FIFO reader");
    if (child == 0) {
        close(ready[0]);
        /* Tell the parent to begin probing; no FIFO descriptor is inherited. */
        if (write(ready[1], "r", 1) != 1)
            _exit(1);
        int fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
        _exit(fd == -1 && errno == EINTR ? 0 : 1);
    }
    close(ready[1]);
    char byte = 0;
    check(read(ready[0], &byte, 1) == 1 && byte == 'r', "child reaches FIFO open");
    close(ready[0]);
    wait_for_pending_channel(path);
    int result = syscall(SYS_umount2, mount_path, 0);
    int error = errno;
    check(kill(child, SIGUSR1) == 0, "interrupt pending FIFO open");
    int status;
    check(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "pending open releases state after interruption");
    if (result != -1 || error != EBUSY) {
        fprintf(stderr, "FAIL: pending FIFO open keeps mount busy: result=%d errno=%d expected=%d\n",
                result, error, EBUSY);
        exit(1);
    }
    check(syscall(SYS_umount2, mount_path, 0) == 0, "mount becomes idle after interrupted open");
    check(rmdir(mount_path) == 0, "remove mount point");
}

int fifo_boundary_tests(void)
{
    void (*cases[])(void) = {full_fd_table, shared_mount_identity, pending_open_busy};
    const char *names[] = {"FIFO EMFILE ordering", "FIFO mount aliases share channel", "FIFO pending-open mount busy"};
    int failed = 0;
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        fflush(NULL);
        pid_t child = fork();
        check(child >= 0, "fork boundary case");
        if (child == 0) {
            cases[i]();
            _exit(0);
        }
        int status;
        check(waitpid(child, &status, 0) == child, "wait boundary case");
        int ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
        printf("%s: %s\n", ok ? "PASS" : "FAIL", names[i]);
        failed += !ok;
    }
    return failed ? 1 : 0;
}
