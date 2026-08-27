/*
 * test-namespace — verify UTS, PID and USER namespace semantics.
 *
 * Scenarios exercised:
 *   1. unshare(CLONE_NEWUTS) + sethostname does not affect the parent.
 *   2. clone(CLONE_NEWPID)  -> child getpid() returns the local PID.
 *   3. unshare(CLONE_NEWUSER) -> getuid() returns 65534 (nobody).
 *   4. clone3(CLONE_THREAD | CLONE_NEWPID) is rejected with EINVAL.
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

struct clone3_args_extended
{
    struct clone3_args args;
    unsigned long long extension;
};

static void run_clone3_size_validation_test(void)
{
    struct clone3_args_extended extended;

    errno = 0;
    CHECK(syscall(__NR_clone3, NULL, 4097U) == -1 && errno == E2BIG,
          "clone3 rejects oversized argument structures before user-memory access");

    memset(&extended, 0, sizeof(extended));
    extended.args.flags = CLONE_THREAD;
    extended.args.exit_signal = SIGCHLD;
    extended.extension = 1;
    errno = 0;
    CHECK(syscall(__NR_clone3, &extended, sizeof(extended)) == -1 && errno == E2BIG,
          "clone3 rejects nonzero extension bytes before argument validation");
}

static pid_t clone3_child(unsigned long long flags)
{
    struct clone3_args args;
    memset(&args, 0, sizeof(args));
    args.flags = flags;
    args.exit_signal = SIGCHLD;
    return (pid_t)syscall(__NR_clone3, &args, sizeof(args));
}

static void run_pid_namespace_flag_validation_test(void)
{
    pid_t verifier = fork();
    CHECK(verifier >= 0, "fork for PID namespace flag validation");

    if (verifier == 0)
    {
        struct clone3_args args;
        memset(&args, 0, sizeof(args));
        args.flags = CLONE_THREAD | CLONE_VM | CLONE_SIGHAND | CLONE_NEWPID;

        errno = 0;
        long result = syscall(__NR_clone3, &args, sizeof(args));
        if (result == -1)
            _exit(errno == EINVAL ? 0 : 2);

        syscall(SYS_exit_group, 1);
        __builtin_unreachable();
    }

    int status;
    CHECK(waitpid(verifier, &status, 0) == verifier,
          "wait for PID namespace flag verifier");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "clone3(CLONE_THREAD | CLONE_NEWPID) rejected with EINVAL");
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

static void run_persistent_pid_namespace_for_children_test(void)
{
    pid_t verifier = fork();
    CHECK(verifier >= 0, "fork persistent PID namespace verifier");

    if (verifier == 0)
    {
        int init_ready[2];
        int init_release[2];
        int second_pid_pipe[2];
        if (pipe(init_ready) != 0 || pipe(init_release) != 0 ||
            pipe(second_pid_pipe) != 0)
            _exit(2);

        if (unshare(CLONE_NEWPID) != 0)
            _exit(3);

        pid_t namespace_init = fork();
        if (namespace_init < 0)
            _exit(4);
        if (namespace_init == 0)
        {
            close(init_ready[0]);
            close(init_release[1]);
            close(second_pid_pipe[0]);
            close(second_pid_pipe[1]);

            pid_t visible_pid = getpid();
            if (write(init_ready[1], &visible_pid, sizeof(visible_pid)) !=
                (ssize_t)sizeof(visible_pid))
                _exit(5);
            close(init_ready[1]);

            char release;
            if (read(init_release[0], &release, sizeof(release)) !=
                (ssize_t)sizeof(release))
                _exit(6);
            close(init_release[0]);
            _exit(visible_pid == 1 ? 0 : 7);
        }

        close(init_ready[1]);
        close(init_release[0]);
        pid_t first_visible_pid = 0;
        if (read(init_ready[0], &first_visible_pid, sizeof(first_visible_pid)) !=
            (ssize_t)sizeof(first_visible_pid) ||
            first_visible_pid != 1)
            _exit(8);
        close(init_ready[0]);

        pid_t second = fork();
        if (second < 0)
            _exit(9);
        if (second == 0)
        {
            close(init_release[1]);
            close(second_pid_pipe[0]);
            pid_t visible_pid = getpid();
            if (write(second_pid_pipe[1], &visible_pid, sizeof(visible_pid)) !=
                (ssize_t)sizeof(visible_pid))
                _exit(10);
            close(second_pid_pipe[1]);
            _exit(0);
        }

        close(second_pid_pipe[1]);
        pid_t second_visible_pid = 0;
        if (read(second_pid_pipe[0], &second_visible_pid,
                 sizeof(second_visible_pid)) !=
            (ssize_t)sizeof(second_visible_pid))
            _exit(11);
        close(second_pid_pipe[0]);

        int second_status;
        if (waitpid(second, &second_status, 0) != second ||
            !WIFEXITED(second_status) || WEXITSTATUS(second_status) != 0)
            _exit(12);

        char release = 'R';
        if (write(init_release[1], &release, sizeof(release)) !=
            (ssize_t)sizeof(release))
            _exit(13);
        close(init_release[1]);

        int init_status;
        if (waitpid(namespace_init, &init_status, 0) != namespace_init ||
            !WIFEXITED(init_status) || WEXITSTATUS(init_status) != 0)
            _exit(14);

        _exit(second_visible_pid == 2 ? 0 : 15);
    }

    int status;
    CHECK(waitpid(verifier, &status, 0) == verifier,
          "wait persistent PID namespace verifier");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "pid_ns_for_children persists across multiple forks");
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

    run_clone3_size_validation_test();
    run_uts_namespace_test();
    run_pid_namespace_flag_validation_test();
    run_pid_namespace_test();
    run_persistent_pid_namespace_for_children_test();
    run_user_namespace_test();

    TEST_DONE();
}
