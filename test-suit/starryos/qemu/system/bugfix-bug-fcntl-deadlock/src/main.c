/*
 * bug-fcntl-deadlock: POSIX F_SETLKW must report EDEADLK when the
 * requested byte-range lock would create a process wait-for cycle.
 *
 * Reproducer:
 *   child A holds [0, 1) and blocks on child B's [1, 2).
 *   child B then tries to block on child A's [0, 1).
 *
 * Linux detects the cycle B -> A -> B and returns EDEADLK to B. Without
 * deadlock detection both children sleep forever.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                      \
    if (cond) {                                                         \
        printf("  PASS: " fmt "\n", ##__VA_ARGS__);                   \
        passed++;                                                       \
    } else {                                                            \
        printf("  FAIL: " fmt "\n", ##__VA_ARGS__);                   \
        failed++;                                                       \
    }                                                                   \
} while (0)

static void msleep(long ms)
{
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (ms % 1000) * 1000000;
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}

static void write_byte(int fd, char byte)
{
    ssize_t n = write(fd, &byte, 1);
    if (n != 1) {
        _exit(90);
    }
}

static char read_byte(int fd)
{
    char byte = 0;
    ssize_t n = read(fd, &byte, 1);
    if (n != 1) {
        _exit(91);
    }
    return byte;
}

static int set_lock_cmd(int fd, int cmd, short type, off_t start, off_t len)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fcntl(fd, cmd, &fl);
}

static pid_t wait_child_timeout(pid_t pid, int *status, long timeout_ms)
{
    long waited = 0;
    while (waited < timeout_ms) {
        pid_t got = waitpid(pid, status, WNOHANG);
        if (got != 0) {
            return got;
        }
        msleep(10);
        waited += 10;
    }
    return 0;
}

static pid_t spawn_locker_a(const char *path, int cmd_read_fd, int event_fd)
{
    pid_t pid = fork();
    if (pid != 0) {
        return pid;
    }

    int fd = open(path, O_RDWR);
    if (fd < 0) {
        _exit(10);
    }
    if (set_lock_cmd(fd, F_SETLK, F_WRLCK, 0, 1) != 0) {
        _exit(11);
    }
    write_byte(event_fd, 'L');

    if (read_byte(cmd_read_fd) != 'S') {
        _exit(12);
    }
    write_byte(event_fd, 'W');

    errno = 0;
    if (set_lock_cmd(fd, F_SETLKW, F_WRLCK, 1, 1) != 0) {
        _exit(errno == EDEADLK ? 13 : 14);
    }
    set_lock_cmd(fd, F_SETLK, F_UNLCK, 0, 2);
    close(fd);
    _exit(0);
}

static pid_t spawn_locker_b(const char *path, int cmd_read_fd, int event_fd)
{
    pid_t pid = fork();
    if (pid != 0) {
        return pid;
    }

    int fd = open(path, O_RDWR);
    if (fd < 0) {
        _exit(20);
    }
    if (set_lock_cmd(fd, F_SETLK, F_WRLCK, 1, 1) != 0) {
        _exit(21);
    }
    write_byte(event_fd, 'L');

    if (read_byte(cmd_read_fd) != 'S') {
        _exit(22);
    }

    errno = 0;
    int ret = set_lock_cmd(fd, F_SETLKW, F_WRLCK, 0, 1);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EDEADLK) {
        set_lock_cmd(fd, F_SETLK, F_UNLCK, 1, 1);
        close(fd);
        _exit(0);
    }
    if (ret == 0) {
        set_lock_cmd(fd, F_SETLK, F_UNLCK, 0, 2);
        close(fd);
        _exit(23);
    }
    set_lock_cmd(fd, F_SETLK, F_UNLCK, 1, 1);
    close(fd);
    _exit(24);
}

static int run_deadlock_cycle(const char *path)
{
    int a_cmd[2], a_evt[2], b_cmd[2], b_evt[2];
    if (pipe(a_cmd) != 0 || pipe(a_evt) != 0 ||
        pipe(b_cmd) != 0 || pipe(b_evt) != 0) {
        printf("FAIL: pipe setup: %s\n", strerror(errno));
        return 1;
    }

    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        printf("FAIL: create test file: %s\n", strerror(errno));
        return 1;
    }
    if (ftruncate(fd, 4096) != 0) {
        printf("FAIL: ftruncate: %s\n", strerror(errno));
        close(fd);
        return 1;
    }
    close(fd);

    pid_t a = spawn_locker_a(path, a_cmd[0], a_evt[1]);
    pid_t b = spawn_locker_b(path, b_cmd[0], b_evt[1]);
    if (a < 0 || b < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        return 1;
    }

    close(a_cmd[0]);
    close(a_evt[1]);
    close(b_cmd[0]);
    close(b_evt[1]);

    CHECK(read_byte(a_evt[0]) == 'L', "child A acquired [0,1) lock");
    CHECK(read_byte(b_evt[0]) == 'L', "child B acquired [1,2) lock");

    write_byte(a_cmd[1], 'S');
    CHECK(read_byte(a_evt[0]) == 'W', "child A entered F_SETLKW path");
    msleep(150);

    write_byte(b_cmd[1], 'S');

    int b_status = 0;
    pid_t got_b = wait_child_timeout(b, &b_status, 2000);
    if (got_b == 0) {
        kill(b, SIGKILL);
        waitpid(b, &b_status, 0);
    }
    CHECK(got_b == b, "child B returned instead of blocking forever");
    CHECK(WIFEXITED(b_status), "child B exited normally (status=0x%x)",
          b_status);
    CHECK(WIFEXITED(b_status) && WEXITSTATUS(b_status) == 0,
          "child B observed EDEADLK");

    int a_status = 0;
    pid_t got_a = wait_child_timeout(a, &a_status, 2000);
    if (got_a == 0) {
        kill(a, SIGKILL);
        waitpid(a, &a_status, 0);
    }
    CHECK(got_a == a, "child A woke after child B released [1,2)");
    CHECK(WIFEXITED(a_status), "child A exited normally (status=0x%x)",
          a_status);
    CHECK(WIFEXITED(a_status) && WEXITSTATUS(a_status) == 0,
          "child A acquired the released lock");

    close(a_cmd[1]);
    close(a_evt[0]);
    close(b_cmd[1]);
    close(b_evt[0]);
    return failed == 0 ? 0 : 1;
}

int main(void)
{
    printf("=== bug-fcntl-deadlock ===\n");

    const char *path = "/tmp/starry_bug_fcntl_deadlock";
    run_deadlock_cycle(path);
    unlink(path);

    printf("=== bug-fcntl-deadlock: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
