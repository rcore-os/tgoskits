/*
 * Deterministic PID-identity transfer regression for non-leader execve.
 *
 * A parent opens one process pidfd for the child leader identity and one
 * PIDFD_THREAD pidfd for the non-leader thread that will exec. After exec:
 *   - the process pidfd must still address the same process generation;
 *   - getpid() must retain the pre-exec TGID and gettid() must equal it;
 *   - the old thread pidfd must remain attached to the exited thread
 *     generation and must not redirect to the new leader runtime task.
 */

#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PIDFD_THREAD
#define PIDFD_THREAD 128u
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(SYS_pidfd_open, pid, flags);
}

static int x_pidfd_send_signal(int pidfd, int sig)
{
    return (int)syscall(SYS_pidfd_send_signal, pidfd, sig, NULL, 0u);
}

struct exec_context {
    const char *self;
    int tid_write;
    int exec_go_read;
    int ready_write;
    int finish_read;
};

static void *nonleader_exec(void *opaque)
{
    const struct exec_context *ctx = opaque;
    pid_t tid = (pid_t)syscall(SYS_gettid);
    if (write(ctx->tid_write, &tid, sizeof(tid)) != (ssize_t)sizeof(tid))
        _exit(41);

    char go;
    if (read(ctx->exec_go_read, &go, 1) != 1)
        _exit(42);

    char expected_pid[32];
    char ready_fd[32];
    char finish_fd[32];
    snprintf(expected_pid, sizeof(expected_pid), "%ld", (long)getpid());
    snprintf(ready_fd, sizeof(ready_fd), "%d", ctx->ready_write);
    snprintf(finish_fd, sizeof(finish_fd), "%d", ctx->finish_read);
    char *argv[] = {
        (char *)ctx->self,
        (char *)"post-exec",
        expected_pid,
        ready_fd,
        finish_fd,
        NULL,
    };
    execv(ctx->self, argv);
    (void)write(ctx->ready_write, "E", 1);
    _exit(43);
}

static int post_exec_main(char **argv)
{
    char *end = NULL;
    long expected_pid = strtol(argv[2], &end, 10);
    if (end == argv[2] || *end != '\0')
        return 51;
    int ready_write = (int)strtol(argv[3], &end, 10);
    if (end == argv[3] || *end != '\0')
        return 52;
    int finish_read = (int)strtol(argv[4], &end, 10);
    if (end == argv[4] || *end != '\0')
        return 53;

    pid_t pid = getpid();
    pid_t tid = (pid_t)syscall(SYS_gettid);
    char result = (pid == expected_pid && tid == pid) ? 'R' : 'F';
    if (write(ready_write, &result, 1) != 1)
        return 54;

    char finish;
    if (read(finish_read, &finish, 1) != 1)
        return 55;
    if (result != 'R') {
        fprintf(stderr,
                "FAIL: post-exec expected TGID=%ld, observed pid=%ld tid=%ld\n",
                expected_pid, (long)pid, (long)tid);
        return 56;
    }
    puts("PID_IDENTITY_EXEC_CHILD_OK");
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 5 && strcmp(argv[1], "post-exec") == 0)
        return post_exec_main(argv);

    TEST_START("non-leader exec PID identity transfer");
    signal(SIGPIPE, SIG_IGN);

    int tid_pipe[2];
    int exec_go[2];
    int ready_pipe[2];
    int finish_pipe[2];
    int rc = pipe(tid_pipe);
    CHECK_RET(rc, 0, "create TID publication pipe");
    if (rc != 0) {
        TEST_DONE();
    }
    rc = pipe(exec_go);
    CHECK_RET(rc, 0, "create exec release pipe");
    if (rc != 0) {
        TEST_DONE();
    }
    rc = pipe(ready_pipe);
    CHECK_RET(rc, 0, "create post-exec ready pipe");
    if (rc != 0) {
        TEST_DONE();
    }
    rc = pipe(finish_pipe);
    CHECK_RET(rc, 0, "create post-exec finish pipe");
    if (rc != 0) {
        TEST_DONE();
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork exec-transfer child");
    if (child == 0) {
        close(tid_pipe[0]);
        close(exec_go[1]);
        close(ready_pipe[0]);
        close(finish_pipe[1]);

        struct exec_context ctx = {
            .self = argv[0],
            .tid_write = tid_pipe[1],
            .exec_go_read = exec_go[0],
            .ready_write = ready_pipe[1],
            .finish_read = finish_pipe[0],
        };
        pthread_t thread;
        if (pthread_create(&thread, NULL, nonleader_exec, &ctx) != 0)
            _exit(61);
        for (;;)
            pause();
    }
    if (child < 0) {
        TEST_DONE();
    }

    close(tid_pipe[1]);
    close(exec_go[0]);
    close(ready_pipe[1]);
    close(finish_pipe[0]);

    pid_t exec_tid = 0;
    CHECK_RET(read(tid_pipe[0], &exec_tid, sizeof(exec_tid)), sizeof(exec_tid),
              "receive non-leader TID");
    close(tid_pipe[0]);
    CHECK(exec_tid > 0 && exec_tid != child, "exec thread has a distinct TID");

    int process_pidfd = x_pidfd_open(child, 0);
    int thread_pidfd = x_pidfd_open(exec_tid, PIDFD_THREAD);
    CHECK(process_pidfd >= 0, "open process pidfd before exec");
    CHECK(thread_pidfd >= 0, "open thread pidfd before exec");

    CHECK_RET(write(exec_go[1], "G", 1), 1, "release non-leader exec");
    close(exec_go[1]);
    char ready = 0;
    CHECK_RET(read(ready_pipe[0], &ready, 1), 1, "wait for post-exec image");
    close(ready_pipe[0]);
    CHECK(ready == 'R', "post-exec TGID is unchanged and gettid equals getpid");

    if (process_pidfd >= 0)
        CHECK_RET(x_pidfd_send_signal(process_pidfd, 0), 0,
                  "process pidfd follows the transferred leader identity");
    if (thread_pidfd >= 0)
        CHECK_ERR(x_pidfd_send_signal(thread_pidfd, 0), ESRCH,
                  "old thread pidfd is not redirected during exec");

    CHECK_RET(write(finish_pipe[1], "F", 1), 1, "release post-exec image");
    close(finish_pipe[1]);
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "reap exec-transfer child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "exec-transfer child exited normally");
    if (process_pidfd >= 0)
        close(process_pidfd);
    if (thread_pidfd >= 0)
        close(thread_pidfd);

    TEST_DONE();
}
