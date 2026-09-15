#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static void fail(const char *message)
{
    fprintf(stderr, "FAIL: %s (errno=%d)\n", message, errno);
    exit(1);
}

static void write_all(int fd, const char *bytes, size_t length)
{
    while (length != 0) {
        ssize_t n = write(fd, bytes, length);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            fail("write terminal bytes");
        bytes += n;
        length -= (size_t)n;
    }
}

static void read_exact(int fd, char *bytes, size_t length)
{
    while (length != 0) {
        struct pollfd pfd = {.fd = fd, .events = POLLIN};
        int ready = poll(&pfd, 1, 2000);
        if (ready < 0 && errno == EINTR)
            continue;
        if (ready <= 0 || !(pfd.revents & POLLIN))
            fail("terminal bytes did not arrive");
        ssize_t n = read(fd, bytes, length);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            fail("read terminal bytes");
        bytes += n;
        length -= (size_t)n;
    }
}

static void open_terminal(int *master, int *slave)
{
    *master = posix_openpt(O_RDWR | O_NOCTTY);
    if (*master < 0 || grantpt(*master) != 0 || unlockpt(*master) != 0)
        fail("create PTY");
    char *name = ptsname(*master);
    if (name == NULL || (*slave = open(name, O_RDWR | O_NOCTTY)) < 0)
        fail("open PTY slave");
    struct termios raw;
    if (tcgetattr(*slave, &raw) != 0)
        fail("read termios");
    cfmakeraw(&raw);
    if (tcsetattr(*slave, TCSANOW, &raw) != 0)
        fail("set raw mode");
}

static void check_query_transport(int master, int slave)
{
    const char query[] = "before\033[6nafter";
    const char reply[] = "\033[24;80R";
    char observed[sizeof(query) - 1];
    write_all(slave, query, sizeof(query) - 1);
    struct pollfd pfd = {.fd = slave, .events = POLLIN};
    if (poll(&pfd, 1, 0) != 0)
        fail("TTY fabricated input before terminal replied");
    read_exact(master, observed, sizeof(observed));
    if (memcmp(observed, query, sizeof(observed)) != 0)
        fail("TTY changed the cursor query");

    /* Splitting writes must not change the terminal byte stream. */
    write_all(slave, "\033[", 2);
    write_all(slave, "6n", 2);
    char split[4];
    read_exact(master, split, sizeof(split));
    if (memcmp(split, "\033[6n", sizeof(split)) != 0)
        fail("TTY changed a split cursor query");

    write_all(master, reply, sizeof(reply) - 1);
    char response[sizeof(reply) - 1];
    read_exact(slave, response, sizeof(response));
    if (memcmp(response, reply, sizeof(response)) != 0)
        fail("TTY changed terminal input");
    puts("TTY_QUERY_TRANSPORT_OK");
}

static void check_resize(int master, int slave)
{
    int output[2];
    if (pipe(output) != 0)
        fail("create resize output pipe");
    pid_t child = fork();
    if (child < 0)
        fail("fork resize");
    if (child == 0) {
        close(master);
        close(output[0]);
        if (setsid() < 0 || ioctl(slave, TIOCSCTTY, 0) != 0
            || dup2(slave, STDIN_FILENO) < 0
            || dup2(slave, STDERR_FILENO) < 0
            || dup2(output[1], STDOUT_FILENO) < 0)
            _exit(2);
        close(slave);
        close(output[1]);
        execl("/bin/busybox", "busybox", "resize", (char *)NULL);
        _exit(127);
    }
    close(output[1]);
    const char query[] = "\0337\033[r\033[999;999H\033[6n";
    char observed[sizeof(query) - 1];
    read_exact(master, observed, sizeof(observed));
    if (memcmp(observed, query, sizeof(observed)) != 0)
        fail("resize query did not reach terminal");
    write_all(master, "\033[24;80R", 8);
    const char expected[] = "COLUMNS=80;LINES=24;export COLUMNS LINES;\n";
    char result[sizeof(expected) - 1];
    read_exact(output[0], result, sizeof(result));
    if (memcmp(result, expected, sizeof(result)) != 0)
        fail("resize reported incorrect dimensions");
    close(output[0]);
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0)
        fail("resize failed");
    struct winsize size;
    if (ioctl(slave, TIOCGWINSZ, &size) != 0
        || size.ws_row != 24 || size.ws_col != 80)
        fail("resize did not update window size");
    puts("TTY_RESIZE_OK");
}

int main(void)
{
    alarm(15);
    int master, slave;
    open_terminal(&master, &slave);
    check_query_transport(master, slave);
    check_resize(master, slave);
    close(slave);
    close(master);
    puts("TTY_CURSOR_REPORT_PASSED");
    return 0;
}
