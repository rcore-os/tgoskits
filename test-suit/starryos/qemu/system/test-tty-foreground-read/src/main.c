/*
 * test-tty-foreground-read
 *
 * A terminal lets only its foreground process group read it, and the check
 * uses the reader's group at the time of the read. A member of the session
 * reads its controlling terminal while in the foreground group, is refused
 * while in a group of its own, and reads the same pending line once back.
 * It then keeps reading and polling the terminal from two threads while two
 * other threads move the process between the two groups; the terminal must
 * keep answering and still deliver a line typed afterwards.
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define PTS_DIR "/tmp/tty-fg-read-pts"
#define READERS 2
#define MOVERS 2
#define MOVES_PER_MOVER 2000

static char slave_name[64];
static int master = -1;
static pid_t foreground_pgid;

static atomic_int movers_left;
static atomic_ulong reads, unexpected_reads, move_errors;

/*
 * The shared /dev/pts table holds sixteen indices for the whole suite, so this
 * case allocates from an instance of its own.
 */
static int open_private_master(const char *mountpoint, char *slave, size_t len)
{
    if (mkdir(mountpoint, 0755) != 0 && errno != EEXIST) {
        return -1;
    }
    if (mount("none", mountpoint, "devpts", 0, "newinstance,mode=0620,gid=5,ptmxmode=0666") != 0) {
        return -1;
    }

    char ptmx[96];
    snprintf(ptmx, sizeof(ptmx), "%s/ptmx", mountpoint);
    int fd = open(ptmx, O_RDWR | O_NOCTTY);
    if (fd < 0) {
        return -1;
    }
    unsigned int number = 0;
    int unlock = 0;
    if (ioctl(fd, TIOCGPTN, &number) != 0 || ioctl(fd, TIOCSPTLCK, &unlock) != 0) {
        close(fd);
        return -1;
    }
    snprintf(slave, len, "%s/%u", mountpoint, number);
    return fd;
}

/* Linux refuses a background read with EIO once SIGTTIN is ignored. */
static int read_refused(int err)
{
    return err == EIO || err == EAGAIN;
}

static void *reader(void *arg)
{
    int tty = *(int *)arg;
    char buf[16];
    struct pollfd pfd = {.fd = tty, .events = POLLIN};
    while (atomic_load(&movers_left) > 0) {
        poll(&pfd, 1, 0);
        /* Nothing is typed during this phase, so every read must fail. */
        ssize_t n = read(tty, buf, sizeof(buf));
        if (n >= 0 || !read_refused(errno)) {
            atomic_fetch_add(&unexpected_reads, 1);
        }
        atomic_fetch_add(&reads, 1);
    }
    return NULL;
}

static void *mover(void *arg)
{
    (void)arg;
    for (int i = 0; i < MOVES_PER_MOVER; i++) {
        if (setpgid(0, 0) != 0 || setpgid(0, foreground_pgid) != 0) {
            atomic_fetch_add(&move_errors, 1);
        }
    }
    atomic_fetch_sub(&movers_left, 1);
    return NULL;
}

static void type_line(const char *line)
{
    size_t len = strlen(line);
    CHECK_RET(write(master, line, len), (long)len, "type a line on the master");
}

static void read_line(int tty, const char *line, const char *msg)
{
    struct pollfd pfd = {.fd = tty, .events = POLLIN};
    CHECK_RET(poll(&pfd, 1, 5000), 1, "the pending line is readable");
    char buf[16] = {0};
    CHECK_RET(read(tty, buf, sizeof(buf) - 1), (long)strlen(line), msg);
    CHECK(strcmp(buf, line) == 0, "the line arrives unchanged");
}

static void member_reads_terminal(void)
{
    signal(SIGTTIN, SIG_IGN);
    signal(SIGTTOU, SIG_IGN);
    int tty = open(slave_name, O_RDWR | O_NONBLOCK | O_NOCTTY);
    CHECK(tty >= 0, "the member opens its controlling terminal");
    CHECK_RET(tcgetpgrp(tty), foreground_pgid, "the member starts in the foreground group");

    type_line("a\n");
    read_line(tty, "a\n", "the foreground group reads the line");

    CHECK_RET(setpgid(0, 0), 0, "the member moves to a group of its own");
    type_line("b\n");
    char buf[16];
    errno = 0;
    ssize_t n = read(tty, buf, sizeof(buf));
    CHECK(n < 0 && read_refused(errno), "a background group is refused the read");
    CHECK_RET(setpgid(0, foreground_pgid), 0, "the member returns to the foreground group");
    read_line(tty, "b\n", "the refused line is still there for the foreground group");

    pthread_t readers[READERS];
    pthread_t movers[MOVERS];
    atomic_store(&movers_left, MOVERS);
    for (int i = 0; i < READERS; i++) {
        CHECK_RET(pthread_create(&readers[i], NULL, reader, &tty), 0, "start a reader");
    }
    for (int i = 0; i < MOVERS; i++) {
        CHECK_RET(pthread_create(&movers[i], NULL, mover, NULL), 0, "start a mover");
    }
    for (int i = 0; i < MOVERS; i++) {
        pthread_join(movers[i], NULL);
    }
    for (int i = 0; i < READERS; i++) {
        pthread_join(readers[i], NULL);
    }
    printf("  reads=%lu moves=%d\n", atomic_load(&reads), MOVERS * MOVES_PER_MOVER * 2);
    CHECK(atomic_load(&move_errors) == 0, "every group move succeeds");
    CHECK(atomic_load(&reads) > 0, "the readers ran while the groups changed");
    CHECK(atomic_load(&unexpected_reads) == 0, "reads during the moves only report an empty or refused terminal");

    CHECK_RET(getpgrp(), foreground_pgid, "the member ends in the foreground group");
    type_line("c\n");
    read_line(tty, "c\n", "the terminal still delivers a line after the moves");
    close(tty);
}

static void session_with_terminal(void)
{
    CHECK(setsid() > 0, "start a session");
    int tty = open(slave_name, O_RDWR | O_NOCTTY);
    CHECK(tty >= 0, "open the slave");
    CHECK_RET(ioctl(tty, TIOCSCTTY, 0), 0, "make it the controlling terminal");
    foreground_pgid = getpgrp();

    fflush(stdout);
    int failed_before = __fail;
    pid_t member = fork();
    if (member < 0) {
        CHECK(0, "fork the session member");
        return;
    }
    if (member == 0) {
        member_reads_terminal();
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(member, &status, 0), member, "the member runs to the end");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the member's checks all passed");
}

int main(void)
{
    TEST_START("tty reads follow the reader's current process group");

    master = open_private_master(PTS_DIR, slave_name, sizeof(slave_name));
    CHECK(master >= 0, "allocate a pty from a private devpts instance");
    if (master < 0) {
        TEST_DONE();
    }

    fflush(stdout);
    int failed_before = __fail;
    pid_t leader = fork();
    if (leader < 0) {
        CHECK(0, "fork the session leader");
        TEST_DONE();
    }
    if (leader == 0) {
        session_with_terminal();
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(leader, &status, 0), leader, "the session runs to the end");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the session's checks all passed");
    close(master);

    TEST_DONE();
}
