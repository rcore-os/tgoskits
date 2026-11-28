#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

#define KERNEL_NCCS 19
#define KERNEL_CBAUD 0010017U
#define KERNEL_BOTHER 0010000U
#define KERNEL_IBSHIFT 16U
#define KERNEL_CIBAUD (KERNEL_CBAUD << KERNEL_IBSHIFT)

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

static void check_format_only_updates(int fd, struct kernel_termios2 initial)
{
    static const unsigned long requests[] = {
        TCSETS, TCSETSW, TCSETSF,
        STARRY_TCSETS2, STARRY_TCSETSW2, STARRY_TCSETSF2,
    };
    struct kernel_termios2 format = initial;

    for (size_t i = 0; i < sizeof(requests) / sizeof(requests[0]); i++) {
        struct kernel_termios2 observed;
        format.c_cflag ^= CSTOPB;
        if (syscall(SYS_ioctl, fd, requests[i], &format) != 0
            || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0) {
            fprintf(stderr, "format-only ioctl request: %#lx\n", requests[i]);
            fail("apply and read back format-only termios update");
            return;
        }
        if (observed.c_cflag != format.c_cflag
            || observed.c_ispeed != initial.c_ispeed
            || observed.c_ospeed != initial.c_ospeed) {
            fprintf(stderr, "format-only ioctl request: %#lx\n", requests[i]);
            fail("update line format while preserving baudrate");
            return;
        }
    }
}

static int is_expected_invalid_baudrate_error(int error)
{
#ifdef __riscv
    /* QEMU virt declares a programmable NS16550 input clock. */
    return error == EINVAL;
#else
    /* Some other firmware describes the divisor but omits the input clock. */
    return error == EINVAL || error == EOPNOTSUPP;
#endif
}

static void check_serial_hangup_and_restore(
    int fd, const struct kernel_termios2 *initial)
{
    static const unsigned long requests[] = {
        TCSETS, TCSETSW, TCSETSF,
        STARRY_TCSETS2, STARRY_TCSETSW2, STARRY_TCSETSF2,
    };
    struct kernel_termios2 hangup = *initial;
    hangup.c_cflag &= ~(KERNEL_CBAUD | KERNEL_CIBAUD);
    hangup.c_ispeed = 0;
    hangup.c_ospeed = 0;

    for (size_t i = 0; i < sizeof(requests) / sizeof(requests[0]); i++) {
        struct kernel_termios2 observed;
        if (syscall(SYS_ioctl, fd, requests[i], &hangup) != 0
            || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0) {
            fprintf(stderr, "hangup ioctl request: %#lx\n", requests[i]);
            fail("apply and read back physical serial B0");
            return;
        }
        if (memcmp(&observed, &hangup, sizeof(observed)) != 0) {
            fail("physical serial reports B0 and zero speeds");
        }

        struct kernel_termios2 unavailable = *initial;
        unavailable.c_cflag = (unavailable.c_cflag & ~KERNEL_CBAUD) | KERNEL_BOTHER;
        unavailable.c_ospeed = 250000000U;
        errno = 0;
        if (syscall(SYS_ioctl, fd, STARRY_TCSETS2, &unavailable) != -1
            || !is_expected_invalid_baudrate_error(errno)
            || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0
            || memcmp(&observed, &hangup, sizeof(observed)) != 0)
            fail("failed physical serial reconfiguration preserves B0");

        /* Legacy TCSETS requests cannot encode a custom numeric baud rate. */
        unsigned long restore_request = (initial->c_cflag & KERNEL_CBAUD) == KERNEL_BOTHER
            ? STARRY_TCSETS2 : requests[i];
        if (syscall(SYS_ioctl, fd, restore_request, initial) != 0
            || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0
            || memcmp(&observed, initial, sizeof(observed)) != 0) {
            fail("restore physical serial speed after B0");
            return;
        }
    }
}

static void check_baudrate_normalization(void)
{
    struct pty_pair pty = {.master = -1, .slave = -1};
    struct kernel_termios2 raw;
    struct kernel_termios2 observed;

    if (open_raw_pty(&pty, &raw) != 0) {
        fail("open raw PTY for custom baudrate");
        goto out;
    }
    raw.c_cflag = (raw.c_cflag & ~(KERNEL_CBAUD | KERNEL_CIBAUD))
        | KERNEL_BOTHER | (KERNEL_BOTHER << KERNEL_IBSHIFT);
    raw.c_ispeed = 654321U;
    raw.c_ospeed = 123456U;
    if (syscall(SYS_ioctl, pty.slave, STARRY_TCSETS2, &raw) != 0) {
        fail("set a custom baudrate before legacy format update");
        goto out;
    }
    check_format_only_updates(pty.slave, raw);

    raw.c_cflag = (raw.c_cflag & ~(KERNEL_CBAUD | KERNEL_CIBAUD))
        | B9600 | (B4800 << KERNEL_IBSHIFT);
    if (syscall(SYS_ioctl, pty.slave, TCSETS, &raw) != 0
        || syscall(SYS_ioctl, pty.slave, STARRY_TCGETS2, &observed) != 0
        || observed.c_ispeed != 4800U || observed.c_ospeed != 9600U)
        fail("TCSETS decodes independent encoded rates over previous BOTHER values");

    raw.c_cflag = (raw.c_cflag & ~(KERNEL_CBAUD | KERNEL_CIBAUD)) | B19200;
    if (syscall(SYS_ioctl, pty.slave, STARRY_TCSETS2, &raw) != 0
        || syscall(SYS_ioctl, pty.slave, STARRY_TCGETS2, &observed) != 0
        || observed.c_ispeed != 19200U || observed.c_ospeed != 19200U)
        fail("TCSETS2 ignores numeric rates for fixed encodings and inherits output speed");

    raw.c_cflag = (raw.c_cflag & ~KERNEL_CBAUD) | KERNEL_BOTHER;
    raw.c_ospeed = 0;
    if (syscall(SYS_ioctl, pty.slave, STARRY_TCSETS2, &raw) != 0
        || syscall(SYS_ioctl, pty.slave, STARRY_TCGETS2, &observed) != 0
        || observed.c_ispeed != 0 || observed.c_ospeed != 0)
        fail("TCSETS2 keeps zero BOTHER output and inherits it for input");

out:
    close_pty(&pty);
}

static void check_serial_reports_common_baudrate(int fd,
                                                 const struct kernel_termios2 *valid)
{
    struct kernel_termios2 request = *valid;
    struct kernel_termios2 observed = {0};

    unsigned int expected_baudrate = valid->c_ospeed;
#ifdef __riscv
    /* Neither rate is exactly representable by the QEMU virt NS16550 divisor.
     * TCGETS2 returns the accepted nominal speed for both directions. */
    expected_baudrate = valid->c_ospeed == 100000U ? 100001U : 100000U;
    request.c_cflag = (request.c_cflag & ~KERNEL_CBAUD) | KERNEL_BOTHER;
    request.c_ospeed = expected_baudrate;
#endif
    /* A directly booted PL011 can have an unset divisor. Keep its unknown
     * output rate when normalizing an input-only baudrate request. */
    unsigned int output_encoding = request.c_cflag & KERNEL_CBAUD;
    request.c_cflag = (request.c_cflag & ~KERNEL_CIBAUD)
        | (KERNEL_BOTHER << KERNEL_IBSHIFT);
    request.c_ispeed = expected_baudrate == 4800U ? 9600U : 4800U;
    if (syscall(SYS_ioctl, fd, STARRY_TCSETSW2, &request) != 0
        || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0
        || (expected_baudrate == 0 && output_encoding != KERNEL_BOTHER)
        || observed.c_ospeed != expected_baudrate
        || observed.c_ispeed != expected_baudrate
        || (observed.c_cflag & KERNEL_CBAUD) != output_encoding
        || (observed.c_cflag & KERNEL_CIBAUD) != (output_encoding << KERNEL_IBSHIFT)) {
        fprintf(stderr, "UART baudrate mismatch: expected %u (encoding 0x%x), "
                "observed input %u output %u (c_cflag=0x%x)\n",
                expected_baudrate, output_encoding, observed.c_ispeed,
                observed.c_ospeed, observed.c_cflag);
        fail("physical UART reports a common nominal input and output baudrate");
    }
    if (expected_baudrate != 0) {
        request = observed;
        request.c_cflag = (request.c_cflag & ~(KERNEL_CBAUD | KERNEL_CIBAUD))
            | KERNEL_BOTHER;
        request.c_ispeed = 0;
        request.c_ospeed = 0;
        if (syscall(SYS_ioctl, fd, STARRY_TCSETS2, &request) != 0
            || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0
            || memcmp(&observed, &request, sizeof(observed)) != 0)
            fail("physical UART accepts and reports zero BOTHER");
    }

    if (syscall(SYS_ioctl, fd, STARRY_TCSETSW2, valid) != 0
        || syscall(SYS_ioctl, fd, STARRY_TCGETS2, &observed) != 0
        || memcmp(&observed, valid, sizeof(observed)) != 0)
        fail("restore serial termios after baudrate test");
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
    if (ioctl(fd, STARRY_TCSETS2, &invalid) != -1
        || !is_expected_invalid_baudrate_error(errno))
        fail("TCSETS2 rejects an invalid or unavailable baudrate");
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

static void check_tcsetsf_discards_pending_input(unsigned long request, int on_master)
{
    struct pty_pair pty = {.master = -1, .slave = -1};
    struct kernel_termios2 raw;
    const unsigned char stale = 'S';
    const unsigned char fresh = '\r';
    unsigned char observed = 0;

    if (open_raw_pty(&pty, &raw) != 0) {
        fail("open raw PTY for a termios update with input flush");
        goto out;
    }
    if (write_all(pty.master, &stale, 1) != 0 || !wait_readable(pty.slave)) {
        fail("queue stale input before changing termios");
        goto out;
    }
    raw.c_iflag |= ICRNL;
    if (syscall(SYS_ioctl, on_master ? pty.master : pty.slave, request, &raw) != 0) {
        fail("apply termios with input flush");
        goto out;
    }

    errno = 0;
    if (read(pty.slave, &observed, 1) != -1
        || (errno != EAGAIN && errno != EWOULDBLOCK)) {
        fail("the termios update discards stale input");
        goto out;
    }
    if (write_all(pty.master, &fresh, 1) != 0 || !wait_readable(pty.slave)
        || read(pty.slave, &observed, 1) != 1 || observed != '\n')
        fail("fresh input uses the new ICRNL setting");

out:
    close_pty(&pty);
}

static void check_noncanonical_update_preserves_pending_input(unsigned long request, int on_master)
{
    struct pty_pair pty = {.master = -1, .slave = -1};
    struct kernel_termios2 raw;
    const unsigned char queued = 'A';
    const unsigned char fresh = 'B';
    unsigned char observed = 0;
    int setting_fd;

    if (open_raw_pty(&pty, &raw) != 0) {
        fail("open PTY for canonical mode transition");
        goto out;
    }
    setting_fd = on_master ? pty.master : pty.slave;
    struct kernel_termios2 canonical = raw;
    canonical.c_lflag |= ICANON | ECHO;
    if (syscall(SYS_ioctl, pty.slave, request, &canonical) != 0
        || write_all(pty.master, &queued, 1) != 0
        || !wait_readable(pty.master)
        || syscall(SYS_read, pty.master, &observed, 1) != 1
        || observed != queued) {
        fail("queue and echo a byte without completing the canonical line");
        goto out;
    }

    errno = 0;
    if (syscall(SYS_read, pty.slave, &observed, 1) != -1
        || (errno != EAGAIN && errno != EWOULDBLOCK)) {
        fail("an incomplete canonical line is not readable");
        goto out;
    }
    if (syscall(SYS_ioctl, setting_fd, request, &raw) != 0
        || syscall(SYS_ioctl, pty.slave, request, &canonical) != 0
        || syscall(SYS_read, pty.slave, &observed, 1) != 1
        || observed != queued) {
        fail("pending input is immediately readable after disabling and re-enabling ICANON");
        goto out;
    }
    if (write_all(pty.master, &queued, 1) != 0
        || !wait_readable(pty.master)
        || syscall(SYS_read, pty.master, &observed, 1) != 1
        || observed != queued) {
        fail("queue another incomplete line for the input order check");
        goto out;
    }

    /* Wait until both bytes are readable before checking the order. */
    raw.c_cc[VMIN] = 2;
    unsigned char pair[2];
    if (syscall(SYS_ioctl, setting_fd, request, &raw) != 0
        || write_all(pty.master, &fresh, 1) != 0
        || !wait_readable(pty.slave)
        || syscall(SYS_read, pty.slave, pair, sizeof(pair)) != (long)sizeof(pair)
        || pair[0] != queued || pair[1] != fresh)
        fail("noncanonical reads deliver the pending byte before fresh input");

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
        check_format_only_updates(serial, valid);
        check_serial_reports_common_baudrate(serial, &valid);
        check_failed_serial_config_is_not_published(serial, &valid);
        check_serial_writes_with_waiting_updates(serial, &valid);
        check_serial_hangup_and_restore(serial, &valid);
    }
    if (serial >= 0)
        close(serial);
    check_baudrate_normalization();
    check_tcsetsf_discards_pending_input(TCSETSF, 0);
    check_tcsetsf_discards_pending_input(STARRY_TCSETSF2, 1);
    check_noncanonical_update_preserves_pending_input(TCSETS, 0);
    check_noncanonical_update_preserves_pending_input(STARRY_TCSETS2, 1);

    if (failures != 0) {
        fprintf(stderr, "test-tty-termios-transaction: %d failure(s)\n",
                failures);
        return 1;
    }
    puts("test-tty-termios-transaction: PASS");
    return 0;
}
