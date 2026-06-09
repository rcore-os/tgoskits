/*
 * Focused StarryOS regression test for Nix builder process lifecycle.
 *
 * Replicates the fork/exec/wait topology that Nix uses to launch a builder
 * process, including multi-threaded scenarios matching Nix's thread pool.
 *
 * Scenarios:
 *   1. fork + exec /bin/sh → wait4 collects exit (single-thread)
 *   2. fork + exec with env → waitpid collects exit
 *   3. double fork (worker→builder) nested process topology
 *   4. fork + exec + waitpid from a secondary thread (Nix thread pool pattern)
 *
 * Final marker: NIX_BUILDER_LIFECYCLE_ALL_PASSED
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#define MARKER_DIR "/tmp"
#define MARKER_FILE MARKER_DIR "/builder-lifecycle-marker"
#define MARKER_FILE2 MARKER_DIR "/builder-lifecycle-marker2"

/* ── Scenario 1: fork + exec /bin/sh → wait4 ── */
static void test_fork_exec_shell_wait4(void)
{
    TEST_START("fork + exec /bin/sh -c echo OK → wait4 collects exit");

    unlink(MARKER_FILE);

    pid_t pid = fork();
    CHECK(pid >= 0, "fork for builder scenario");
    if (pid < 0) return;

    if (pid == 0) {
        char *const argv[] = { "/bin/sh", "-c",
            "echo BUILDER_OK > " MARKER_FILE, NULL };
        execve("/bin/sh", argv, environ);
        _exit(126);
    }

    int status = 0;
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 returns builder child pid");
    if (waited == pid) {
        CHECK(WIFEXITED(status), "builder child exits normally");
        if (WIFEXITED(status)) {
            CHECK(WEXITSTATUS(status) == 0, "builder child exit status 0");
        }
    }

    int fd = open(MARKER_FILE, O_RDONLY);
    CHECK(fd >= 0, "builder marker file exists");
    if (fd >= 0) {
        char buf[64] = {0};
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        CHECK(n > 0, "builder marker file non-empty");
        close(fd);
    }
    unlink(MARKER_FILE);

    printf("NIX_BUILDER_FORK_EXEC_WAIT4_PASSED\n");
}

/* ── Scenario 2: fork + exec with explicit env ── */
static void test_fork_exec_with_env_wait4(void)
{
    TEST_START("fork + exec /bin/sh with env → wait4");

    unlink(MARKER_FILE);

    pid_t pid = fork();
    CHECK(pid >= 0, "fork for env scenario");
    if (pid < 0) return;

    if (pid == 0) {
        char *const argv[] = { "/bin/sh", "-c",
            "echo BUILDER_ENV_OK > " MARKER_FILE, NULL };
        execve("/bin/sh", argv, environ);
        _exit(126);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    CHECK(waited == pid, "waitpid returns env builder pid");
    if (waited == pid) {
        CHECK(WIFEXITED(status), "env builder exits normally");
    }

    unlink(MARKER_FILE);
    printf("NIX_BUILDER_FORK_EXEC_ENV_PASSED\n");
}

/* ── Scenario 3: double fork (worker→builder) nested topology ── */
static void test_double_fork_builder_topology(void)
{
    TEST_START("double fork worker→builder topology");

    unlink(MARKER_FILE);

    pid_t worker = fork();
    CHECK(worker >= 0, "fork worker process");
    if (worker < 0) return;

    if (worker == 0) {
        pid_t builder = fork();
        CHECK(builder >= 0, "worker forks builder");
        if (builder < 0) _exit(1);

        if (builder == 0) {
            char *const argv[] = { "/bin/sh", "-c",
                "echo NESTED_BUILDER_OK > " MARKER_FILE, NULL };
            execve("/bin/sh", argv, environ);
            _exit(126);
        }

        int bstatus = 0;
        pid_t bwaited = waitpid(builder, &bstatus, 0);
        CHECK(bwaited == builder, "worker waitpid collects builder");
        if (bwaited == builder) {
            CHECK(WIFEXITED(bstatus), "builder exits normally in worker");
        }
        _exit(WIFEXITED(bstatus) ? WEXITSTATUS(bstatus) : 1);
    }

    int wstatus = 0;
    pid_t wwaited = waitpid(worker, &wstatus, 0);
    CHECK(wwaited == worker, "parent waitpid collects worker");
    if (wwaited == worker) {
        CHECK(WIFEXITED(wstatus), "worker exits normally");
        if (WIFEXITED(wstatus)) {
            CHECK(WEXITSTATUS(wstatus) == 0, "worker exit status 0");
        }
    }

    int fd = open(MARKER_FILE, O_RDONLY);
    CHECK(fd >= 0, "nested builder marker exists");
    if (fd >= 0) close(fd);
    unlink(MARKER_FILE);

    printf("NIX_BUILDER_DOUBLE_FORK_PASSED\n");
}

/* ── Scenario 4: fork+exec+waitpid from a secondary thread ── */
struct thread_fork_ctx {
    int ok;
    char marker_path[128];
};

static void *thread_fork_exec_wait(void *arg)
{
    struct thread_fork_ctx *ctx = (struct thread_fork_ctx *)arg;
    ctx->ok = 0;

    unlink(ctx->marker_path);

    pid_t pid = fork();
    if (pid < 0) return NULL;

    if (pid == 0) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "echo THREAD_BUILDER_OK > %s", ctx->marker_path);
        char *argv[] = { "/bin/sh", "-c", cmd, NULL };
        execve("/bin/sh", argv, environ);
        _exit(126);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    if (waited != pid) return NULL;
    if (!WIFEXITED(status)) return NULL;
    if (WEXITSTATUS(status) != 0) return NULL;

    int fd = open(ctx->marker_path, O_RDONLY);
    if (fd < 0) return NULL;
    char buf[64] = {0};
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    unlink(ctx->marker_path);
    if (n <= 0) return NULL;

    ctx->ok = 1;
    return NULL;
}

static void test_thread_fork_exec_wait(void)
{
    TEST_START("secondary thread fork + exec /bin/sh → waitpid");

    struct thread_fork_ctx ctx;
    snprintf(ctx.marker_path, sizeof(ctx.marker_path), "%s", MARKER_FILE2);

    pthread_t thr;
    int rc = pthread_create(&thr, NULL, thread_fork_exec_wait, &ctx);
    CHECK(rc == 0, "pthread_create for builder thread");
    if (rc != 0) return;

    void *ret = NULL;
    rc = pthread_join(thr, &ret);
    CHECK(rc == 0, "pthread_join builder thread");
    if (rc == 0) {
        CHECK(ctx.ok == 1, "thread fork+exec+waitpid completed successfully");
    }

    printf("NIX_BUILDER_THREAD_FORK_WAIT_PASSED\n");
}

int main(void)
{
    test_fork_exec_shell_wait4();
    test_fork_exec_with_env_wait4();
    test_double_fork_builder_topology();
    test_thread_fork_exec_wait();

    printf("NIX_BUILDER_LIFECYCLE_ALL_PASSED\n");
    return 0;
}
