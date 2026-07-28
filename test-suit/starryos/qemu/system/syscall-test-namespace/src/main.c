/*
 * test-namespace — verify UTS, PID and USER namespace semantics.
 *
 * Scenarios exercised:
 *   1. unshare(CLONE_NEWUTS) + sethostname does not affect the parent.
 *   2. clone(CLONE_NEWPID)  -> child getpid() returns the local PID.
 *   3. PID namespace shutdown drains a newly reparented WNOWAIT zombie.
 *   4. unshare(CLONE_NEWUSER) -> getuid() returns 65534 (nobody).
 */

#include "test_framework.h"

#include <errno.h>
#include <sched.h>
#include <signal.h>
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

#define NAMESPACE_SHUTDOWN_TIMEOUT_MS 5000
#define WAIT_POLL_INTERVAL_US 10000

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

static void write_exact(int fd, const void *buffer, size_t length)
{
    const unsigned char *cursor = buffer;
    while (length > 0)
    {
        ssize_t written = write(fd, cursor, length);
        CHECK(written > 0, "write_exact");
        if (written <= 0)
        {
            return;
        }
        cursor += (size_t)written;
        length -= (size_t)written;
    }
}

static void read_exact(int fd, void *buffer, size_t length)
{
    unsigned char *cursor = buffer;
    while (length > 0)
    {
        ssize_t received = read(fd, cursor, length);
        CHECK(received > 0, "read_exact");
        if (received <= 0)
        {
            return;
        }
        cursor += (size_t)received;
        length -= (size_t)received;
    }
}

static pid_t waitpid_with_timeout(pid_t child, int *status, int timeout_ms)
{
    int waited_ms = 0;
    for (;;)
    {
        pid_t waited = waitpid(child, status, WNOHANG);
        if (waited != 0)
        {
            return waited;
        }
        if (waited_ms >= timeout_ms)
        {
            return 0;
        }
        usleep(WAIT_POLL_INTERVAL_US);
        waited_ms += WAIT_POLL_INTERVAL_US / 1000;
    }
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
        int failures_before_pid_test = __fail;
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

        int observed_pipe[2];
        int release_pipe[2];
        CHECK(pipe(observed_pipe) == 0, "PID namespace orphan observation pipe");
        CHECK(pipe(release_pipe) == 0, "PID namespace orphan release pipe");

        pid_t intermediate = fork();
        CHECK(intermediate >= 0, "fork intermediate PID namespace parent");
        if (intermediate == 0)
        {
            pid_t orphan = fork();
            CHECK(orphan >= 0, "fork PID namespace orphan");
            if (orphan == 0)
            {
                close(observed_pipe[0]);
                close(release_pipe[1]);

                const char ready = 'R';
                write_exact(observed_pipe[1], &ready, sizeof(ready));

                char release;
                read_exact(release_pipe[0], &release, sizeof(release));
                pid_t observed_parent = getppid();
                write_exact(observed_pipe[1], &observed_parent,
                            sizeof(observed_parent));
                _exit(observed_parent == 1 ? 0 : 1);
            }
            _exit(0);
        }

        close(observed_pipe[1]);
        close(release_pipe[0]);

        char ready;
        read_exact(observed_pipe[0], &ready, sizeof(ready));
        CHECK(ready == 'R', "PID namespace orphan reached observation barrier");

        int intermediate_status;
        CHECK(waitpid(intermediate, &intermediate_status, 0) == intermediate,
              "wait for intermediate PID namespace parent");
        CHECK(WIFEXITED(intermediate_status)
                  && WEXITSTATUS(intermediate_status) == 0,
              "intermediate PID namespace parent exited 0");

        const char release = 'G';
        write_exact(release_pipe[1], &release, sizeof(release));

        pid_t observed_parent;
        read_exact(observed_pipe[0], &observed_parent, sizeof(observed_parent));
        CHECK(observed_parent == 1,
              "orphan is reparented to init in its PID namespace");

        int orphan_status;
        CHECK(waitpid(-1, &orphan_status, 0) > 0,
              "PID namespace init reaps the adopted orphan");
        CHECK(WIFEXITED(orphan_status) && WEXITSTATUS(orphan_status) == 0,
              "adopted PID namespace orphan exited 0");

        close(observed_pipe[0]);
        close(release_pipe[1]);
        _exit(__fail == failures_before_pid_test ? 0 : 1);
    }

    /* ---- parent -------------------------------------------------------- */
    int status;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "PID child exited 0");

    /* Parent pid must not change. */
    pid_t now = getpid();
    CHECK(now == parent_pid, "parent getpid() unchanged after child clone");
}

static void run_pid_namespace_shutdown_test(void)
{
    pid_t namespace_init = clone3_child(CLONE_NEWPID);
    CHECK(namespace_init >= 0, "clone namespace init for shutdown test");

    if (namespace_init == 0)
    {
        int zombie_ready[2];
        if (pipe(zombie_ready) != 0)
        {
            _exit(2);
        }

        pid_t holder = fork();
        if (holder < 0)
        {
            _exit(3);
        }
        if (holder == 0)
        {
            close(zombie_ready[0]);
            pid_t zombie = fork();
            if (zombie < 0)
            {
                _exit(4);
            }
            if (zombie == 0)
            {
                _exit(0);
            }

            siginfo_t observation;
            memset(&observation, 0, sizeof(observation));
            if (waitid(P_PID, (id_t)zombie, &observation,
                       WEXITED | WNOWAIT) != 0)
            {
                _exit(5);
            }
            const char ready = 'Z';
            if (write(zombie_ready[1], &ready, sizeof(ready))
                != (ssize_t)sizeof(ready))
            {
                _exit(7);
            }
            for (;;)
            {
                pause();
            }
        }

        close(zombie_ready[1]);
        char ready = 0;
        read_exact(zombie_ready[0], &ready, sizeof(ready));
        close(zombie_ready[0]);
        if (ready != 'Z')
        {
            _exit(6);
        }

        /*
         * The namespace init exits while a live parent retains a WNOWAIT
         * zombie. Shutdown must kill the parent, service the newly reparented
         * zombie, and only then release PID 1.
         */
        _exit(0);
    }

    int status;
    pid_t waited = waitpid_with_timeout(
        namespace_init, &status, NAMESPACE_SHUTDOWN_TIMEOUT_MS);
    CHECK(waited == namespace_init,
          "wait for PID namespace shutdown");
    if (waited != namespace_init)
    {
        kill(namespace_init, SIGKILL);
        return;
    }
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "PID namespace init exits after zombie shutdown");
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
    run_pid_namespace_shutdown_test();
    run_user_namespace_test();

    TEST_DONE();
}
