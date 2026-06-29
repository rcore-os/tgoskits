/*
 * test-namespace — verify UTS, PID, mount and USER namespace semantics.
 *
 * Scenarios exercised:
 *   1. unshare(CLONE_NEWUTS) + sethostname does not affect the parent.
 *   2. clone(CLONE_NEWPID)  -> child getpid() returns the local PID.
 *   3. unshare(CLONE_NEWNS) isolates mount and umount operations.
 *   4. unshare(CLONE_NEWUSER) -> getuid() returns 65534 (nobody).
 */

#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

// ---- clone3 helpers (standardised across architectures) --------------------

#ifndef __NR_clone3
#if defined(__aarch64__)
#define __NR_clone3 435
#elif defined(__x86_64__)
#define __NR_clone3 435
#elif defined(__riscv)
#define __NR_clone3 435
#elif defined(__loongarch__) || defined(__loongarch64)
#define __NR_clone3 435
#else
#error "unknown architecture: define __NR_clone3"
#endif
#endif

struct clone3_args
{
    unsigned long long flags;       /* CLONE_* flags */
    unsigned long long pidfd;       /* PID fd for CLONE_PIDFD */
    unsigned long long child_tid;   /* address to store child TID */
    unsigned long long parent_tid;  /* address to store parent TID */
    unsigned long long exit_signal; /* exit signal */
    unsigned long long stack;       /* child stack (lowest address) */
    unsigned long long stack_size;  /* stack size */
    unsigned long long tls;         /* TLS descriptor */
    unsigned long long set_tid;     /* pointer to set_tid array */
    unsigned long long set_tid_size;/* number of elements in set_tid */
    unsigned long long cgroup;      /* CLONE_INTO_CGROUP fd */
};

static pid_t clone3_child(unsigned long long flags)
{
    struct clone3_args args;
    memset(&args, 0, sizeof(args));
    args.flags = flags;
    args.exit_signal = SIGCHLD;
    return (pid_t)syscall(__NR_clone3, &args, sizeof(args));
}

// ---------------------------------------------------------------------------

static void run_uts_namespace_test(void)
{
    int pipefd[2];
    int rc = pipe(pipefd);
    CHECK(rc == 0, "pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork for UTS test");

    if (child == 0)
    {
        /* ---- child ---------------------------------------------------- */
        close(pipefd[0]); /* close read end */

        /* Save the parent hostname before we change anything. */
        char parent_hostname[65];
        rc = gethostname(parent_hostname, sizeof(parent_hostname));
        CHECK(rc == 0, "child: gethostname before unshare");
        ssize_t wr = write(pipefd[1], parent_hostname, strlen(parent_hostname) + 1);
        (void)wr;

        /* Enter a new UTS namespace. */
        rc = unshare(CLONE_NEWUTS);
        CHECK_RET(rc, 0, "unshare(CLONE_NEWUTS)");

        /* Set a different hostname inside the new namespace. */
        const char *new_name = "newns-host";
        rc = sethostname(new_name, strlen(new_name));
        CHECK_RET(rc, 0, "sethostname in new UTS ns");

        /* Verify the hostname inside the child namespace. */
        char hostname[65];
        rc = gethostname(hostname, sizeof(hostname));
        CHECK_RET(rc, 0, "child: gethostname after sethostname");
        CHECK(strcmp(hostname, new_name) == 0, "child hostname == newname");

        close(pipefd[1]);
        _exit(0);
    }

    /* ---- parent -------------------------------------------------------- */
    close(pipefd[1]); /* close write end */

    /* Read the original hostname that the child captured. */
    char orig_hostname[65];
    ssize_t rd = read(pipefd[0], orig_hostname, sizeof(orig_hostname));
    CHECK(rd > 0, "parent: read original hostname from pipe");
    close(pipefd[0]);

    /* Wait for the child. */
    int status;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "UTS child exited 0");

    /* The parent's hostname must be unchanged. */
    char parent_now[65];
    rc = gethostname(parent_now, sizeof(parent_now));
    CHECK_RET(rc, 0, "parent: gethostname after child exit");
    CHECK(strcmp(parent_now, orig_hostname) == 0,
          "parent hostname unchanged after child unshare(CLONE_NEWUTS)");
}

static void run_pid_namespace_test(void)
{
    pid_t parent_pid = getpid();

    pid_t child = clone3_child(CLONE_NEWPID);
    CHECK(child >= 0, "clone3(CLONE_NEWPID)");

    if (child == 0)
    {
        /* ---- child ---------------------------------------------------- */
        pid_t my_pid = getpid();

        /* The first process in a new PID namespace is PID 1. */
        CHECK(my_pid == 1, "child in new PID namespace: getpid() == 1");

        /* The parent PID seen from inside must be 0
         * (the parent is in a different PID namespace). */
        pid_t ppid = getppid();
        CHECK(ppid == 0, "child getppid() == 0 (parent in different PID ns)");

        /* Verify the pid reported by getpid() is NOT the parent's pid.
         * This catches an implementation that fails to translate. */
        CHECK(my_pid != parent_pid,
              "child pid differs from parent pid (namespace isolation)");

        _exit(0);
    }

    /* ---- parent -------------------------------------------------------- */
    int status;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "PID child exited 0");

    /* Parent pid must not change. */
    pid_t now = getpid();
    CHECK(now == parent_pid, "parent getpid() unchanged after child clone");
}

static void run_user_namespace_test(void)
{
    /* Save pre-unshare uid for later comparison. */
    uid_t before = getuid();

    int rc = unshare(CLONE_NEWUSER);
    CHECK_RET(rc, 0, "unshare(CLONE_NEWUSER)");

    uid_t after = getuid();

    /* In a non-root user namespace, uid maps to the overflow uid (65534). */
    CHECK(after == 65534,
          "getuid() == 65534 after unshare(CLONE_NEWUSER)");

    /* Should differ from the pre-unshare value. */
    CHECK(after != before,
          "getuid() changed after unshare(CLONE_NEWUSER)");

    /* gid should also be 65534. */
    gid_t gid = getgid();
    CHECK(gid == 65534,
          "getgid() == 65534 after unshare(CLONE_NEWUSER)");
}

static void run_mount_namespace_test(void)
{
    const char *target = "/tmp/test-mount-namespace";
    const char *child_file = "/tmp/test-mount-namespace/child-only";
    const char *parent_file = "/tmp/test-mount-namespace/parent-only";
    int child_ready[2];
    int parent_ready[2];
    int original_ns = open("/proc/self/ns/mnt", O_RDONLY);

    CHECK_ERR(clone3_child(CLONE_NEWNS | CLONE_FS), EINVAL,
              "clone3 rejects CLONE_NEWNS | CLONE_FS");
    CHECK(original_ns >= 0, "open original mount namespace fd");
    unlink(child_file);
    unlink(parent_file);
    if (mkdir(target, 0755) != 0)
        CHECK(errno == EEXIST, "create mount namespace test directory");
    CHECK_RET(pipe(child_ready), 0, "create child-ready pipe");
    CHECK_RET(pipe(parent_ready), 0, "create parent-ready pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork for mount namespace test");
    if (child == 0)
    {
        close(child_ready[0]);
        close(parent_ready[1]);

        CHECK_RET(unshare(CLONE_NEWNS), 0, "child unshare(CLONE_NEWNS)");
        CHECK_RET(mount("none", target, "tmpfs", 0, NULL), 0,
                  "child mounts private tmpfs");

        int fd = open(child_file, O_CREAT | O_WRONLY, 0644);
        CHECK(fd >= 0, "child creates file in private mount");
        if (fd >= 0)
            close(fd);

        char byte = 'C';
        CHECK(write(child_ready[1], &byte, 1) == 1,
              "child signals private mount ready");
        CHECK(read(parent_ready[0], &byte, 1) == 1,
              "child waits for parent visibility check");

        errno = 0;
        CHECK(access(parent_file, F_OK) == -1 && errno == ENOENT,
              "child private mount hides parent underlying file");
        CHECK_RET(umount(target), 0, "child unmounts private tmpfs");

        close(child_ready[1]);
        close(parent_ready[0]);
        _exit(__fail > 0 ? 1 : 0);
    }

    close(child_ready[1]);
    close(parent_ready[0]);

    char byte;
    CHECK(read(child_ready[0], &byte, 1) == 1,
          "parent waits for child private mount");
    errno = 0;
    CHECK(access(child_file, F_OK) == -1 && errno == ENOENT,
          "child mount is not visible in parent namespace");

    char child_ns_path[64];
    snprintf(child_ns_path, sizeof(child_ns_path), "/proc/%d/ns/mnt", child);
    int child_ns = open(child_ns_path, O_RDONLY);
    CHECK(child_ns >= 0, "open child mount namespace fd");
    if (child_ns >= 0 && original_ns >= 0)
    {
        CHECK_RET(setns(child_ns, CLONE_NEWNS), 0,
                  "parent joins child mount namespace");
        CHECK(access(child_file, F_OK) == 0,
              "setns exposes child private mount");
        CHECK_RET(setns(original_ns, CLONE_NEWNS), 0,
                  "parent restores original mount namespace");
        errno = 0;
        CHECK(access(child_file, F_OK) == -1 && errno == ENOENT,
              "restored namespace hides child private mount");
    }
    if (child_ns >= 0)
        close(child_ns);

    int fd = open(parent_file, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "parent creates file below underlying mountpoint");
    if (fd >= 0)
        close(fd);
    byte = 'P';
    CHECK(write(parent_ready[1], &byte, 1) == 1,
          "parent releases child");

    int status;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "mount namespace child exited 0");
    CHECK(access(parent_file, F_OK) == 0,
          "parent underlying file remains after child umount");

    close(child_ready[0]);
    close(parent_ready[1]);
    if (original_ns >= 0)
        close(original_ns);
    unlink(parent_file);
    rmdir(target);
}

static void run_mount_propagation_test(void)
{
    const char *base = "/tmp/test-mount-propagation";
    const char *parent_mount = "/tmp/test-mount-propagation/from-parent";
    const char *child_mount = "/tmp/test-mount-propagation/from-child";
    const char *down_mount = "/tmp/test-mount-propagation/down-only";
    const char *up_mount = "/tmp/test-mount-propagation/up-blocked";
    int parent_to_child[2];
    int child_to_parent[2];
    char byte;

    mkdir(base, 0755);
    CHECK_RET(mount("none", base, "tmpfs", 0, NULL), 0,
              "mount propagation test root");
    mkdir(parent_mount, 0755);
    mkdir(child_mount, 0755);
    mkdir(down_mount, 0755);
    mkdir(up_mount, 0755);
    CHECK_RET(mount("none", base, "tmpfs", MS_SHARED, NULL), 0,
              "mark propagation root shared");
    CHECK_RET(pipe(parent_to_child), 0, "create propagation command pipe");
    CHECK_RET(pipe(child_to_parent), 0, "create propagation reply pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork for mount propagation test");
    if (child == 0)
    {
        close(parent_to_child[1]);
        close(child_to_parent[0]);
        CHECK_RET(unshare(CLONE_NEWNS), 0,
                  "child unshares shared mount namespace");
        byte = 'R';
        write(child_to_parent[1], &byte, 1);

        read(parent_to_child[0], &byte, 1);
        CHECK(access("/tmp/test-mount-propagation/from-parent/marker", F_OK) == 0,
              "shared mount propagates parent to child namespace");
        CHECK_RET(mount("none", child_mount, "tmpfs", 0, NULL), 0,
                  "child creates reverse shared propagation mount");
        int fd = open("/tmp/test-mount-propagation/from-child/marker",
                      O_CREAT | O_WRONLY, 0644);
        CHECK(fd >= 0, "child writes reverse propagation marker");
        if (fd >= 0)
            close(fd);
        byte = 'S';
        write(child_to_parent[1], &byte, 1);

        read(parent_to_child[0], &byte, 1);
        CHECK_RET(mount("none", base, "tmpfs", MS_SLAVE, NULL), 0,
                  "child converts shared root to slave");
        byte = 'L';
        write(child_to_parent[1], &byte, 1);

        read(parent_to_child[0], &byte, 1);
        CHECK(access("/tmp/test-mount-propagation/down-only/marker", F_OK) == 0,
              "slave receives mount from master namespace");
        CHECK_RET(mount("none", up_mount, "tmpfs", 0, NULL), 0,
                  "child mounts below slave root");
        fd = open("/tmp/test-mount-propagation/up-blocked/marker",
                  O_CREAT | O_WRONLY, 0644);
        CHECK(fd >= 0, "child writes marker below slave root");
        if (fd >= 0)
            close(fd);
        byte = 'U';
        write(child_to_parent[1], &byte, 1);

        read(parent_to_child[0], &byte, 1);
        CHECK_RET(umount(up_mount), 0, "child unmounts non-propagated slave child");
        close(parent_to_child[0]);
        close(child_to_parent[1]);
        _exit(__fail > 0 ? 1 : 0);
    }

    close(parent_to_child[0]);
    close(child_to_parent[1]);
    read(child_to_parent[0], &byte, 1);

    CHECK_RET(mount("none", parent_mount, "tmpfs", 0, NULL), 0,
              "parent mounts below shared root");
    int fd = open("/tmp/test-mount-propagation/from-parent/marker",
                  O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "parent writes shared propagation marker");
    if (fd >= 0)
        close(fd);
    byte = 'P';
    write(parent_to_child[1], &byte, 1);

    read(child_to_parent[0], &byte, 1);
    CHECK(access("/tmp/test-mount-propagation/from-child/marker", F_OK) == 0,
          "shared mount propagates child back to parent namespace");
    byte = 'C';
    write(parent_to_child[1], &byte, 1);

    read(child_to_parent[0], &byte, 1);
    CHECK_RET(mount("none", down_mount, "tmpfs", 0, NULL), 0,
              "parent mounts event for slave namespace");
    fd = open("/tmp/test-mount-propagation/down-only/marker",
              O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0, "parent writes slave propagation marker");
    if (fd >= 0)
        close(fd);
    byte = 'D';
    write(parent_to_child[1], &byte, 1);

    read(child_to_parent[0], &byte, 1);
    errno = 0;
    CHECK(access("/tmp/test-mount-propagation/up-blocked/marker", F_OK) == -1
              && errno == ENOENT,
          "slave mount does not propagate back to master namespace");
    byte = 'X';
    write(parent_to_child[1], &byte, 1);

    int status;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "mount propagation child exited 0");

    CHECK_RET(umount(down_mount), 0, "unmount slave propagation mount");
    CHECK_RET(umount(child_mount), 0, "unmount reverse shared mount");
    CHECK_RET(umount(parent_mount), 0, "unmount parent shared mount");
    CHECK_RET(mount("none", base, "tmpfs", MS_PRIVATE | MS_REC, NULL), 0,
              "make propagation tree recursively private");
    CHECK_RET(umount(base), 0, "unmount propagation test root");
    close(parent_to_child[1]);
    close(child_to_parent[0]);
    rmdir(base);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("namespace (UTS / PID / mount / USER isolation)");

    run_uts_namespace_test();
    run_pid_namespace_test();
    run_mount_namespace_test();
    run_mount_propagation_test();
    run_user_namespace_test();

    TEST_DONE();
}
