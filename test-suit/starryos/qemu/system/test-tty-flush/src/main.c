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

static void check_selector(struct pty_pair *pty, int selector, int drop_input,
                           int drop_output)
{
    static const char input[] = "queued-input";
    static const char output[] = "queued-output";

    if (queue_input(pty, input) != 0 || queue_output(pty, output) != 0) {
        fail("queue PTY input and output");
        return;
    }
    if (ioctl(pty->slave, TCFLSH, selector) != 0) {
        fail("TCFLSH selector succeeds");
        return;
    }

    if ((drop_input ? expect_empty(pty->slave)
                    : read_exact(pty->slave, input, sizeof(input) - 1)) != 0)
        fail("TCFLSH input effect");
    if ((drop_output ? expect_empty(pty->master)
                     : read_exact(pty->master, output, sizeof(output) - 1)) != 0)
        fail("TCFLSH output effect");
}

static void check_invalid_selector(struct pty_pair *pty)
{
    errno = 0;
    if (ioctl(pty->slave, TCFLSH, 3) != -1 || errno != EINVAL)
        fail("TCFLSH rejects invalid selector with EINVAL");
}

static void check_input_source_flush(struct pty_pair *pty)
{
    static const unsigned char stale = 0x5a;
    static const unsigned char fresh = 0xa5;
    unsigned char delivered[4096];

    memset(delivered, 0x33, sizeof(delivered));
    if (write_all(pty->master, delivered, sizeof(delivered)) != 0
        || !wait_readable(pty->slave, 2000)) {
        fail("fill line discipline input buffer");
        return;
    }
    if (write_all(pty->master, &stale, sizeof(stale)) != 0
        || ioctl(pty->slave, TCFLSH, TCIFLUSH) != 0) {
        fail("flush input before line discipline delivery");
        return;
    }
    if (write_all(pty->master, &fresh, sizeof(fresh)) != 0
        || read_exact(pty->slave, &fresh, sizeof(fresh)) != 0)
        fail("TCIFLUSH discards source and staged input");
}

static void check_deferred_echo_flush(void)
{
    struct pty_pair pty = {.master = -1, .slave = -1};
    unsigned char fill[4096];
    static const unsigned char stale = 'S';
    static const unsigned char fresh = 'F';
    struct termios term;

    memset(fill, 'x', sizeof(fill));
    if (open_raw_pty(&pty) != 0 || tcgetattr(pty.slave, &term) != 0) {
        fail("open PTY for deferred echo flush");
        goto out;
    }
    term.c_lflag |= ECHO;
    if (tcsetattr(pty.slave, TCSANOW, &term) != 0
        || write_all(pty.slave, fill, sizeof(fill)) != 0
        || write_all(pty.master, &stale, sizeof(stale)) != 0
        || !wait_readable(pty.slave, 2000)) {
        fail("queue echo behind full PTY output");
        goto out;
    }

    if (ioctl(pty.slave, TCFLSH, TCOFLUSH) != 0
        || write_all(pty.master, &fresh, sizeof(fresh)) != 0
        || read_exact(pty.master, &fresh, sizeof(fresh)) != 0
        || expect_empty(pty.master) != 0)
        fail("TCOFLUSH discards deferred echo");

out:
    close_pty(&pty);
}

static void check_serial_output_flush(void)
{
    int fd = open("/dev/ttyS0", O_RDWR | O_NOCTTY | O_NONBLOCK);
    int rc;

    if (fd < 0) {
        fail("open serial TTY for output flush");
        return;
    }
    rc = ioctl(fd, TCFLSH, TCOFLUSH);
    if (rc != 0) {
        if (errno == EOPNOTSUPP)
            puts("SKIP: serial hardware has no TX-only discard");
        else
            fail("TCOFLUSH discards serial output");
    }
    close(fd);
}

static void check_reader_wakeup_after_flush(struct pty_pair *pty)
{
    enum { ROUNDS = 64 };
    int ready[2] = {-1, -1};
    int done[2] = {-1, -1};

    if (pipe(ready) != 0 || pipe(done) != 0) {
        fail("prepare reader wakeup test");
        if (ready[0] >= 0)
            close(ready[0]);
        if (ready[1] >= 0)
            close(ready[1]);
        if (done[0] >= 0)
            close(done[0]);
        if (done[1] >= 0)
            close(done[1]);
        return;
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork reader wakeup test");
        close(ready[0]);
        close(ready[1]);
        close(done[0]);
        close(done[1]);
        return;
    }
    if (child == 0) {
        close(ready[0]);
        close(done[0]);
        close(pty->master);
        if (set_nonblocking(pty->slave, 0) != 0)
            _exit(10);
        for (unsigned int round = 0; round < ROUNDS; round++) {
            unsigned char byte;
            if (write_all(ready[1], "R", 1) != 0)
                _exit(11);
            if (read(pty->slave, &byte, 1) != 1
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
            || ioctl(pty->slave, TCFLSH, TCIFLUSH) != 0
            || write_all(pty->master, &byte, 1) != 0
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
}

int main(void)
{
    struct pty_pair pty = {.master = -1, .slave = -1};

    if (open_raw_pty(&pty) != 0) {
        fail("open raw PTY");
    } else {
        check_selector(&pty, TCIFLUSH, 1, 0);
        check_selector(&pty, TCOFLUSH, 0, 1);
        check_selector(&pty, TCIOFLUSH, 1, 1);
        check_invalid_selector(&pty);
        check_reader_wakeup_after_flush(&pty);
        check_input_source_flush(&pty);
    }
    close_pty(&pty);
    check_deferred_echo_flush();
    check_serial_output_flush();

    if (failures != 0) {
        fprintf(stderr, "test-tty-flush: %d failure(s)\n", failures);
        return 1;
    }
    puts("test-tty-flush: PASS");
    return 0;
}
