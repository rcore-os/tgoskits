#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/times.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h>"
#endif

#ifndef P_PIDFD
#define P_PIDFD 3
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static void expect_waitpid_echild(pid_t pid, const char *msg)
{
    int status = 0;
    errno = 0;
    pid_t waited = waitpid(pid, &status, WNOHANG);
    CHECK(waited == -1 && errno == ECHILD, msg);
}

static void expect_sigchld_exit(const siginfo_t *info, pid_t pid, int status,
                                const char *msg)
{
    CHECK(info->si_pid == pid, msg);
    CHECK(info->si_code == CLD_EXITED, "waitid reports CLD_EXITED");
    CHECK(info->si_status == status, "waitid reports child exit status");
}

static pid_t fork_exit_child(int status)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork child");
    if (pid == 0) {
        _exit(status);
    }
    return pid;
}

static void test_waitid_pidfd_reaps_child(void)
{
    printf("--- waitid(P_PIDFD) reaps exited child ---\n");

    pid_t child = fork_exit_child(42);
    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(child) succeeds");
    if (pfd < 0) {
        (void)waitpid(child, NULL, 0);
        return;
    }

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED), 0,
              "waitid(P_PIDFD, WEXITED) succeeds");
    expect_sigchld_exit(&info, child, 42, "waitid reports the pidfd child");
    expect_waitpid_echild(child, "pidfd waitid consumes the child zombie");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_waitid_pidfd_wnowait_keeps_child_waitable(void)
{
    printf("--- waitid(P_PIDFD) WNOWAIT keeps zombie waitable ---\n");

    pid_t child = fork_exit_child(7);
    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(child) succeeds");
    if (pfd < 0) {
        (void)waitpid(child, NULL, 0);
        return;
    }

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOWAIT), 0,
              "waitid(P_PIDFD, WNOWAIT) observes child");
    expect_sigchld_exit(&info, child, 7, "WNOWAIT reports the pidfd child");

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child,
              "waitpid can still reap after WNOWAIT");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 7,
          "waitpid sees original exit status");

    memset(&info, 0, sizeof(info));
    CHECK_ERR(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOHANG), ECHILD,
              "pidfd waitid after reap returns ECHILD");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_waitid_pidfd_nohang_alive_child(void)
{
    printf("--- waitid(P_PIDFD) WNOHANG for live child ---\n");

    int pipefd[2];
    CHECK_RET(pipe(pipefd), 0, "create child sync pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork blocking child");
    if (child == 0) {
        close(pipefd[1]);
        char byte = 0;
        (void)read(pipefd[0], &byte, 1);
        close(pipefd[0]);
        _exit(5);
    }

    close(pipefd[0]);
    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(live child) succeeds");
    if (pfd < 0) {
        close(pipefd[1]);
        (void)waitpid(child, NULL, 0);
        return;
    }

    siginfo_t info;
    memset(&info, 0xff, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOHANG), 0,
              "waitid(P_PIDFD, WNOHANG) succeeds for live child");
    CHECK(info.si_pid == 0, "WNOHANG clears siginfo when child is not waitable");

    CHECK_RET(write(pipefd[1], "x", 1), 1, "release child");
    close(pipefd[1]);

    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED), 0,
              "waitid(P_PIDFD) reaps released child");
    expect_sigchld_exit(&info, child, 5, "waitid reports released child");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

struct delayed_leader_exit {
    int leader_exiting_fd;
    int release_worker_fd;
};

static void *exit_after_leader(void *arg)
{
    struct delayed_leader_exit *state = arg;
    char byte = 0;

    if (write(state->leader_exiting_fd, "x", 1) != 1) {
        syscall(SYS_exit_group, 90);
    }
    if (read(state->release_worker_fd, &byte, 1) != 1) {
        syscall(SYS_exit_group, 91);
    }
    syscall(SYS_exit_group, 23);
    return NULL;
}

static void test_pidfd_delays_leader_exit_until_last_thread(void)
{
    printf("--- pidfd stays unreadable while an exited leader has a live thread ---\n");

    int leader_exiting[2];
    int release_worker[2];
    CHECK_RET(pipe(leader_exiting), 0, "create leader-exit pipe");
    CHECK_RET(pipe(release_worker), 0, "create worker-release pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork multithreaded child");
    if (child == 0) {
        close(leader_exiting[0]);
        close(release_worker[1]);

        struct delayed_leader_exit state = {
            .leader_exiting_fd = leader_exiting[1],
            .release_worker_fd = release_worker[0],
        };
        pthread_t worker;
        if (pthread_create(&worker, NULL, exit_after_leader, &state) != 0) {
            syscall(SYS_exit_group, 92);
        }

        char byte = 0;
        if (read(release_worker[0], &byte, 0) != 0) {
            syscall(SYS_exit_group, 93);
        }
        syscall(SYS_exit, 11);
        syscall(SYS_exit_group, 94);
    }

    close(leader_exiting[1]);
    close(release_worker[0]);

    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(multithreaded child) succeeds");
    if (pfd < 0) {
        close(leader_exiting[0]);
        close(release_worker[1]);
        (void)waitpid(child, NULL, 0);
        return;
    }

    char byte = 0;
    CHECK_RET(read(leader_exiting[0], &byte, 1), 1,
              "worker confirms the leader is exiting");
    close(leader_exiting[0]);

    struct pollfd pollfd = {
        .fd = pfd,
        .events = POLLIN | POLLRDNORM | POLLHUP,
    };
    CHECK_RET(poll(&pollfd, 1, 300), 0,
              "pidfd is not ready while another thread remains alive");

    siginfo_t info;
    memset(&info, 0xff, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOHANG), 0,
              "waitid(P_PIDFD, WNOHANG) succeeds before the last thread exits");
    CHECK(info.si_pid == 0,
          "an exited leader is not waitable while another thread remains alive");

    CHECK_RET(write(release_worker[1], "x", 1), 1, "release the last thread");
    close(release_worker[1]);

    pollfd.revents = 0;
    CHECK_RET(poll(&pollfd, 1, 2000), 1,
              "pidfd becomes ready after the last thread exits");
    CHECK((pollfd.revents & (POLLIN | POLLRDNORM)) != 0,
          "last-thread exit publishes readable pidfd events");

    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED), 0,
              "waitid reaps the process after its last thread exits");
    expect_sigchld_exit(&info, child, 23,
                        "waitid reports the thread-group exit status");
    CHECK_RET(close(pfd), 0, "close multithreaded-child pidfd");
}

static int consume_cpu_ticks(clock_t minimum_ticks)
{
    struct tms start;
    struct tms now;
    struct timespec deadline;

    if (times(&start) == (clock_t)-1 ||
        clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) {
        return -1;
    }
    deadline.tv_sec += 5;

    volatile unsigned long accumulator = 0;
    for (;;) {
        for (unsigned long i = 0; i < 100000; ++i) {
            accumulator += i;
        }
        if (times(&now) == (clock_t)-1) {
            return -1;
        }
        if ((now.tms_utime + now.tms_stime) -
                (start.tms_utime + start.tms_stime) >=
            minimum_ticks) {
            return accumulator == 0 ? -1 : 0;
        }

        struct timespec current;
        if (clock_gettime(CLOCK_MONOTONIC, &current) != 0) {
            return -1;
        }
        if (current.tv_sec > deadline.tv_sec ||
            (current.tv_sec == deadline.tv_sec &&
             current.tv_nsec >= deadline.tv_nsec)) {
            return -1;
        }
    }
}

static void test_wait_accumulates_reaped_child_cpu_time(void)
{
    printf("--- wait accumulates the reaped child's CPU time exactly once ---\n");

    struct tms before;
    struct tms after;
    CHECK(times(&before) != (clock_t)-1, "read parent CPU accounting before fork");

    pid_t child = fork();
    CHECK(child >= 0, "fork CPU-accounting child");
    if (child == 0) {
        _exit(consume_cpu_ticks(2) == 0 ? 0 : 95);
    }
    if (child < 0) {
        return;
    }

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "reap CPU-accounting child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child consumed measurable CPU time");
    CHECK(times(&after) != (clock_t)-1, "read parent CPU accounting after wait");

    clock_t before_children = before.tms_cutime + before.tms_cstime;
    clock_t after_children = after.tms_cutime + after.tms_cstime;
    CHECK(after_children > before_children,
          "wait credits the child's frozen CPU time to its parent");
}

struct concurrent_wait {
    pthread_barrier_t *barrier;
    pid_t child;
    pid_t result;
    int error;
    int status;
};

static void *wait_for_same_child(void *arg)
{
    struct concurrent_wait *wait = arg;
    int barrier_result = pthread_barrier_wait(wait->barrier);

    if (barrier_result != 0 && barrier_result != PTHREAD_BARRIER_SERIAL_THREAD) {
        wait->result = -2;
        return NULL;
    }
    errno = 0;
    wait->result = waitpid(wait->child, &wait->status, 0);
    wait->error = errno;
    return NULL;
}

static void test_concurrent_wait_reaps_exactly_once(void)
{
    printf("--- concurrent waiters claim exactly one zombie reap ---\n");

    pid_t child = fork_exit_child(37);
    pthread_barrier_t barrier;
    CHECK_RET(pthread_barrier_init(&barrier, NULL, 2), 0,
              "initialize concurrent-wait barrier");

    struct concurrent_wait first = {
        .barrier = &barrier,
        .child = child,
    };
    struct concurrent_wait second = {
        .barrier = &barrier,
        .child = child,
    };
    pthread_t first_thread;
    pthread_t second_thread;
    CHECK_RET(pthread_create(&first_thread, NULL, wait_for_same_child, &first),
              0, "create first waiter");
    CHECK_RET(pthread_create(&second_thread, NULL, wait_for_same_child, &second),
              0, "create second waiter");
    CHECK_RET(pthread_join(first_thread, NULL), 0, "join first waiter");
    CHECK_RET(pthread_join(second_thread, NULL), 0, "join second waiter");
    CHECK_RET(pthread_barrier_destroy(&barrier), 0,
              "destroy concurrent-wait barrier");

    int reaped = 0;
    int already_reaped = 0;
    struct concurrent_wait *waiters[] = {&first, &second};
    for (size_t i = 0; i < 2; ++i) {
        if (waiters[i]->result == child) {
            ++reaped;
            CHECK(WIFEXITED(waiters[i]->status) &&
                      WEXITSTATUS(waiters[i]->status) == 37,
                  "winning waiter observes the frozen exit status");
        } else if (waiters[i]->result == -1 && waiters[i]->error == ECHILD) {
            ++already_reaped;
        }
    }
    CHECK(reaped == 1, "exactly one waiter consumes the zombie");
    CHECK(already_reaped == 1, "the losing waiter observes ECHILD");
}

static void test_waitid_pidfd_errors(void)
{
    printf("--- waitid(P_PIDFD) error paths ---\n");

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_ERR(waitid(P_PIDFD, -1, &info, WEXITED | WNOHANG), EINVAL,
              "negative pidfd returns EINVAL");

    int pipefd[2];
    CHECK_RET(pipe(pipefd), 0, "create non-pidfd pipe");
    CHECK_ERR(waitid(P_PIDFD, (id_t)pipefd[0], &info, WEXITED | WNOHANG),
              EBADF, "non-pidfd file descriptor returns EBADF");
    close(pipefd[0]);
    close(pipefd[1]);

    int self_pfd = x_pidfd_open(getpid(), 0);
    CHECK(self_pfd >= 0, "pidfd_open(self) succeeds");
    if (self_pfd >= 0) {
        CHECK_ERR(waitid(P_PIDFD, (id_t)self_pfd, &info, WEXITED | WNOHANG),
                  ECHILD, "pidfd for non-child process returns ECHILD");
        close(self_pfd);
    }
}

int main(void)
{
    TEST_START("waitid P_PIDFD");
    test_waitid_pidfd_reaps_child();
    test_waitid_pidfd_wnowait_keeps_child_waitable();
    test_waitid_pidfd_nohang_alive_child();
    test_pidfd_delays_leader_exit_until_last_thread();
    test_wait_accumulates_reaped_child_cpu_time();
    test_concurrent_wait_reaps_exactly_once();
    test_waitid_pidfd_errors();
    TEST_DONE();
}
