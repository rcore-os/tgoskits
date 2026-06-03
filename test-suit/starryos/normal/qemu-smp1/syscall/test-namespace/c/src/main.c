/*
 * test-namespace — verify UTS, PID and USER namespace semantics.
 *
 * Scenarios exercised:
 *   1. unshare(CLONE_NEWUTS) + sethostname does not affect the parent.
 *   2. clone(CLONE_NEWPID)  -> child getpid() returns the local PID.
 *   3. unshare(CLONE_NEWUSER) -> getuid() returns 65534 (nobody).
 */

#include "test_framework.h"

#include <errno.h>
#include <sched.h>
#include <string.h>
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

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("namespace (UTS / PID / USER isolation)");

    run_uts_namespace_test();
    run_pid_namespace_test();
    run_user_namespace_test();

    TEST_DONE();
}
