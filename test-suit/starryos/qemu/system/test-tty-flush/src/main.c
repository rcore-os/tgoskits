#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

struct pty_pair {
    int master;
    int slave;
};

static int failures;

static void fail(const char *message)
{
    fprintf(stderr, "FAIL: %s (errno=%d: %s)\n", message, errno,
            strerror(errno));
    failures++;
}

static int set_nonblocking(int fd, int enabled)
{
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0)
        return -1;
    if (enabled)
        flags |= O_NONBLOCK;
    else
        flags &= ~O_NONBLOCK;
    return fcntl(fd, F_SETFL, flags);
}

static int open_raw_pty(struct pty_pair *pty)
{
    pty->master = posix_openpt(O_RDWR | O_NOCTTY);
    if (pty->master < 0 || grantpt(pty->master) != 0
        || unlockpt(pty->master) != 0)
        return -1;

    char *name = ptsname(pty->master);
    if (name == NULL)
        return -1;
    pty->slave = open(name, O_RDWR | O_NOCTTY);
    if (pty->slave < 0)
        return -1;

    struct termios term;
    if (tcgetattr(pty->slave, &term) != 0)
        return -1;
    cfmakeraw(&term);
    term.c_cc[VMIN] = 1;
    term.c_cc[VTIME] = 0;
    if (tcsetattr(pty->slave, TCSANOW, &term) != 0
        || set_nonblocking(pty->master, 1) != 0
        || set_nonblocking(pty->slave, 1) != 0)
        return -1;
    return 0;
}

static void close_pty(struct pty_pair *pty)
{
    if (pty->master >= 0)
        close(pty->master);
    if (pty->slave >= 0)
        close(pty->slave);
}

static int wait_readable(int fd, int timeout_ms)
{
    struct pollfd pfd = {.fd = fd, .events = POLLIN, .revents = 0};
    int rc;
    do {
        rc = poll(&pfd, 1, timeout_ms);
    } while (rc < 0 && errno == EINTR);
    return rc > 0 && (pfd.revents & POLLIN) != 0;
}

static int write_all(int fd, const void *data, size_t len)
{
    const unsigned char *bytes = data;
    size_t offset = 0;
    while (offset < len) {
        ssize_t count = write(fd, bytes + offset, len - offset);
        if (count > 0) {
            offset += (size_t)count;
            continue;
        }
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            struct pollfd pfd = {.fd = fd, .events = POLLOUT, .revents = 0};
            if (poll(&pfd, 1, 2000) > 0)
                continue;
        }
        return -1;
    }
    return 0;
}

static int read_exact(int fd, const void *expected, size_t len)
{
    unsigned char actual[64];
    size_t offset = 0;
    if (len > sizeof(actual))
        return -1;
    while (offset < len) {
        if (!wait_readable(fd, 2000))
            return -1;
        ssize_t count = read(fd, actual + offset, len - offset);
        if (count > 0) {
            offset += (size_t)count;
            continue;
        }
        if (count < 0 && errno == EINTR)
            continue;
        return -1;
    }
    return memcmp(actual, expected, len) == 0 ? 0 : -1;
}

static int expect_empty(int fd)
{
    unsigned char byte;
    if (wait_readable(fd, 50))
        return -1;
    errno = 0;
    return read(fd, &byte, 1) == -1
        && (errno == EAGAIN || errno == EWOULDBLOCK)
        ? 0
        : -1;
}

static int queue_input(struct pty_pair *pty, const char *payload)
{
    return write_all(pty->master, payload, strlen(payload)) == 0
        && wait_readable(pty->slave, 2000)
        ? 0
        : -1;
}

static int queue_output(struct pty_pair *pty, const char *payload)
{
    return write_all(pty->slave, payload, strlen(payload));
}

static void check_selector(int selector, int drop_input, int drop_output)
{
    static const char input[] = "queued-input";
    static const char output[] = "queued-output";
    struct pty_pair pty = {.master = -1, .slave = -1};

    if (open_raw_pty(&pty) != 0) {
        fail("open raw PTY");
        close_pty(&pty);
        return;
    }
    if (queue_input(&pty, input) != 0 || queue_output(&pty, output) != 0) {
        fail("queue PTY input and output");
        close_pty(&pty);
        return;
    }
    if (ioctl(pty.slave, TCFLSH, selector) != 0) {
        fail("TCFLSH selector succeeds");
        close_pty(&pty);
        return;
    }

    if ((drop_input ? expect_empty(pty.slave)
                    : read_exact(pty.slave, input, sizeof(input) - 1)) != 0)
        fail("TCFLSH input effect");
    if ((drop_output ? expect_empty(pty.master)
                     : read_exact(pty.master, output, sizeof(output) - 1)) != 0)
        fail("TCFLSH output effect");
    close_pty(&pty);
}

static void check_invalid_selector(void)
{
    struct pty_pair pty = {.master = -1, .slave = -1};
    if (open_raw_pty(&pty) != 0) {
        fail("open PTY for invalid selector");
        close_pty(&pty);
        return;
    }
    errno = 0;
    if (ioctl(pty.slave, TCFLSH, 3) != -1 || errno != EINVAL)
        fail("TCFLSH rejects invalid selector with EINVAL");
    close_pty(&pty);
}

static void check_reader_wakeup_after_flush(void)
{
    enum { ROUNDS = 64 };
    struct pty_pair pty = {.master = -1, .slave = -1};
    int ready[2] = {-1, -1};
    int done[2] = {-1, -1};

    if (open_raw_pty(&pty) != 0 || pipe(ready) != 0 || pipe(done) != 0) {
        fail("prepare reader wakeup test");
        close_pty(&pty);
        return;
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork reader wakeup test");
        close_pty(&pty);
        return;
    }
    if (child == 0) {
        close(ready[0]);
        close(done[0]);
        close(pty.master);
        if (set_nonblocking(pty.slave, 0) != 0)
            _exit(10);
        for (unsigned int round = 0; round < ROUNDS; round++) {
            unsigned char byte;
            if (write_all(ready[1], "R", 1) != 0)
                _exit(11);
            if (read(pty.slave, &byte, 1) != 1
                || byte != (unsigned char)(round + 1))
                _exit(12);
            if (write_all(done[1], "D", 1) != 0)
                _exit(13);
        }
        _exit(0);
    }

    close(ready[1]);
    close(done[1]);
    for (unsigned int round = 0; round < ROUNDS; round++) {
        unsigned char signal;
        unsigned char byte = (unsigned char)(round + 1);
        if (read(ready[0], &signal, 1) != 1
            || ioctl(pty.slave, TCFLSH, TCIFLUSH) != 0
            || write_all(pty.master, &byte, 1) != 0
            || read(done[0], &signal, 1) != 1) {
            fail("flush followed by blocked reader wakeup");
            break;
        }
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0)
        fail("reader wakeup child completed");
    close(ready[0]);
    close(done[0]);
    close_pty(&pty);
}

int main(void)
{
    check_selector(TCIFLUSH, 1, 0);
    check_selector(TCOFLUSH, 0, 1);
    check_selector(TCIOFLUSH, 1, 1);
    check_invalid_selector();
    check_reader_wakeup_after_flush();

    if (failures != 0) {
        fprintf(stderr, "test-tty-flush: %d failure(s)\n", failures);
        return 1;
    }
    puts("test-tty-flush: PASS");
    return 0;
}
