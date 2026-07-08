/*
 * test-cargo-jobserver-wait: cargo-like process coordination stress.
 *
 * The AArch64/HVF 8-core StarryOS self-build reached the late cargo tail and
 * then showed only the cargo parent sleeping, with no rustc/build-script child
 * processes left in ps output. This test is a short feedback loop for the OS
 * pieces cargo depends on at that point: pipe readiness, eventfd wakeups,
 * poll/epoll readiness, waitpid(-1, WNOHANG) reaping, and child exit status
 * propagation.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/file.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHILDREN 16
#define BUILD_SCRIPT_WAVE 4
#define EXEC_CHILDREN 8
#define PTHREAD_WORKERS 2
#define PTHREAD_SPAWNS_PER_WORKER 4
#define SPAWN_CHILDREN 8
#define JOB_TOKENS 4
#define EVENTFD_WORKERS 2
#define EVENTFD_WRITES_PER_WORKER 8
#define EVENTFD_TOTAL_WRITES (EVENTFD_WORKERS * EVENTFD_WRITES_PER_WORKER)
#define EVENTFD_ET_REWRITES 3
#define ACCOUNTING_WORKERS 2
#define ACCOUNTING_JOBS 8
#define LOCK_WORKERS 2
#define LOCK_ITERATIONS 4
#define POLL_TIMEOUT_MS 50
#define MAX_LOOPS 200

extern char **environ;

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("  PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("  FAIL: %s: %s\n", name, detail);
    failed++;
}

static int write_byte_retry(int fd, char byte)
{
    for (int i = 0; i < 2000; i++) {
        ssize_t n = write(fd, &byte, 1);
        if (n == 1) {
            return 0;
        }
        if (n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) {
            sched_yield();
            continue;
        }
        return -1;
    }
    errno = EAGAIN;
    return -1;
}

static int write_eventfd_retry(int fd, uint64_t value)
{
    for (int i = 0; i < 2000; i++) {
        ssize_t n = write(fd, &value, sizeof(value));
        if (n == (ssize_t)sizeof(value)) {
            return 0;
        }
        if (n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) {
            sched_yield();
            continue;
        }
        if (n >= 0) {
            errno = EIO;
        }
        return -1;
    }
    errno = EAGAIN;
    return -1;
}

static void yield_briefly(void)
{
    struct timespec ts = {
        .tv_sec = 0,
        .tv_nsec = 1000 * 1000,
    };

    sched_yield();
    (void)nanosleep(&ts, NULL);
}

static int drain_pipe(int fd, int *bytes, int *saw_eof)
{
    char buf[64];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n > 0) {
            *bytes += (int)n;
            continue;
        }
        if (n == 0) {
            *saw_eof = 1;
            return 0;
        }
        if (errno == EINTR) {
            continue;
        }
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            return 0;
        }
        return -1;
    }
}

static int drain_eventfd(int fd, uint64_t *total)
{
    for (;;) {
        uint64_t value = 0;
        ssize_t n = read(fd, &value, sizeof(value));
        if (n == (ssize_t)sizeof(value)) {
            *total += value;
            continue;
        }
        if (n < 0 && errno == EINTR) {
            continue;
        }
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            return 0;
        }
        if (n >= 0) {
            errno = EIO;
        }
        return -1;
    }
}

static int reap_available(int *reaped, int expect_status_mod)
{
    for (;;) {
        int status = 0;
        pid_t pid = waitpid(-1, &status, WNOHANG);
        if (pid > 0) {
            if (!WIFEXITED(status)) {
                printf("    child %ld did not exit normally: status=%d\n", (long)pid, status);
                return -1;
            }
            if (expect_status_mod > 0 && WEXITSTATUS(status) >= expect_status_mod) {
                printf("    child %ld exit status out of range: %d\n",
                       (long)pid, WEXITSTATUS(status));
                return -1;
            }
            (*reaped)++;
            continue;
        }
        if (pid == 0) {
            return 0;
        }
        if (errno == EINTR) {
            continue;
        }
        if (errno == ECHILD) {
            return 0;
        }
        return -1;
    }
}

static int read_counter_file(int fd, int *value)
{
    char buf[64];
    ssize_t n;

    memset(buf, 0, sizeof(buf));
    n = pread(fd, buf, sizeof(buf) - 1, 0);
    if (n < 0) {
        return -1;
    }
    if (n == 0) {
        *value = 0;
        return 0;
    }
    *value = atoi(buf);
    return 0;
}

static int write_counter_file(int fd, int value)
{
    char buf[64];
    int len = snprintf(buf, sizeof(buf), "%d\n", value);

    if (len < 0 || len >= (int)sizeof(buf)) {
        errno = EINVAL;
        return -1;
    }
    if (ftruncate(fd, 0) != 0) {
        return -1;
    }
    if (pwrite(fd, buf, (size_t)len, 0) != len) {
        if (errno == 0) {
            errno = EIO;
        }
        return -1;
    }
    return 0;
}

static int prepare_counter_file(const char *path)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (fd < 0) {
        return -1;
    }
    if (write_counter_file(fd, 0) != 0) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return -1;
    }
    return fd;
}

static int wait_for_lock_children(int expected)
{
    int reaped = 0;
    for (int loops = 0; reaped < expected && loops < MAX_LOOPS; loops++) {
        if (reap_available(&reaped, 81) != 0) {
            return -1;
        }
        if (reaped < expected) {
            yield_briefly();
        }
    }
    if (reaped != expected) {
        errno = ETIMEDOUT;
        return -1;
    }
    return 0;
}

static int counter_matches(const char *path, int expected, char *detail, size_t detail_len)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        snprintf(detail, detail_len, "open final counter failed errno=%d", errno);
        return 0;
    }

    int got = -1;
    int rc = read_counter_file(fd, &got);
    int saved_errno = errno;
    close(fd);
    if (rc != 0) {
        snprintf(detail, detail_len, "read final counter failed errno=%d", saved_errno);
        return 0;
    }

    snprintf(detail, detail_len, "counter=%d/%d", got, expected);
    return got == expected;
}

static int lock_wave_child_flock(const char *path)
{
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        _exit(80);
    }

    for (int i = 0; i < LOCK_ITERATIONS; i++) {
        int value = 0;
        if (flock(fd, LOCK_EX) != 0) {
            _exit(81);
        }
        if (read_counter_file(fd, &value) != 0) {
            _exit(82);
        }
        sched_yield();
        if (write_counter_file(fd, value + 1) != 0) {
            _exit(83);
        }
        if (flock(fd, LOCK_UN) != 0) {
            _exit(84);
        }
        sched_yield();
    }

    close(fd);
    _exit(0);
}

static int set_fcntl_lock(int fd, short type, int wait)
{
    struct flock fl = {
        .l_type = type,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 0,
    };
    return fcntl(fd, wait ? F_SETLKW : F_SETLK, &fl);
}

static int lock_wave_child_fcntl(const char *path)
{
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        _exit(80);
    }

    for (int i = 0; i < LOCK_ITERATIONS; i++) {
        int value = 0;
        if (set_fcntl_lock(fd, F_WRLCK, 1) != 0) {
            _exit(81);
        }
        if (read_counter_file(fd, &value) != 0) {
            _exit(82);
        }
        sched_yield();
        if (write_counter_file(fd, value + 1) != 0) {
            _exit(83);
        }
        if (set_fcntl_lock(fd, F_UNLCK, 0) != 0) {
            _exit(84);
        }
        sched_yield();
    }

    close(fd);
    _exit(0);
}

static void run_lock_counter_wave(const char *name, const char *path,
                                  int (*child_fn)(const char *))
{
    int fd = prepare_counter_file(path);
    if (fd < 0) {
        note_fail(name, strerror(errno));
        return;
    }
    close(fd);

    for (int i = 0; i < LOCK_WORKERS; i++) {
        pid_t pid = fork();
        if (pid < 0) {
            note_fail(name, strerror(errno));
            return;
        }
        if (pid == 0) {
            child_fn(path);
        }
    }

    char detail[160];
    if (wait_for_lock_children(LOCK_WORKERS) != 0) {
        snprintf(detail, sizeof(detail), "wait lock children errno=%d", errno);
        note_fail(name, detail);
        return;
    }

    const int expected = LOCK_WORKERS * LOCK_ITERATIONS;
    if (counter_matches(path, expected, detail, sizeof(detail))) {
        char pass_detail[192];
        snprintf(pass_detail, sizeof(pass_detail), "%s workers=%d iterations=%d %s",
                 name, LOCK_WORKERS, LOCK_ITERATIONS, detail);
        note_pass(pass_detail);
    } else {
        note_fail(name, detail);
    }
}

struct eventfd_worker_state {
    int event_fd;
    int pipe_fd;
    int worker_id;
    int writes;
    int error;
};

static void *eventfd_pipe_worker(void *arg)
{
    struct eventfd_worker_state *state = arg;
    char byte = (char)('a' + (state->worker_id % 26));

    for (int i = 0; i < state->writes; i++) {
        if (write_eventfd_retry(state->event_fd, 1) != 0) {
            state->error = errno;
            break;
        }
        if (write_byte_retry(state->pipe_fd, byte) != 0) {
            state->error = errno;
            break;
        }
        if ((i % 4) == 0) {
            sched_yield();
        }
    }

    return NULL;
}

static int join_eventfd_workers(pthread_t *threads, struct eventfd_worker_state *states)
{
    int first_error = 0;
    for (int i = 0; i < EVENTFD_WORKERS; i++) {
        pthread_join(threads[i], NULL);
        if (states[i].error != 0 && first_error == 0) {
            first_error = states[i].error;
        }
    }
    if (first_error != 0) {
        errno = first_error;
        return -1;
    }
    return 0;
}

static int start_eventfd_workers(pthread_t *threads, struct eventfd_worker_state *states,
                                 int event_fd, int pipe_fd)
{
    for (int i = 0; i < EVENTFD_WORKERS; i++) {
        states[i] = (struct eventfd_worker_state) {
            .event_fd = event_fd,
            .pipe_fd = pipe_fd,
            .worker_id = i,
            .writes = EVENTFD_WRITES_PER_WORKER,
            .error = 0,
        };
        int rc = pthread_create(&threads[i], NULL, eventfd_pipe_worker, &states[i]);
        if (rc != 0) {
            errno = rc;
            for (int j = 0; j < i; j++) {
                pthread_join(threads[j], NULL);
            }
            return -1;
        }
    }
    return 0;
}

static void test_pipe_poll_wait_tail(void)
{
    printf("[phase] pipe poll + waitpid tail\n");

    int pipefd[2];
    if (pipe2(pipefd, O_CLOEXEC | O_NONBLOCK) != 0) {
        note_fail("pipe poll wait setup", strerror(errno));
        return;
    }

    for (int i = 0; i < CHILDREN; i++) {
        pid_t pid = fork();
        if (pid < 0) {
            note_fail("fork pipe writers", strerror(errno));
            close(pipefd[0]);
            close(pipefd[1]);
            return;
        }
        if (pid == 0) {
            close(pipefd[0]);
            if (write_byte_retry(pipefd[1], (char)('a' + (i % 26))) != 0) {
                _exit(80);
            }
            close(pipefd[1]);
            _exit(i % 64);
        }
    }

    close(pipefd[1]);

    int bytes = 0;
    int reaped = 0;
    int saw_eof = 0;
    int loops = 0;

    while ((bytes < CHILDREN || reaped < CHILDREN || !saw_eof) && loops++ < MAX_LOOPS) {
        if (bytes >= CHILDREN && saw_eof) {
            /*
             * The data path is complete.  Polling an EOF pipe can return
             * immediately forever, so yield while the remaining children
             * finish exiting and become waitable.
             */
            yield_briefly();
            if (reap_available(&reaped, 81) != 0) {
                note_fail("waitpid pipe writers", strerror(errno));
                break;
            }
            continue;
        }

        struct pollfd pfd = {
            .fd = pipefd[0],
            .events = POLLIN | POLLHUP,
            .revents = 0,
        };

        int pr = poll(&pfd, 1, POLL_TIMEOUT_MS);
        if (pr < 0) {
            if (errno == EINTR) {
                continue;
            }
            note_fail("poll pipe writers", strerror(errno));
            break;
        }
        if (pr > 0) {
            if (pfd.revents & POLLERR) {
                note_fail("poll pipe writers", "POLLERR");
                break;
            }
            if (pfd.revents & (POLLIN | POLLHUP)) {
                if (drain_pipe(pipefd[0], &bytes, &saw_eof) != 0) {
                    note_fail("drain pipe writers", strerror(errno));
                    break;
                }
            }
        }
        if (reap_available(&reaped, 81) != 0) {
            note_fail("waitpid pipe writers", strerror(errno));
            break;
        }
    }

    close(pipefd[0]);

    char detail[160];
    if (bytes == CHILDREN && reaped == CHILDREN && saw_eof) {
        snprintf(detail, sizeof(detail), "bytes=%d reaped=%d eof=%d loops=%d",
                 bytes, reaped, saw_eof, loops);
        note_pass(detail);
    } else {
        snprintf(detail, sizeof(detail), "bytes=%d/%d reaped=%d/%d eof=%d loops=%d",
                 bytes, CHILDREN, reaped, CHILDREN, saw_eof, loops);
        note_fail("pipe poll wait tail completion", detail);
    }
}

static void test_jobserver_token_reuse(void)
{
    printf("[phase] jobserver token pipe + waitpid\n");

    int token[2];
    if (pipe2(token, O_CLOEXEC | O_NONBLOCK) != 0) {
        note_fail("jobserver pipe setup", strerror(errno));
        return;
    }

    for (int i = 0; i < JOB_TOKENS; i++) {
        if (write_byte_retry(token[1], '+') != 0) {
            note_fail("jobserver initial token write", strerror(errno));
            close(token[0]);
            close(token[1]);
            return;
        }
    }

    int launched = 0;
    int reaped = 0;
    int loops = 0;

    while ((launched < CHILDREN || reaped < CHILDREN) && loops++ < MAX_LOOPS) {
        if (reap_available(&reaped, 65) != 0) {
            note_fail("jobserver waitpid", strerror(errno));
            break;
        }

        while (launched < CHILDREN) {
            struct pollfd pfd = {
                .fd = token[0],
                .events = POLLIN,
                .revents = 0,
            };
            int pr = poll(&pfd, 1, 0);
            if (pr < 0) {
                if (errno == EINTR) {
                    continue;
                }
                note_fail("jobserver token poll", strerror(errno));
                close(token[0]);
                close(token[1]);
                return;
            }
            if (pr == 0 || !(pfd.revents & POLLIN)) {
                break;
            }

            char byte;
            ssize_t n = read(token[0], &byte, 1);
            if (n != 1) {
                if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) {
                    break;
                }
                note_fail("jobserver token read", n == 0 ? "unexpected EOF" : strerror(errno));
                close(token[0]);
                close(token[1]);
                return;
            }

            pid_t pid = fork();
            if (pid < 0) {
                note_fail("jobserver fork", strerror(errno));
                close(token[0]);
                close(token[1]);
                return;
            }
            if (pid == 0) {
                close(token[0]);
                for (int spin = 0; spin < 8 + (launched % 5); spin++) {
                    sched_yield();
                }
                if (write_byte_retry(token[1], '+') != 0) {
                    _exit(80);
                }
                close(token[1]);
                _exit(launched % 64);
            }
            launched++;
        }

        if (launched >= CHILDREN) {
            /*
             * All work has been issued.  The token pipe may remain readable
             * because completed children returned their tokens; polling it
             * again would turn the parent into a tight WNOHANG loop and could
             * starve the last child on cooperative or timer-sensitive kernels.
             */
            yield_briefly();
        } else if (reaped < CHILDREN) {
            struct pollfd pfd = {
                .fd = token[0],
                .events = POLLIN,
                .revents = 0,
            };
            int pr = poll(&pfd, 1, POLL_TIMEOUT_MS);
            if (pr < 0 && errno != EINTR) {
                note_fail("jobserver blocking poll", strerror(errno));
                break;
            }
        }
    }

    close(token[0]);
    close(token[1]);

    char detail[160];
    if (launched == CHILDREN && reaped == CHILDREN) {
        snprintf(detail, sizeof(detail), "launched=%d reaped=%d loops=%d",
                 launched, reaped, loops);
        note_pass(detail);
    } else {
        snprintf(detail, sizeof(detail), "launched=%d/%d reaped=%d/%d loops=%d",
                 launched, CHILDREN, reaped, CHILDREN, loops);
        note_fail("jobserver token reuse completion", detail);
    }
}

static int run_true_child(int err_fd)
{
    char *const true_argv[] = { "true", NULL };
    char *const busybox_argv[] = { "true", NULL };
    int saved_errno;

    execv("/bin/true", true_argv);
    saved_errno = errno;
    execv("/usr/bin/true", true_argv);
    saved_errno = errno;
    execv("/bin/busybox", busybox_argv);
    saved_errno = errno;

    (void)write(err_fd, &saved_errno, sizeof(saved_errno));
    _exit(127);
}

static int wait_child_nohang(pid_t pid, int *status)
{
    for (int loops = 0; loops < MAX_LOOPS; loops++) {
        pid_t got = waitpid(pid, status, WNOHANG);
        if (got == pid) {
            return 0;
        }
        if (got < 0 && errno != EINTR) {
            return -1;
        }
        yield_briefly();
    }
    errno = EAGAIN;
    return -1;
}

static void test_exec_cloexec_error_pipe(void)
{
    printf("[phase] fork exec + CLOEXEC error pipe\n");

    int ok = 0;
    for (int i = 0; i < EXEC_CHILDREN; i++) {
        int err_pipe[2];
        if (pipe2(err_pipe, O_CLOEXEC | O_NONBLOCK) != 0) {
            note_fail("exec error pipe setup", strerror(errno));
            return;
        }

        pid_t pid = fork();
        if (pid < 0) {
            note_fail("exec fork", strerror(errno));
            close(err_pipe[0]);
            close(err_pipe[1]);
            return;
        }

        if (pid == 0) {
            close(err_pipe[0]);
            run_true_child(err_pipe[1]);
        }

        close(err_pipe[1]);

        int saw_eof = 0;
        int saw_error = 0;
        int loops = 0;
        while (!saw_eof && loops++ < MAX_LOOPS) {
            struct pollfd pfd = {
                .fd = err_pipe[0],
                .events = POLLIN | POLLHUP,
                .revents = 0,
            };

            int pr = poll(&pfd, 1, POLL_TIMEOUT_MS);
            if (pr < 0) {
                if (errno == EINTR) {
                    continue;
                }
                note_fail("exec error pipe poll", strerror(errno));
                close(err_pipe[0]);
                return;
            }
            if (pr == 0) {
                continue;
            }
            if (pfd.revents & POLLERR) {
                note_fail("exec error pipe poll", "POLLERR");
                close(err_pipe[0]);
                return;
            }
            if (pfd.revents & (POLLIN | POLLHUP)) {
                int child_errno = 0;
                ssize_t n = read(err_pipe[0], &child_errno, sizeof(child_errno));
                if (n == 0) {
                    saw_eof = 1;
                    break;
                }
                if (n > 0) {
                    char detail[128];
                    snprintf(detail, sizeof(detail), "child reported exec errno=%d", child_errno);
                    note_fail("exec error pipe child", detail);
                    saw_error = 1;
                    break;
                }
                if (errno != EINTR && errno != EAGAIN && errno != EWOULDBLOCK) {
                    note_fail("exec error pipe read", strerror(errno));
                    close(err_pipe[0]);
                    return;
                }
            }
        }
        close(err_pipe[0]);

        int status = 0;
        if (wait_child_nohang(pid, &status) != 0) {
            note_fail("exec waitpid", strerror(errno));
            return;
        }
        if (saw_error || !saw_eof) {
            note_fail("exec CLOEXEC EOF", saw_error ? "exec failure reported" : "missing EOF");
            return;
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            char detail[128];
            snprintf(detail, sizeof(detail), "status=%d", status);
            note_fail("exec child status", detail);
            return;
        }
        ok++;
    }

    char detail[96];
    snprintf(detail, sizeof(detail), "exec_children=%d", ok);
    note_pass(detail);
}

static int spawn_true(pid_t *pid)
{
    char *const true_argv[] = { "true", NULL };
    char *const busybox_argv[] = { "true", NULL };
    int rc;

    rc = posix_spawn(pid, "/bin/true", NULL, NULL, true_argv, environ);
    if (rc == 0) {
        return 0;
    }
    rc = posix_spawn(pid, "/usr/bin/true", NULL, NULL, true_argv, environ);
    if (rc == 0) {
        return 0;
    }
    return posix_spawn(pid, "/bin/busybox", NULL, NULL, busybox_argv, environ);
}

static void test_posix_spawn_wait(void)
{
    printf("[phase] posix_spawn + waitpid\n");

    int ok = 0;
    for (int i = 0; i < SPAWN_CHILDREN; i++) {
        pid_t pid = -1;
        int rc = spawn_true(&pid);
        if (rc != 0) {
            errno = rc;
            note_fail("posix_spawn true", strerror(errno));
            return;
        }

        int status = 0;
        if (wait_child_nohang(pid, &status) != 0) {
            note_fail("posix_spawn waitpid", strerror(errno));
            return;
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            char detail[128];
            snprintf(detail, sizeof(detail), "status=%d", status);
            note_fail("posix_spawn child status", detail);
            return;
        }
        ok++;
    }

    char detail[96];
    snprintf(detail, sizeof(detail), "spawn_children=%d", ok);
    note_pass(detail);
}

static int spawn_shell_with_pipes(pid_t *pid, int stdout_fd, int stderr_fd, int index)
{
    posix_spawn_file_actions_t actions;
    int rc = posix_spawn_file_actions_init(&actions);
    if (rc != 0) {
        return rc;
    }

    rc = posix_spawn_file_actions_adddup2(&actions, stdout_fd, STDOUT_FILENO);
    if (rc == 0) {
        rc = posix_spawn_file_actions_adddup2(&actions, stderr_fd, STDERR_FILENO);
    }
    if (rc == 0 && stdout_fd != STDOUT_FILENO) {
        rc = posix_spawn_file_actions_addclose(&actions, stdout_fd);
    }
    if (rc == 0 && stderr_fd != STDERR_FILENO) {
        rc = posix_spawn_file_actions_addclose(&actions, stderr_fd);
    }
    if (rc != 0) {
        posix_spawn_file_actions_destroy(&actions);
        return rc;
    }

    char script[160];
    snprintf(script, sizeof(script),
             "echo build-script-stdout-%d; echo build-script-stderr-%d 1>&2",
             index, index);
    char *const argv[] = { "sh", "-c", script, NULL };
    rc = posix_spawn(pid, "/bin/sh", &actions, NULL, argv, environ);
    posix_spawn_file_actions_destroy(&actions);
    return rc;
}

static void test_posix_spawn_output_pipes(void)
{
    printf("[phase] posix_spawn stdout/stderr pipes\n");

    int ok = 0;
    for (int i = 0; i < SPAWN_CHILDREN; i++) {
        int out_pipe[2];
        int err_pipe[2];
        if (pipe2(out_pipe, O_CLOEXEC | O_NONBLOCK) != 0) {
            note_fail("spawn stdout pipe setup", strerror(errno));
            return;
        }
        if (pipe2(err_pipe, O_CLOEXEC | O_NONBLOCK) != 0) {
            note_fail("spawn stderr pipe setup", strerror(errno));
            close(out_pipe[0]);
            close(out_pipe[1]);
            return;
        }

        pid_t pid = -1;
        int rc = spawn_shell_with_pipes(&pid, out_pipe[1], err_pipe[1], i);
        close(out_pipe[1]);
        close(err_pipe[1]);
        if (rc != 0) {
            errno = rc;
            note_fail("posix_spawn shell", strerror(errno));
            close(out_pipe[0]);
            close(err_pipe[0]);
            return;
        }

        int out_bytes = 0;
        int err_bytes = 0;
        int out_eof = 0;
        int err_eof = 0;
        int loops = 0;
        while ((!out_eof || !err_eof) && loops++ < MAX_LOOPS) {
            struct pollfd pfds[2] = {
                {
                    .fd = out_eof ? -1 : out_pipe[0],
                    .events = POLLIN | POLLHUP,
                    .revents = 0,
                },
                {
                    .fd = err_eof ? -1 : err_pipe[0],
                    .events = POLLIN | POLLHUP,
                    .revents = 0,
                },
            };
            int pr = poll(pfds, 2, POLL_TIMEOUT_MS);
            if (pr < 0) {
                if (errno == EINTR) {
                    continue;
                }
                note_fail("spawn output poll", strerror(errno));
                close(out_pipe[0]);
                close(err_pipe[0]);
                return;
            }
            if (pr == 0) {
                continue;
            }
            if ((pfds[0].revents | pfds[1].revents) & POLLERR) {
                note_fail("spawn output poll", "POLLERR");
                close(out_pipe[0]);
                close(err_pipe[0]);
                return;
            }
            if (!out_eof && (pfds[0].revents & (POLLIN | POLLHUP))) {
                if (drain_pipe(out_pipe[0], &out_bytes, &out_eof) != 0) {
                    note_fail("spawn stdout drain", strerror(errno));
                    close(out_pipe[0]);
                    close(err_pipe[0]);
                    return;
                }
            }
            if (!err_eof && (pfds[1].revents & (POLLIN | POLLHUP))) {
                if (drain_pipe(err_pipe[0], &err_bytes, &err_eof) != 0) {
                    note_fail("spawn stderr drain", strerror(errno));
                    close(out_pipe[0]);
                    close(err_pipe[0]);
                    return;
                }
            }
        }
        close(out_pipe[0]);
        close(err_pipe[0]);

        int status = 0;
        if (wait_child_nohang(pid, &status) != 0) {
            note_fail("spawn output waitpid", strerror(errno));
            return;
        }
        if (!out_eof || !err_eof || out_bytes == 0 || err_bytes == 0) {
            char detail[160];
            snprintf(detail, sizeof(detail),
                     "out_eof=%d err_eof=%d out_bytes=%d err_bytes=%d loops=%d",
                     out_eof, err_eof, out_bytes, err_bytes, loops);
            note_fail("spawn output pipe completion", detail);
            return;
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            char detail[128];
            snprintf(detail, sizeof(detail), "status=%d", status);
            note_fail("spawn output child status", detail);
            return;
        }
        ok++;
    }

    char detail[96];
    snprintf(detail, sizeof(detail), "spawn_output_children=%d", ok);
    note_pass(detail);
}

struct build_script_child {
    pid_t pid;
    int out_fd;
    int err_fd;
    int out_eof;
    int err_eof;
    int out_bytes;
    int err_bytes;
};

static void close_build_script_child(struct build_script_child *child)
{
    if (child->out_fd >= 0) {
        close(child->out_fd);
        child->out_fd = -1;
    }
    if (child->err_fd >= 0) {
        close(child->err_fd);
        child->err_fd = -1;
    }
}

static void test_build_script_wave(void)
{
    printf("[phase] concurrent build-script stdout/stderr wave\n");

    struct build_script_child children[BUILD_SCRIPT_WAVE];
    memset(children, 0, sizeof(children));
    for (int i = 0; i < BUILD_SCRIPT_WAVE; i++) {
        children[i].pid = -1;
        children[i].out_fd = -1;
        children[i].err_fd = -1;
    }

    for (int i = 0; i < BUILD_SCRIPT_WAVE; i++) {
        int out_pipe[2];
        int err_pipe[2];
        if (pipe2(out_pipe, O_CLOEXEC | O_NONBLOCK) != 0) {
            note_fail("build-script stdout pipe setup", strerror(errno));
            goto cleanup;
        }
        if (pipe2(err_pipe, O_CLOEXEC | O_NONBLOCK) != 0) {
            note_fail("build-script stderr pipe setup", strerror(errno));
            close(out_pipe[0]);
            close(out_pipe[1]);
            goto cleanup;
        }

        pid_t pid = -1;
        int rc = spawn_shell_with_pipes(&pid, out_pipe[1], err_pipe[1], i);
        close(out_pipe[1]);
        close(err_pipe[1]);
        if (rc != 0) {
            errno = rc;
            note_fail("build-script posix_spawn", strerror(errno));
            close(out_pipe[0]);
            close(err_pipe[0]);
            goto cleanup;
        }

        children[i].pid = pid;
        children[i].out_fd = out_pipe[0];
        children[i].err_fd = err_pipe[0];
    }

    int reaped = 0;
    int loops = 0;
    while (reaped < BUILD_SCRIPT_WAVE && loops++ < MAX_LOOPS) {
        struct pollfd pfds[BUILD_SCRIPT_WAVE * 2];
        int child_index[BUILD_SCRIPT_WAVE * 2];
        int is_stderr[BUILD_SCRIPT_WAVE * 2];
        int nfds = 0;

        for (int i = 0; i < BUILD_SCRIPT_WAVE; i++) {
            if (!children[i].out_eof) {
                child_index[nfds] = i;
                is_stderr[nfds] = 0;
                pfds[nfds++] = (struct pollfd) {
                    .fd = children[i].out_fd,
                    .events = POLLIN | POLLHUP,
                    .revents = 0,
                };
            }
            if (!children[i].err_eof) {
                child_index[nfds] = i;
                is_stderr[nfds] = 1;
                pfds[nfds++] = (struct pollfd) {
                    .fd = children[i].err_fd,
                    .events = POLLIN | POLLHUP,
                    .revents = 0,
                };
            }
        }

        if (nfds == 0) {
            yield_briefly();
        } else {
            int pr = poll(pfds, (nfds_t)nfds, POLL_TIMEOUT_MS);
            if (pr < 0) {
                if (errno == EINTR) {
                    continue;
                }
                note_fail("build-script output poll", strerror(errno));
                goto cleanup;
            }

            for (int i = 0; i < nfds; i++) {
                if (pfds[i].revents == 0) {
                    continue;
                }
                if (pfds[i].revents & POLLERR) {
                    note_fail("build-script output poll", "POLLERR");
                    goto cleanup;
                }
                if (pfds[i].revents & (POLLIN | POLLHUP)) {
                    struct build_script_child *child = &children[child_index[i]];
                    int *bytes = is_stderr[i] ? &child->err_bytes : &child->out_bytes;
                    int *eof = is_stderr[i] ? &child->err_eof : &child->out_eof;
                    if (drain_pipe(pfds[i].fd, bytes, eof) != 0) {
                        note_fail("build-script output drain", strerror(errno));
                        goto cleanup;
                    }
                }
            }
        }

        if (reap_available(&reaped, 1) != 0) {
            note_fail("build-script wave waitpid", strerror(errno));
            goto cleanup;
        }
    }

    int all_output = 1;
    for (int i = 0; i < BUILD_SCRIPT_WAVE; i++) {
        if (!children[i].out_eof || !children[i].err_eof || children[i].out_bytes == 0 ||
            children[i].err_bytes == 0) {
            all_output = 0;
            break;
        }
    }

    if (reaped == BUILD_SCRIPT_WAVE && all_output) {
        char detail[128];
        snprintf(detail, sizeof(detail), "scripts=%d reaped=%d loops=%d",
                 BUILD_SCRIPT_WAVE, reaped, loops);
        note_pass(detail);
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail), "scripts=%d reaped=%d output_ok=%d loops=%d",
                 BUILD_SCRIPT_WAVE, reaped, all_output, loops);
        note_fail("build-script wave completion", detail);
    }

cleanup:
    for (int i = 0; i < BUILD_SCRIPT_WAVE; i++) {
        close_build_script_child(&children[i]);
    }
}

struct pthread_reaper_state {
    pthread_mutex_t lock;
    pthread_cond_t cond;
    int done;
    int error;
};

static void *pthread_spawn_worker(void *arg)
{
    struct pthread_reaper_state *state = arg;

    for (int i = 0; i < PTHREAD_SPAWNS_PER_WORKER; i++) {
        int local_error = 0;
        pid_t pid = -1;
        int rc = spawn_true(&pid);
        if (rc != 0) {
            local_error = rc;
        } else {
            int status = 0;
            if (wait_child_nohang(pid, &status) != 0) {
                local_error = errno;
            } else if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
                local_error = ECHILD;
            }
        }

        pthread_mutex_lock(&state->lock);
        if (local_error != 0 && state->error == 0) {
            state->error = local_error;
        }
        state->done++;
        pthread_cond_signal(&state->cond);
        pthread_mutex_unlock(&state->lock);

        if (local_error != 0) {
            break;
        }
    }

    return NULL;
}

static void test_pthread_spawn_reaper(void)
{
    printf("[phase] pthread posix_spawn reaper condvar\n");

    struct pthread_reaper_state state = {
        .lock = PTHREAD_MUTEX_INITIALIZER,
        .cond = PTHREAD_COND_INITIALIZER,
        .done = 0,
        .error = 0,
    };
    pthread_t threads[PTHREAD_WORKERS];

    for (int i = 0; i < PTHREAD_WORKERS; i++) {
        int rc = pthread_create(&threads[i], NULL, pthread_spawn_worker, &state);
        if (rc != 0) {
            errno = rc;
            note_fail("pthread_create spawn worker", strerror(errno));
            return;
        }
    }

    const int total = PTHREAD_WORKERS * PTHREAD_SPAWNS_PER_WORKER;
    int loops = 0;
    pthread_mutex_lock(&state.lock);
    while (state.done < total && state.error == 0 && loops++ < MAX_LOOPS) {
        struct timespec ts;
        if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
            ts.tv_sec = time(NULL);
            ts.tv_nsec = 0;
        }
        ts.tv_nsec += 20 * 1000 * 1000;
        if (ts.tv_nsec >= 1000 * 1000 * 1000) {
            ts.tv_sec++;
            ts.tv_nsec -= 1000 * 1000 * 1000;
        }
        int rc = pthread_cond_timedwait(&state.cond, &state.lock, &ts);
        if (rc != 0 && rc != ETIMEDOUT) {
            state.error = rc;
            break;
        }
    }
    int done = state.done;
    int error = state.error;
    pthread_mutex_unlock(&state.lock);

    for (int i = 0; i < PTHREAD_WORKERS; i++) {
        pthread_join(threads[i], NULL);
    }

    if (error != 0) {
        errno = error;
        note_fail("pthread spawn worker", strerror(errno));
        return;
    }
    if (done != total) {
        char detail[128];
        snprintf(detail, sizeof(detail), "done=%d/%d loops=%d", done, total, loops);
        note_fail("pthread reaper completion", detail);
        return;
    }

    char detail[96];
    snprintf(detail, sizeof(detail), "threaded_spawns=%d loops=%d", done, loops);
    note_pass(detail);
}

static void test_eventfd_poll_pipe_wave(void)
{
    printf("[phase] eventfd + pipe poll worker wave\n");

    int event_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (event_fd < 0) {
        note_fail("eventfd poll setup", strerror(errno));
        return;
    }

    int pipefd[2];
    if (pipe2(pipefd, O_CLOEXEC | O_NONBLOCK) != 0) {
        note_fail("eventfd poll pipe setup", strerror(errno));
        close(event_fd);
        return;
    }

    pthread_t threads[EVENTFD_WORKERS];
    struct eventfd_worker_state states[EVENTFD_WORKERS];
    if (start_eventfd_workers(threads, states, event_fd, pipefd[1]) != 0) {
        note_fail("eventfd poll pthread_create", strerror(errno));
        close(pipefd[0]);
        close(pipefd[1]);
        close(event_fd);
        return;
    }

    uint64_t event_total = 0;
    int pipe_bytes = 0;
    int pipe_eof = 0;
    int loops = 0;
    while ((event_total < EVENTFD_TOTAL_WRITES || pipe_bytes < EVENTFD_TOTAL_WRITES) &&
           loops++ < MAX_LOOPS) {
        struct pollfd pfds[2] = {
            {
                .fd = event_fd,
                .events = POLLIN,
                .revents = 0,
            },
            {
                .fd = pipefd[0],
                .events = POLLIN,
                .revents = 0,
            },
        };

        int pr = poll(pfds, 2, POLL_TIMEOUT_MS);
        if (pr < 0) {
            if (errno == EINTR) {
                continue;
            }
            note_fail("eventfd poll wait", strerror(errno));
            break;
        }
        if (pr == 0) {
            yield_briefly();
            continue;
        }
        if ((pfds[0].revents | pfds[1].revents) & POLLERR) {
            note_fail("eventfd poll wait", "POLLERR");
            break;
        }
        if (pfds[0].revents & POLLIN) {
            if (drain_eventfd(event_fd, &event_total) != 0) {
                note_fail("eventfd poll drain eventfd", strerror(errno));
                break;
            }
        }
        if (pfds[1].revents & POLLIN) {
            if (drain_pipe(pipefd[0], &pipe_bytes, &pipe_eof) != 0) {
                note_fail("eventfd poll drain pipe", strerror(errno));
                break;
            }
        }
    }

    int worker_rc = join_eventfd_workers(threads, states);
    if (worker_rc != 0) {
        note_fail("eventfd poll worker", strerror(errno));
    }

    close(pipefd[0]);
    close(pipefd[1]);
    close(event_fd);

    char detail[160];
    if (worker_rc == 0 && event_total == EVENTFD_TOTAL_WRITES &&
        pipe_bytes == EVENTFD_TOTAL_WRITES) {
        snprintf(detail, sizeof(detail), "event_total=%llu pipe_bytes=%d loops=%d",
                 (unsigned long long)event_total, pipe_bytes, loops);
        note_pass(detail);
    } else {
        snprintf(detail, sizeof(detail), "event_total=%llu/%d pipe_bytes=%d/%d loops=%d",
                 (unsigned long long)event_total, EVENTFD_TOTAL_WRITES, pipe_bytes,
                 EVENTFD_TOTAL_WRITES, loops);
        note_fail("eventfd poll worker wave completion", detail);
    }
}

static void test_eventfd_epoll_rewake_no_read(void)
{
    printf("[phase] eventfd epoll ET rewake without read\n");

    int event_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (event_fd < 0) {
        note_fail("eventfd epoll setup", strerror(errno));
        return;
    }

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        note_fail("eventfd epoll create", strerror(errno));
        close(event_fd);
        return;
    }

    struct epoll_event interest = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = event_fd,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, event_fd, &interest) != 0) {
        note_fail("eventfd epoll ctl add", strerror(errno));
        close(epfd);
        close(event_fd);
        return;
    }

    int events_seen = 0;
    for (int i = 0; i < EVENTFD_ET_REWRITES; i++) {
        if (write_eventfd_retry(event_fd, 1) != 0) {
            note_fail("eventfd epoll write", strerror(errno));
            break;
        }
        struct epoll_event event = { 0 };
        int n = epoll_wait(epfd, &event, 1, POLL_TIMEOUT_MS);
        if (n < 0) {
            if (errno == EINTR) {
                i--;
                continue;
            }
            note_fail("eventfd epoll wait", strerror(errno));
            break;
        }
        if (n != 1 || event.data.fd != event_fd || !(event.events & EPOLLIN)) {
            char detail[160];
            snprintf(detail, sizeof(detail), "iter=%d n=%d fd=%d events=0x%x",
                     i, n, n == 1 ? event.data.fd : -1, n == 1 ? event.events : 0);
            note_fail("eventfd epoll rewake event", detail);
            break;
        }
        events_seen++;
    }

    uint64_t total = 0;
    if (drain_eventfd(event_fd, &total) != 0) {
        note_fail("eventfd epoll drain", strerror(errno));
    }

    close(epfd);
    close(event_fd);

    char detail[160];
    if (events_seen == EVENTFD_ET_REWRITES && total == EVENTFD_ET_REWRITES) {
        snprintf(detail, sizeof(detail), "rewrites=%d total=%llu",
                 events_seen, (unsigned long long)total);
        note_pass(detail);
    } else {
        snprintf(detail, sizeof(detail), "events_seen=%d/%d total=%llu/%d",
                 events_seen, EVENTFD_ET_REWRITES, (unsigned long long)total,
                 EVENTFD_ET_REWRITES);
        note_fail("eventfd epoll ET rewake completion", detail);
    }
}

static void test_eventfd_epoll_pipe_wave(void)
{
    printf("[phase] eventfd + pipe epoll worker wave\n");

    int event_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (event_fd < 0) {
        note_fail("eventfd epoll wave setup", strerror(errno));
        return;
    }

    int pipefd[2];
    if (pipe2(pipefd, O_CLOEXEC | O_NONBLOCK) != 0) {
        note_fail("eventfd epoll wave pipe setup", strerror(errno));
        close(event_fd);
        return;
    }

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        note_fail("eventfd epoll wave create", strerror(errno));
        close(pipefd[0]);
        close(pipefd[1]);
        close(event_fd);
        return;
    }

    struct epoll_event event_interest = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = event_fd,
    };
    struct epoll_event pipe_interest = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = pipefd[0],
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, event_fd, &event_interest) != 0 ||
        epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &pipe_interest) != 0) {
        note_fail("eventfd epoll wave ctl add", strerror(errno));
        close(epfd);
        close(pipefd[0]);
        close(pipefd[1]);
        close(event_fd);
        return;
    }

    pthread_t threads[EVENTFD_WORKERS];
    struct eventfd_worker_state states[EVENTFD_WORKERS];
    if (start_eventfd_workers(threads, states, event_fd, pipefd[1]) != 0) {
        note_fail("eventfd epoll wave pthread_create", strerror(errno));
        close(epfd);
        close(pipefd[0]);
        close(pipefd[1]);
        close(event_fd);
        return;
    }

    uint64_t event_total = 0;
    int pipe_bytes = 0;
    int pipe_eof = 0;
    int loops = 0;
    while ((event_total < EVENTFD_TOTAL_WRITES || pipe_bytes < EVENTFD_TOTAL_WRITES) &&
           loops++ < MAX_LOOPS) {
        struct epoll_event events[8];
        int n = epoll_wait(epfd, events, 8, POLL_TIMEOUT_MS);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            note_fail("eventfd epoll wave wait", strerror(errno));
            break;
        }
        if (n == 0) {
            yield_briefly();
            continue;
        }
        for (int i = 0; i < n; i++) {
            if (events[i].events & EPOLLERR) {
                note_fail("eventfd epoll wave wait", "EPOLLERR");
                break;
            }
            if (events[i].data.fd == event_fd && (events[i].events & EPOLLIN)) {
                if (drain_eventfd(event_fd, &event_total) != 0) {
                    note_fail("eventfd epoll wave drain eventfd", strerror(errno));
                    break;
                }
            } else if (events[i].data.fd == pipefd[0] && (events[i].events & EPOLLIN)) {
                if (drain_pipe(pipefd[0], &pipe_bytes, &pipe_eof) != 0) {
                    note_fail("eventfd epoll wave drain pipe", strerror(errno));
                    break;
                }
            }
        }
    }

    int worker_rc = join_eventfd_workers(threads, states);
    if (worker_rc != 0) {
        note_fail("eventfd epoll wave worker", strerror(errno));
    }

    close(epfd);
    close(pipefd[0]);
    close(pipefd[1]);
    close(event_fd);

    char detail[160];
    if (worker_rc == 0 && event_total == EVENTFD_TOTAL_WRITES &&
        pipe_bytes == EVENTFD_TOTAL_WRITES) {
        snprintf(detail, sizeof(detail), "event_total=%llu pipe_bytes=%d loops=%d",
                 (unsigned long long)event_total, pipe_bytes, loops);
        note_pass(detail);
    } else {
        snprintf(detail, sizeof(detail), "event_total=%llu/%d pipe_bytes=%d/%d loops=%d",
                 (unsigned long long)event_total, EVENTFD_TOTAL_WRITES, pipe_bytes,
                 EVENTFD_TOTAL_WRITES, loops);
        note_fail("eventfd epoll worker wave completion", detail);
    }
}

struct cargo_accounting_state {
    pthread_mutex_t lock;
    pthread_cond_t cond;
    int event_fd;
    int next_job;
    int spawned;
    int reaped;
    int error;
    int waiter_wakeups;
};

static void cargo_accounting_set_error(struct cargo_accounting_state *state, int error)
{
    pthread_mutex_lock(&state->lock);
    if (state->error == 0) {
        state->error = error;
    }
    pthread_cond_broadcast(&state->cond);
    pthread_mutex_unlock(&state->lock);
}

static void *cargo_accounting_worker(void *arg)
{
    struct cargo_accounting_state *state = arg;

    for (;;) {
        pthread_mutex_lock(&state->lock);
        if (state->next_job >= ACCOUNTING_JOBS || state->error != 0) {
            pthread_mutex_unlock(&state->lock);
            break;
        }
        state->next_job++;
        pthread_mutex_unlock(&state->lock);

        pid_t pid = -1;
        int rc = spawn_true(&pid);
        if (rc != 0) {
            cargo_accounting_set_error(state, rc);
            break;
        }

        pthread_mutex_lock(&state->lock);
        state->spawned++;
        pthread_cond_broadcast(&state->cond);
        pthread_mutex_unlock(&state->lock);

        if (write_eventfd_retry(state->event_fd, 1) != 0) {
            cargo_accounting_set_error(state, errno);
            break;
        }

        sched_yield();
    }

    return NULL;
}

static void *cargo_accounting_waiter(void *arg)
{
    struct cargo_accounting_state *state = arg;

    pthread_mutex_lock(&state->lock);
    while (state->reaped < ACCOUNTING_JOBS && state->error == 0) {
        struct timespec ts;
        if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
            ts.tv_sec = time(NULL);
            ts.tv_nsec = 0;
        }
        ts.tv_nsec += 20 * 1000 * 1000;
        if (ts.tv_nsec >= 1000 * 1000 * 1000) {
            ts.tv_sec++;
            ts.tv_nsec -= 1000 * 1000 * 1000;
        }
        int rc = pthread_cond_timedwait(&state->cond, &state->lock, &ts);
        if (rc != 0 && rc != ETIMEDOUT) {
            if (state->error == 0) {
                state->error = rc;
            }
            break;
        }
        state->waiter_wakeups++;
    }
    pthread_mutex_unlock(&state->lock);
    return NULL;
}

static int cargo_accounting_reap_once(struct cargo_accounting_state *state)
{
    int made_progress = 0;

    for (;;) {
        int status = 0;
        pid_t pid = waitpid(-1, &status, WNOHANG);
        if (pid > 0) {
            if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
                cargo_accounting_set_error(state, ECHILD);
                return -1;
            }
            pthread_mutex_lock(&state->lock);
            state->reaped++;
            pthread_cond_broadcast(&state->cond);
            pthread_mutex_unlock(&state->lock);
            made_progress = 1;
            continue;
        }
        if (pid == 0) {
            return made_progress;
        }
        if (errno == EINTR) {
            continue;
        }
        if (errno == ECHILD) {
            return made_progress;
        }
        cargo_accounting_set_error(state, errno);
        return -1;
    }
}

static void test_cargo_worker_accounting_reaper(void)
{
    printf("[phase] cargo worker accounting + eventfd reaper + condvar\n");

    int event_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (event_fd < 0) {
        note_fail("cargo accounting eventfd setup", strerror(errno));
        return;
    }

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        note_fail("cargo accounting epoll create", strerror(errno));
        close(event_fd);
        return;
    }

    struct epoll_event interest = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = event_fd,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, event_fd, &interest) != 0) {
        note_fail("cargo accounting epoll ctl", strerror(errno));
        close(epfd);
        close(event_fd);
        return;
    }

    struct cargo_accounting_state state = {
        .lock = PTHREAD_MUTEX_INITIALIZER,
        .cond = PTHREAD_COND_INITIALIZER,
        .event_fd = event_fd,
        .next_job = 0,
        .spawned = 0,
        .reaped = 0,
        .error = 0,
        .waiter_wakeups = 0,
    };

    pthread_t workers[ACCOUNTING_WORKERS];
    pthread_t waiter;
    int worker_started = 0;
    int waiter_started = 0;
    for (int i = 0; i < ACCOUNTING_WORKERS; i++) {
        int rc = pthread_create(&workers[i], NULL, cargo_accounting_worker, &state);
        if (rc != 0) {
            errno = rc;
            note_fail("cargo accounting worker create", strerror(errno));
            cargo_accounting_set_error(&state, rc);
            goto join_workers;
        }
        worker_started++;
    }

    int rc = pthread_create(&waiter, NULL, cargo_accounting_waiter, &state);
    if (rc != 0) {
        errno = rc;
        note_fail("cargo accounting waiter create", strerror(errno));
        cargo_accounting_set_error(&state, rc);
        goto join_workers;
    }
    waiter_started = 1;

    uint64_t notifications = 0;
    int loops = 0;
    while (loops++ < MAX_LOOPS) {
        if (cargo_accounting_reap_once(&state) < 0) {
            break;
        }

        pthread_mutex_lock(&state.lock);
        int reaped = state.reaped;
        int error = state.error;
        pthread_mutex_unlock(&state.lock);
        if (reaped >= ACCOUNTING_JOBS || error != 0) {
            break;
        }

        struct epoll_event event = { 0 };
        int n = epoll_wait(epfd, &event, 1, POLL_TIMEOUT_MS);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            cargo_accounting_set_error(&state, errno);
            break;
        }
        if (n == 1 && event.data.fd == event_fd && (event.events & EPOLLIN)) {
            if (drain_eventfd(event_fd, &notifications) != 0) {
                cargo_accounting_set_error(&state, errno);
                break;
            }
        }
        yield_briefly();
    }

    for (int drain = 0; drain < MAX_LOOPS; drain++) {
        pthread_mutex_lock(&state.lock);
        int reaped = state.reaped;
        int spawned = state.spawned;
        int error = state.error;
        pthread_mutex_unlock(&state.lock);
        if (reaped >= ACCOUNTING_JOBS || error != 0) {
            break;
        }
        if (cargo_accounting_reap_once(&state) < 0) {
            break;
        }
        if (reaped == spawned) {
            yield_briefly();
        }
    }

    pthread_mutex_lock(&state.lock);
    if (state.reaped < ACCOUNTING_JOBS && state.error == 0) {
        state.error = ETIMEDOUT;
    }
    pthread_cond_broadcast(&state.cond);
    pthread_mutex_unlock(&state.lock);
    if (waiter_started) {
        pthread_join(waiter, NULL);
    }

join_workers:
    for (int i = 0; i < worker_started; i++) {
        pthread_join(workers[i], NULL);
    }

    (void)cargo_accounting_reap_once(&state);
    (void)drain_eventfd(event_fd, &notifications);

    pthread_mutex_lock(&state.lock);
    int spawned = state.spawned;
    int reaped = state.reaped;
    int error = state.error;
    int waiter_wakeups = state.waiter_wakeups;
    pthread_mutex_unlock(&state.lock);

    close(epfd);
    close(event_fd);

    char detail[192];
    if (error == 0 && spawned == ACCOUNTING_JOBS && reaped == ACCOUNTING_JOBS &&
        notifications >= (uint64_t)ACCOUNTING_JOBS) {
        snprintf(detail, sizeof(detail),
                 "spawned=%d reaped=%d notifications=%llu waiter_wakeups=%d loops=%d",
                 spawned, reaped, (unsigned long long)notifications, waiter_wakeups, loops);
        note_pass(detail);
    } else {
        snprintf(detail, sizeof(detail),
                 "spawned=%d/%d reaped=%d/%d notifications=%llu error=%d waiter_wakeups=%d loops=%d",
                 spawned, ACCOUNTING_JOBS, reaped, ACCOUNTING_JOBS,
                 (unsigned long long)notifications, error, waiter_wakeups, loops);
        note_fail("cargo worker accounting completion", detail);
    }
}

static void test_flock_cache_lock_wave(void)
{
    printf("[phase] cargo cache flock exclusive wave\n");

    run_lock_counter_wave("cargo cache flock exclusive wave",
                          "/tmp/test-cargo-jobserver-wait.flock", lock_wave_child_flock);
}

static void test_fcntl_cache_lock_wave(void)
{
    printf("[phase] cargo cache fcntl F_SETLKW wave\n");

    run_lock_counter_wave("cargo cache fcntl F_SETLKW wave",
                          "/tmp/test-cargo-jobserver-wait.fcntl", lock_wave_child_fcntl);
}

int main(void)
{
    printf("=== test-cargo-jobserver-wait ===\n");

    signal(SIGPIPE, SIG_IGN);
    test_pipe_poll_wait_tail();
    test_jobserver_token_reuse();
    test_exec_cloexec_error_pipe();
    test_posix_spawn_wait();
    test_posix_spawn_output_pipes();
    test_build_script_wave();
    test_pthread_spawn_reaper();
    test_eventfd_poll_pipe_wave();
    test_eventfd_epoll_rewake_no_read();
    test_eventfd_epoll_pipe_wave();
    test_cargo_worker_accounting_reaper();
    test_flock_cache_lock_wave();
    test_fcntl_cache_lock_wave();

    printf("DONE: %d pass, %d fail\n", passed, failed);
    return failed == 0 ? 0 : 1;
}
