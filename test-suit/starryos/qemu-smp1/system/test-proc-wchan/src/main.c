#define _GNU_SOURCE

#include "../common/test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdatomic.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif

#ifndef FUTEX_WAKE
#define FUTEX_WAKE 1
#endif

#define WCHAN_BUF 128
#define WAIT_TIMEOUT_MS 3000
#define WCHAN_ID_FUTEX_WAIT 3ULL
#define WCHAN_ID_POLL_WAIT 4ULL
#define WCHAN_ID_SCHEDULE_TIMEOUT 6ULL
#define WCHAN_ID_DO_WAIT 7ULL
#define WCHAN_ID_FILE_LOCK_WAIT 8ULL

struct shared_state {
    _Atomic int phase;
    _Atomic int aux_pid;
    _Atomic uint32_t futex_word;
};

static long raw_futex(uint32_t *uaddr, int op, uint32_t val,
                      const struct timespec *timeout)
{
    errno = 0;
    return syscall(SYS_futex, uaddr, op, val, timeout, NULL, 0);
}

static long now_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

static void trim_newline(char *buf)
{
    for (char *p = buf; *p != '\0'; p++) {
        if (*p == '\n' || *p == '\r') {
            *p = '\0';
            return;
        }
    }
}

static int read_wchan(pid_t pid, char *buf, size_t len)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/wchan", pid);

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    ssize_t n = read(fd, buf, len - 1);
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    if (n < 0) {
        return -1;
    }

    buf[n] = '\0';
    trim_newline(buf);
    return 0;
}

static int read_stat_wchan(pid_t pid, unsigned long long *value)
{
    char path[64];
    char buf[1024];
    snprintf(path, sizeof(path), "/proc/%d/stat", pid);

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';

    char *end_comm = strrchr(buf, ')');
    if (end_comm == NULL || end_comm[1] != ' ') {
        errno = EINVAL;
        return -1;
    }

    int field = 3;
    char *cursor = end_comm + 2;
    while (*cursor != '\0') {
        while (*cursor == ' ') {
            cursor++;
        }
        if (*cursor == '\0') {
            break;
        }

        char *token = cursor;
        while (*cursor != '\0' && *cursor != ' ') {
            cursor++;
        }
        if (*cursor != '\0') {
            *cursor++ = '\0';
        }

        if (field == 35) {
            errno = 0;
            *value = strtoull(token, NULL, 10);
            return errno == 0 ? 0 : -1;
        }
        field++;
    }

    errno = EINVAL;
    return -1;
}

static int wait_wchan(pid_t pid, const char *expected, char *last, size_t len)
{
    long deadline = now_ms() + WAIT_TIMEOUT_MS;
    last[0] = '\0';

    while (now_ms() < deadline) {
        if (read_wchan(pid, last, len) == 0 && strcmp(last, expected) == 0) {
            return 0;
        }
        sched_yield();
    }

    return -1;
}

static int wait_stat_wchan(pid_t pid, unsigned long long expected,
                           unsigned long long *last)
{
    long deadline = now_ms() + WAIT_TIMEOUT_MS;
    *last = 0;

    while (now_ms() < deadline) {
        if (read_stat_wchan(pid, last) == 0 && *last == expected) {
            return 0;
        }
        sched_yield();
    }

    return -1;
}

static int wait_wchan_empty(pid_t pid, char *last, size_t len)
{
    long deadline = now_ms() + WAIT_TIMEOUT_MS;
    last[0] = '\0';

    while (now_ms() < deadline) {
        if (read_wchan(pid, last, len) == 0 && last[0] == '\0') {
            return 0;
        }
        sched_yield();
    }

    return -1;
}

static int wait_stat_wchan_zero(pid_t pid, unsigned long long *last)
{
    long deadline = now_ms() + WAIT_TIMEOUT_MS;
    *last = 0;

    while (now_ms() < deadline) {
        if (read_stat_wchan(pid, last) == 0 && *last == 0) {
            return 0;
        }
        sched_yield();
    }

    return -1;
}

static void wait_phase(struct shared_state *state, int phase)
{
    long deadline = now_ms() + WAIT_TIMEOUT_MS;
    while (now_ms() < deadline) {
        if (atomic_load_explicit(&state->phase, memory_order_acquire) == phase) {
            return;
        }
        sched_yield();
    }
    fprintf(stderr, "timeout waiting for phase %d\n", phase);
    exit(1);
}

static void wait_child_ok(pid_t pid, const char *msg)
{
    int status = 0;
    pid_t waited;

    do {
        waited = waitpid(pid, &status, 0);
    } while (waited == -1 && errno == EINTR);

    CHECK(waited == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0, msg);
}

static void child_spin_until_parent_observes(struct shared_state *state)
{
    atomic_store_explicit(&state->phase, 2, memory_order_release);
    wait_phase(state, 3);
}

static void test_futex_wchan(struct shared_state *state)
{
    char observed[WCHAN_BUF];
    unsigned long long stat_wchan = 0;
    atomic_store_explicit(&state->phase, 0, memory_order_release);
    atomic_store_explicit(&state->futex_word, 0, memory_order_release);

    pid_t pid = fork();
    if (pid == 0) {
        struct timespec timeout = {
            .tv_sec = 10,
            .tv_nsec = 0,
        };
        atomic_store_explicit(&state->phase, 1, memory_order_release);
        long ret = raw_futex((uint32_t *)&state->futex_word, FUTEX_WAIT, 0,
                             &timeout);
        if (ret != 0) {
            perror("futex wait");
            _exit(1);
        }
        child_spin_until_parent_observes(state);
        _exit(0);
    }

    CHECK(pid > 0, "fork futex waiter");
    wait_phase(state, 1);

    CHECK(wait_wchan(pid, "futex_wait", observed, sizeof(observed)) == 0,
          "futex waiter reports futex_wait");
    CHECK(wait_stat_wchan(pid, WCHAN_ID_FUTEX_WAIT, &stat_wchan) == 0,
          "futex waiter reports numeric stat wchan");
    if (observed[0] != '\0') {
        printf("  INFO | futex observed wchan: %s stat=%llu\n", observed,
               stat_wchan);
    }

    atomic_store_explicit(&state->futex_word, 1, memory_order_release);
    CHECK(raw_futex((uint32_t *)&state->futex_word, FUTEX_WAKE, 1, NULL) == 1,
          "wake futex waiter");

    wait_phase(state, 2);
    CHECK(wait_wchan_empty(pid, observed, sizeof(observed)) == 0,
          "futex waiter clears wchan after wake");
    CHECK(wait_stat_wchan_zero(pid, &stat_wchan) == 0,
          "futex waiter clears numeric stat wchan after wake");
    atomic_store_explicit(&state->phase, 3, memory_order_release);
    wait_child_ok(pid, "futex waiter exits cleanly");
}

static void test_poll_wchan(struct shared_state *state)
{
    char observed[WCHAN_BUF];
    unsigned long long stat_wchan = 0;
    int pipefd[2];
    CHECK(pipe(pipefd) == 0, "create pipe for poll waiter");

    atomic_store_explicit(&state->phase, 0, memory_order_release);
    pid_t pid = fork();
    if (pid == 0) {
        close(pipefd[1]);
        struct pollfd pfd = {
            .fd = pipefd[0],
            .events = POLLIN,
        };
        atomic_store_explicit(&state->phase, 1, memory_order_release);
        int ret = poll(&pfd, 1, 10000);
        if (ret != 1 || (pfd.revents & POLLIN) == 0) {
            perror("poll");
            _exit(1);
        }
        char byte;
        if (read(pipefd[0], &byte, 1) != 1) {
            perror("read pipe");
            _exit(1);
        }
        child_spin_until_parent_observes(state);
        _exit(0);
    }

    close(pipefd[0]);
    CHECK(pid > 0, "fork poll waiter");
    wait_phase(state, 1);

    CHECK(wait_wchan(pid, "poll_wait", observed, sizeof(observed)) == 0,
          "poll waiter reports poll_wait");
    CHECK(wait_stat_wchan(pid, WCHAN_ID_POLL_WAIT, &stat_wchan) == 0,
          "poll waiter reports numeric stat wchan");
    if (observed[0] != '\0') {
        printf("  INFO | poll observed wchan: %s stat=%llu\n", observed,
               stat_wchan);
    }

    CHECK(write(pipefd[1], "x", 1) == 1, "wake poll waiter");
    close(pipefd[1]);

    wait_phase(state, 2);
    CHECK(wait_wchan_empty(pid, observed, sizeof(observed)) == 0,
          "poll waiter clears wchan after wake");
    CHECK(wait_stat_wchan_zero(pid, &stat_wchan) == 0,
          "poll waiter clears numeric stat wchan after wake");
    atomic_store_explicit(&state->phase, 3, memory_order_release);
    wait_child_ok(pid, "poll waiter exits cleanly");
}

static void test_sleep_wchan(struct shared_state *state)
{
    char observed[WCHAN_BUF];
    unsigned long long stat_wchan = 0;
    atomic_store_explicit(&state->phase, 0, memory_order_release);

    pid_t pid = fork();
    if (pid == 0) {
        const struct timespec req = {
            .tv_sec = 2,
            .tv_nsec = 0,
        };
        atomic_store_explicit(&state->phase, 1, memory_order_release);
        nanosleep(&req, NULL);
        child_spin_until_parent_observes(state);
        _exit(0);
    }

    CHECK(pid > 0, "fork sleep waiter");
    wait_phase(state, 1);
    CHECK(wait_wchan(pid, "schedule_timeout", observed, sizeof(observed)) == 0,
          "sleep waiter reports schedule_timeout");
    CHECK(wait_stat_wchan(pid, WCHAN_ID_SCHEDULE_TIMEOUT, &stat_wchan) == 0,
          "sleep waiter reports numeric stat wchan");
    if (observed[0] != '\0') {
        printf("  INFO | sleep observed wchan: %s stat=%llu\n", observed,
               stat_wchan);
    }

    wait_phase(state, 2);
    CHECK(wait_wchan_empty(pid, observed, sizeof(observed)) == 0,
          "sleep waiter clears wchan after timeout");
    CHECK(wait_stat_wchan_zero(pid, &stat_wchan) == 0,
          "sleep waiter clears numeric stat wchan after timeout");
    atomic_store_explicit(&state->phase, 3, memory_order_release);
    wait_child_ok(pid, "sleep waiter exits cleanly");
}

static void set_write_lock(int fd, short cmd)
{
    struct flock fl = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 1,
    };
    if (fcntl(fd, cmd, &fl) != 0) {
        perror("fcntl write lock");
        _exit(1);
    }
}

static void unlock_file(int fd)
{
    struct flock fl = {
        .l_type = F_UNLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 1,
    };
    if (fcntl(fd, F_SETLK, &fl) != 0) {
        perror("fcntl unlock");
        _exit(1);
    }
}

static void test_file_lock_wchan(struct shared_state *state)
{
    const char *path = "/tmp/test-proc-wchan.lock";
    char observed[WCHAN_BUF];
    unsigned long long stat_wchan = 0;

    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
    CHECK(fd >= 0, "open lock test file");
    CHECK(write(fd, "x", 1) == 1, "seed lock test file");
    set_write_lock(fd, F_SETLK);

    atomic_store_explicit(&state->phase, 0, memory_order_release);
    pid_t pid = fork();
    if (pid == 0) {
        int child_fd = open(path, O_RDWR);
        if (child_fd < 0) {
            perror("open child lock file");
            _exit(1);
        }
        atomic_store_explicit(&state->phase, 1, memory_order_release);
        set_write_lock(child_fd, F_SETLKW);
        unlock_file(child_fd);
        close(child_fd);
        child_spin_until_parent_observes(state);
        _exit(0);
    }

    CHECK(pid > 0, "fork file-lock waiter");
    wait_phase(state, 1);
    CHECK(wait_wchan(pid, "file_lock_wait", observed, sizeof(observed)) == 0,
          "file-lock waiter reports file_lock_wait");
    CHECK(wait_stat_wchan(pid, WCHAN_ID_FILE_LOCK_WAIT, &stat_wchan) == 0,
          "file-lock waiter reports numeric stat wchan");
    if (observed[0] != '\0') {
        printf("  INFO | file-lock observed wchan: %s stat=%llu\n", observed,
               stat_wchan);
    }

    unlock_file(fd);
    close(fd);
    wait_phase(state, 2);
    CHECK(wait_wchan_empty(pid, observed, sizeof(observed)) == 0,
          "file-lock waiter clears wchan after wake");
    CHECK(wait_stat_wchan_zero(pid, &stat_wchan) == 0,
          "file-lock waiter clears numeric stat wchan after wake");
    atomic_store_explicit(&state->phase, 3, memory_order_release);
    wait_child_ok(pid, "file-lock waiter exits cleanly");
    unlink(path);
}

static void test_child_wait_wchan(struct shared_state *state)
{
    char observed[WCHAN_BUF];
    unsigned long long stat_wchan = 0;
    atomic_store_explicit(&state->phase, 0, memory_order_release);
    atomic_store_explicit(&state->aux_pid, 0, memory_order_release);

    pid_t waiter = fork();
    if (waiter == 0) {
        pid_t child = fork();
        if (child == 0) {
            for (;;) {
                pause();
            }
        }
        if (child < 0) {
            perror("fork wait child");
            _exit(1);
        }

        atomic_store_explicit(&state->aux_pid, child, memory_order_release);
        atomic_store_explicit(&state->phase, 1, memory_order_release);

        int status = 0;
        pid_t waited;
        do {
            waited = waitpid(child, &status, 0);
        } while (waited == -1 && errno == EINTR);
        if (waited != child) {
            perror("waitpid child");
            _exit(1);
        }

        child_spin_until_parent_observes(state);
        _exit(0);
    }

    CHECK(waiter > 0, "fork child-wait waiter");
    wait_phase(state, 1);
    CHECK(wait_wchan(waiter, "do_wait", observed, sizeof(observed)) == 0,
          "child waiter reports do_wait");
    CHECK(wait_stat_wchan(waiter, WCHAN_ID_DO_WAIT, &stat_wchan) == 0,
          "child waiter reports numeric stat wchan");
    if (observed[0] != '\0') {
        printf("  INFO | child-wait observed wchan: %s stat=%llu\n", observed,
               stat_wchan);
    }

    pid_t child = atomic_load_explicit(&state->aux_pid, memory_order_acquire);
    CHECK(child > 0 && kill(child, SIGTERM) == 0, "wake child waiter by exiting child");

    wait_phase(state, 2);
    CHECK(wait_wchan_empty(waiter, observed, sizeof(observed)) == 0,
          "child waiter clears wchan after child exit");
    CHECK(wait_stat_wchan_zero(waiter, &stat_wchan) == 0,
          "child waiter clears numeric stat wchan after child exit");
    atomic_store_explicit(&state->phase, 3, memory_order_release);
    wait_child_ok(waiter, "child waiter exits cleanly");
}

int main(void)
{
    TEST_START("/proc/<pid>/wchan");

    struct shared_state *state = mmap(NULL, sizeof(*state), PROT_READ | PROT_WRITE,
                                      MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(state != MAP_FAILED, "create shared state");
    if (state == MAP_FAILED) {
        TEST_DONE();
    }

    test_futex_wchan(state);
    test_poll_wchan(state);
    test_sleep_wchan(state);
    test_file_lock_wchan(state);
    test_child_wait_wchan(state);

    munmap(state, sizeof(*state));
    TEST_DONE();
}
