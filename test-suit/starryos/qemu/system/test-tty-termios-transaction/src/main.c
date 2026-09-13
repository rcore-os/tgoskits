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

#define KERNEL_NCCS 19
#define KERNEL_CBAUD 0010017U
#define KERNEL_BOTHER 0010000U

struct kernel_termios2 {
    unsigned int c_iflag;
    unsigned int c_oflag;
    unsigned int c_cflag;
    unsigned int c_lflag;
    unsigned char c_line;
    unsigned char c_cc[KERNEL_NCCS];
    unsigned int c_ispeed;
    unsigned int c_ospeed;
};

#define STARRY_TCGETS2 _IOR('T', 0x2a, struct kernel_termios2)
#define STARRY_TCSETS2 _IOW('T', 0x2b, struct kernel_termios2)
#define STARRY_TCSETSW2 _IOW('T', 0x2c, struct kernel_termios2)
#define STARRY_TCSETSF2 _IOW('T', 0x2d, struct kernel_termios2)

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

static int set_nonblocking(int fd)
{
    int flags = fcntl(fd, F_GETFL);
    return flags < 0 ? -1 : fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

static int write_all(int fd, const void *data, size_t length)
{
    const unsigned char *bytes = data;
    size_t offset = 0;

    while (offset < length) {
        ssize_t written = write(fd, bytes + offset, length - offset);
        if (written > 0) {
            offset += (size_t)written;
            continue;
        }
        if (written < 0 && errno == EINTR)
            continue;
        if (written < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            struct pollfd pfd = {.fd = fd, .events = POLLOUT, .revents = 0};
            if (poll(&pfd, 1, 2000) > 0)
                continue;
        }
        return -1;
    }
    return 0;
}

static int wait_readable(int fd)
{
    struct pollfd pfd = {.fd = fd, .events = POLLIN, .revents = 0};
    int rc;

    do {
        rc = poll(&pfd, 1, 2000);
    } while (rc < 0 && errno == EINTR);
    return rc > 0 && (pfd.revents & POLLIN) != 0;
}

static int open_raw_pty(struct pty_pair *pty, struct kernel_termios2 *raw)
{
    pty->master = posix_openpt(O_RDWR | O_NOCTTY);
    if (pty->master < 0 || grantpt(pty->master) != 0
        || unlockpt(pty->master) != 0)
        return -1;

    char *name = ptsname(pty->master);
    if (name == NULL)
        return -1;
    pty->slave = open(name, O_RDWR | O_NOCTTY);
    if (pty->slave < 0 || ioctl(pty->slave, STARRY_TCGETS2, raw) != 0)
        return -1;

    raw->c_iflag = 0;
    raw->c_oflag = 0;
    raw->c_lflag = 0;
    raw->c_cc[VMIN] = 1;
    raw->c_cc[VTIME] = 0;
    if (ioctl(pty->slave, STARRY_TCSETS2, raw) != 0
        || set_nonblocking(pty->master) != 0
        || set_nonblocking(pty->slave) != 0)
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

static void check_failed_serial_config_is_not_published(int fd,
                                                        struct kernel_termios2 *valid)
{
    struct kernel_termios2 invalid = *valid;
    struct kernel_termios2 observed;

    invalid.c_cflag = (invalid.c_cflag & ~KERNEL_CBAUD) | KERNEL_BOTHER;
    invalid.c_ispeed = 250000000U;
    invalid.c_ospeed = 250000000U;

    errno = 0;
    if (ioctl(fd, STARRY_TCSETS2, &invalid) != -1 || errno != EINVAL)
        fail("TCSETS2 propagates invalid UART configuration as EINVAL");
    if (ioctl(fd, STARRY_TCGETS2, &observed) != 0) {
        fail("TCGETS2 after rejected configuration");
    } else if (memcmp(&observed, valid, sizeof(observed)) != 0) {
        fail("rejected UART configuration preserves published termios");
    }

    if (ioctl(fd, STARRY_TCSETS2, valid) != 0)
        fail("restore valid serial termios");
}

static void check_serial_writes_with_waiting_updates(int fd,
                                                     struct kernel_termios2 *valid)
{
    int ready[2] = {-1, -1};
    unsigned char zeros[256] = {0};

    if (pipe(ready) != 0) {
        fail("create concurrent serial writer pipe");
        return;
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork concurrent serial writer");
        close(ready[0]);
        close(ready[1]);
        return;
    }
    if (child == 0) {
        close(ready[0]);
        if (write_all(ready[1], "R", 1) != 0
            || write_all(fd, zeros, sizeof(zeros)) != 0)
            _exit(10);
        _exit(0);
    }

    close(ready[1]);
    unsigned char signal;
    if (read(ready[0], &signal, 1) != 1) {
        fail("start concurrent serial writer");
    } else {
        for (unsigned int round = 0; round < 32; round++) {
            unsigned long request = (round & 1U) == 0 ? STARRY_TCSETSW2
                                                      : STARRY_TCSETSF2;
            if (ioctl(fd, request, valid) != 0) {
                fail("TCSETSW2/TCSETSF2 serialize with serial writes");
                break;
            }
        }
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0)
        fail("concurrent serial writer completed");
    close(ready[0]);
}

static void check_tcsetsf_clears_input_after_update(void)
{
    struct pty_pair pty = {.master = -1, .slave = -1};
    struct kernel_termios2 raw;
    const unsigned char stale = 'S';
    const unsigned char fresh = 'F';
    unsigned char observed = 0;

    if (open_raw_pty(&pty, &raw) != 0) {
        fail("open raw PTY for TCSETSF2");
        goto out;
    }
    if (write_all(pty.master, &stale, 1) != 0 || !wait_readable(pty.slave)) {
        fail("queue stale input before TCSETSF2");
        goto out;
    }
    if (ioctl(pty.slave, STARRY_TCSETSF2, &raw) != 0) {
        fail("TCSETSF2 updates termios before flushing input");
        goto out;
    }

    errno = 0;
    if (read(pty.slave, &observed, 1) != -1
        || (errno != EAGAIN && errno != EWOULDBLOCK)) {
        fail("TCSETSF2 discards stale input");
        goto out;
    }
    if (write_all(pty.master, &fresh, 1) != 0 || !wait_readable(pty.slave)
        || read(pty.slave, &observed, 1) != 1 || observed != fresh)
        fail("fresh input remains visible after TCSETSF2");

out:
    close_pty(&pty);
}

int main(void)
{
    int serial = open("/dev/ttyS0", O_RDWR | O_NOCTTY);
    struct kernel_termios2 valid;

    if (serial < 0 || ioctl(serial, STARRY_TCGETS2, &valid) != 0) {
        fail("open serial TTY and read termios2");
    } else {
        check_failed_serial_config_is_not_published(serial, &valid);
        check_serial_writes_with_waiting_updates(serial, &valid);
    }
    if (serial >= 0)
        close(serial);
    check_tcsetsf_clears_input_after_update();

    if (failures != 0) {
        fprintf(stderr, "test-tty-termios-transaction: %d failure(s)\n",
                failures);
        return 1;
    }
    puts("test-tty-termios-transaction: PASS");
    return 0;
}
