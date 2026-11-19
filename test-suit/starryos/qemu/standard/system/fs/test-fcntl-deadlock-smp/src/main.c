/*
 * test-fcntl-deadlock-smp: POSIX F_SETLKW deadlock detection must serialize
 * wait graph updates across different inode wait queues.
 *
 * Reproducer:
 *   child A locks file1, then blocks on file2.
 *   child B locks file2, then blocks on file1.
 *
 * The final two F_SETLKW calls target different inodes. If deadlock
 * detection and wait-edge insertion are not protected by the same global
 * wait graph lock, both children can miss the other's pending edge and sleep
 * forever.
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

#define ROUNDS 32
#define CHILD_EDEADLK 0
#define CHILD_ACQUIRED 3

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                      \
    if (cond) {                                                         \
        printf("  PASS: " fmt "\n", ##__VA_ARGS__);                    \
        passed++;                                                       \
    } else {                                                            \
        printf("  FAIL: " fmt "\n", ##__VA_ARGS__);                    \
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
    if (write(fd, &byte, 1) != 1) {
        _exit(90);
    }
}

static char read_byte(int fd)
{
    char byte = 0;
    if (read(fd, &byte, 1) != 1) {
        _exit(91);
    }
    return byte;
}

static int set_lock_cmd(int fd, int cmd, short type)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = type;
    fl.l_whence = SEEK_SET;
    fl.l_start = 0;
    fl.l_len = 1;
    return fcntl(fd, cmd, &fl);
}

static void prepare_file(const char *path)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || ftruncate(fd, 4096) != 0) {
        _exit(92);
    }
    close(fd);
}

static pid_t wait_child_timeout(pid_t pid, int *status, long timeout_ms)
{
    long waited = 0;
    do {
        pid_t got = waitpid(pid, status, WNOHANG);
        if (got != 0) {
            return got;
        }
        msleep(10);
        waited += 10;
    } while (waited < timeout_ms);
    return 0;
}

static pid_t spawn_locker(const char *hold_path, const char *wait_path,
                          int start_read_fd, int event_fd)
{
    pid_t pid = fork();
    if (pid != 0) {
        return pid;
    }

    int hold_fd = open(hold_path, O_RDWR);
    int wait_fd = open(wait_path, O_RDWR);
    if (hold_fd < 0 || wait_fd < 0) {
        _exit(10);
    }
    if (set_lock_cmd(hold_fd, F_SETLK, F_WRLCK) != 0) {
        _exit(11);
    }
    write_byte(event_fd, 'L');

    if (read_byte(start_read_fd) != 'S') {
        _exit(12);
    }

    errno = 0;
    int ret = set_lock_cmd(wait_fd, F_SETLKW, F_WRLCK);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EDEADLK) {
        set_lock_cmd(hold_fd, F_SETLK, F_UNLCK);
        close(wait_fd);
        close(hold_fd);
        _exit(CHILD_EDEADLK);
    }
    if (ret == 0) {
        set_lock_cmd(wait_fd, F_SETLK, F_UNLCK);
        set_lock_cmd(hold_fd, F_SETLK, F_UNLCK);
        close(wait_fd);
        close(hold_fd);
        _exit(CHILD_ACQUIRED);
    }
    set_lock_cmd(hold_fd, F_SETLK, F_UNLCK);
    close(wait_fd);
    close(hold_fd);
    _exit(2);
}

static int run_one_round(int round)
{
    char file1[128];
    char file2[128];
    snprintf(file1, sizeof(file1), "/tmp/starry_fcntl_deadlock_smp_%d_a", round);
    snprintf(file2, sizeof(file2), "/tmp/starry_fcntl_deadlock_smp_%d_b", round);
    prepare_file(file1);
    prepare_file(file2);

    int start_pipe[2], a_evt[2], b_evt[2];
    if (pipe(start_pipe) != 0 || pipe(a_evt) != 0 || pipe(b_evt) != 0) {
        printf("  FAIL: pipe setup: %s\n", strerror(errno));
        return 1;
    }

    pid_t a = spawn_locker(file1, file2, start_pipe[0], a_evt[1]);
    pid_t b = spawn_locker(file2, file1, start_pipe[0], b_evt[1]);
    if (a < 0 || b < 0) {
        printf("  FAIL: fork: %s\n", strerror(errno));
        return 1;
    }

    close(start_pipe[0]);
    close(a_evt[1]);
    close(b_evt[1]);

    if (read_byte(a_evt[0]) != 'L' || read_byte(b_evt[0]) != 'L') {
        return 1;
    }

    write_byte(start_pipe[1], 'S');
    write_byte(start_pipe[1], 'S');

    int a_status = 0;
    int b_status = 0;
    pid_t got_a = 0;
    pid_t got_b = 0;
    long waited = 0;
    while (waited < 3000 && (got_a == 0 || got_b == 0)) {
        if (got_a == 0) {
            got_a = wait_child_timeout(a, &a_status, 0);
        }
        if (got_b == 0) {
            got_b = wait_child_timeout(b, &b_status, 0);
        }
        if (got_a == 0 || got_b == 0) {
            msleep(10);
            waited += 10;
        }
    }
    if (got_a == 0) {
        kill(a, SIGKILL);
        waitpid(a, &a_status, 0);
    }
    if (got_b == 0) {
        kill(b, SIGKILL);
        waitpid(b, &b_status, 0);
    }

    close(start_pipe[1]);
    close(a_evt[0]);
    close(b_evt[0]);
    unlink(file1);
    unlink(file2);

    int a_deadlock = got_a == a && WIFEXITED(a_status) &&
        WEXITSTATUS(a_status) == CHILD_EDEADLK;
    int b_deadlock = got_b == b && WIFEXITED(b_status) &&
        WEXITSTATUS(b_status) == CHILD_EDEADLK;
    int a_acquired = got_a == a && WIFEXITED(a_status) &&
        WEXITSTATUS(a_status) == CHILD_ACQUIRED;
    int b_acquired = got_b == b && WIFEXITED(b_status) &&
        WEXITSTATUS(b_status) == CHILD_ACQUIRED;
    if ((a_deadlock + b_deadlock) != 1 || (a_acquired + b_acquired) != 1) {
        printf("  round %d: expected one EDEADLK and one successful lock "
               "(a=0x%x b=0x%x)\n", round, a_status, b_status);
        return 1;
    }
    return 0;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== test-fcntl-deadlock-smp ===\n");

    for (int i = 0; i < ROUNDS; i++) {
        int ok = run_one_round(i) == 0;
        CHECK(ok, "cross-inode deadlock round %d", i);
        if (!ok) {
            break;
        }
    }

    printf("=== test-fcntl-deadlock-smp: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
