#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static int fails;

#define FIRST_PAYLOAD_SIZE 4090
#define SECOND_PAYLOAD_SIZE 257
#define LARGE_PAYLOAD_SIZE (FIRST_PAYLOAD_SIZE + SECOND_PAYLOAD_SIZE)

static void pass(const char *msg)
{
    printf("  PASS: %s\n", msg);
}

static void fail(const char *msg)
{
    printf("  FAIL: %s (errno=%d: %s)\n", msg, errno, strerror(errno));
    fails++;
}

static ssize_t read_line_timeout(int fd, char *buf, size_t len, int timeout_ms)
{
    size_t off = 0;
    while (off + 1 < len) {
        struct pollfd pfd = {
            .fd = fd,
            .events = POLLIN,
            .revents = 0,
        };
        int rc = poll(&pfd, 1, timeout_ms);
        if (rc <= 0) {
            return rc;
        }
        char ch = 0;
        ssize_t n = read(fd, &ch, 1);
        if (n <= 0) {
            return n;
        }
        buf[off++] = ch;
        if (ch == '\n') {
            break;
        }
    }
    buf[off] = '\0';
    return (ssize_t)off;
}

static unsigned char payload_byte(size_t index)
{
    return (unsigned char)(index % 251 + 1);
}

static void check_large_payload_with_partial_reads(void)
{
    int ready_pipe[2];
    int proceed_pipe[2];
    if (pipe(ready_pipe) != 0 || pipe(proceed_pipe) != 0) {
        fail("partial-read synchronization pipes");
        return;
    }

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("partial-read posix_openpt");
        return;
    }
    if (grantpt(master) != 0 || unlockpt(master) != 0) {
        fail("partial-read grantpt/unlockpt");
        close(master);
        return;
    }
    char *slave_name = ptsname(master);
    if (slave_name == NULL) {
        fail("partial-read ptsname");
        close(master);
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        fail("partial-read fork");
        close(master);
        return;
    }
    if (pid == 0) {
        close(ready_pipe[0]);
        close(proceed_pipe[1]);
        int slave = open(slave_name, O_RDWR | O_NOCTTY);
        if (slave < 0) {
            _exit(121);
        }
        struct termios term;
        if (tcgetattr(slave, &term) != 0) {
            _exit(122);
        }
        cfmakeraw(&term);
        if (tcsetattr(slave, TCSANOW, &term) != 0) {
            _exit(123);
        }

        unsigned char chunk[FIRST_PAYLOAD_SIZE];
        size_t written = 0;
        for (size_t i = 0; i < sizeof(chunk); i++) {
            chunk[i] = payload_byte(i);
        }
        if (write(slave, chunk, sizeof(chunk)) != (ssize_t)sizeof(chunk)) {
            _exit(124);
        }
        if (write(ready_pipe[1], "R", 1) != 1) {
            _exit(125);
        }
        char command;
        if (read(proceed_pipe[0], &command, 1) != 1) {
            _exit(126);
        }

        written = FIRST_PAYLOAD_SIZE;
        for (size_t i = 0; i < SECOND_PAYLOAD_SIZE; i++) {
            chunk[i] = payload_byte(written + i);
        }
        if (write(slave, chunk, SECOND_PAYLOAD_SIZE) != SECOND_PAYLOAD_SIZE) {
            _exit(127);
        }
        close(slave);
        _exit(0);
    }

    close(ready_pipe[1]);
    close(proceed_pipe[0]);
    char ready;
    if (read(ready_pipe[0], &ready, 1) != 1) {
        fail("partial-read child prepares first payload");
        goto out_wait_partial_read;
    }

    size_t received = 0;
    unsigned char chunk[17];
    ssize_t first_read = read(master, chunk, sizeof(chunk));
    if (first_read != (ssize_t)sizeof(chunk)) {
        fail("partial-read master partially consumes first payload");
        goto out_wait_partial_read;
    }
    for (ssize_t i = 0; i < first_read; i++) {
        if (chunk[i] != payload_byte((size_t)i)) {
            fail("partial-read first payload remains ordered");
            goto out_wait_partial_read;
        }
    }
    received = (size_t)first_read;
    if (write(proceed_pipe[1], "C", 1) != 1) {
        fail("partial-read releases second payload");
        goto out_wait_partial_read;
    }

    int payload_ok = 1;
    while (received < LARGE_PAYLOAD_SIZE) {
        ssize_t n = read(master, chunk, sizeof(chunk));
        if (n <= 0) {
            fail("partial-read master receives complete payload");
            payload_ok = 0;
            break;
        }
        for (ssize_t i = 0; i < n; i++) {
            if (chunk[i] != payload_byte(received + (size_t)i)) {
                fail("partial-read payload remains ordered and complete");
                payload_ok = 0;
                break;
            }
        }
        if (!payload_ok) {
            break;
        }
        received += (size_t)n;
    }
    if (payload_ok && received == LARGE_PAYLOAD_SIZE) {
        pass("partial reads preserve payload larger than tty ring buffer");
    }

out_wait_partial_read:
    close(ready_pipe[0]);
    close(proceed_pipe[1]);
    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        fail("partial-read child writes and exits cleanly");
    } else {
        pass("partial-read child writes and exits cleanly");
    }
    close(master);
}

static void check_data_before_eof_without_poll(void)
{
    static const char payload[] = "DATA_BEFORE_EOF";
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("no-poll posix_openpt");
        return;
    }
    if (grantpt(master) != 0 || unlockpt(master) != 0) {
        fail("no-poll grantpt/unlockpt");
        close(master);
        return;
    }
    char *slave_name = ptsname(master);
    if (slave_name == NULL) {
        fail("no-poll ptsname");
        close(master);
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        fail("no-poll fork");
        close(master);
        return;
    }
    if (pid == 0) {
        int slave = open(slave_name, O_RDWR | O_NOCTTY);
        if (slave < 0) {
            _exit(111);
        }
        if (write(slave, payload, sizeof(payload) - 1) !=
            (ssize_t)(sizeof(payload) - 1)) {
            _exit(112);
        }
        close(slave);
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        fail("no-poll child writes and exits cleanly");
        close(master);
        return;
    }
    pass("no-poll child writes and exits cleanly");

    char buf[sizeof(payload)] = {0};
    errno = 0;
    ssize_t n = read(master, buf, sizeof(buf) - 1);
    if (n != (ssize_t)(sizeof(payload) - 1) ||
        memcmp(buf, payload, sizeof(payload) - 1) != 0) {
        fail("master drains queued data before EOF without poll");
        close(master);
        return;
    }
    pass("master drains queued data before EOF without poll");

    errno = 0;
    n = read(master, buf, sizeof(buf) - 1);
    if (n == 0 || (n < 0 && errno == EIO)) {
        pass("master reports EOF only after queued data drains");
    } else {
        fail("master reports EOF only after queued data drains");
    }
    close(master);
}

static void check_nix_like_child(void)
{
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("nix-like posix_openpt");
        return;
    }
    if (grantpt(master) != 0 || unlockpt(master) != 0) {
        fail("nix-like grantpt/unlockpt");
        close(master);
        return;
    }
    char *slave_name = ptsname(master);
    if (slave_name == NULL) {
        fail("nix-like ptsname");
        close(master);
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        fail("nix-like fork");
        close(master);
        return;
    }
    if (pid == 0) {
        int slave = open(slave_name, O_RDWR | O_NOCTTY);
        if (slave < 0) {
            _exit(101);
        }
        struct termios term;
        if (tcgetattr(slave, &term) != 0) {
            _exit(102);
        }
        cfmakeraw(&term);
        if (tcsetattr(slave, TCSANOW, &term) != 0) {
            _exit(103);
        }
        if (dup2(slave, STDERR_FILENO) < 0) {
            _exit(104);
        }
        close(slave);
        if (setsid() < 0) {
            _exit(105);
        }
        if (dup2(STDERR_FILENO, STDOUT_FILENO) < 0) {
            _exit(106);
        }
        int null_fd = open("/dev/null", O_RDWR);
        if (null_fd < 0 || dup2(null_fd, STDIN_FILENO) < 0) {
            _exit(107);
        }
        close(null_fd);
        if (write(STDERR_FILENO, "\002\n", 2) != 2) {
            _exit(108);
        }
        const char setup_log[] =
            "SETUP_LOG_BEGIN "
            "abcdefghijklmnopqrstuvwxyz0123456789 "
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 "
            "pty-buffer-must-not-drop-bytes-after-sentinel\n";
        if (write(STDERR_FILENO, setup_log, sizeof(setup_log) - 1) !=
            (ssize_t)(sizeof(setup_log) - 1)) {
            _exit(109);
        }
        execl("/bin/sh", "sh", "-c", "echo CHILD_OK >&2", (char *)NULL);
        _exit(110);
    }

    char line[256];
    ssize_t n = read_line_timeout(master, line, sizeof(line), 1000);
    if (n <= 0 || line[0] != '\002') {
        fail("nix-like parent reads setup sentinel");
        goto out_wait;
    }
    pass("nix-like parent reads setup sentinel");

    n = read_line_timeout(master, line, sizeof(line), 1000);
    if (n <= 0 || strstr(line, "pty-buffer-must-not-drop-bytes-after-sentinel") == NULL) {
        fail("nix-like parent reads long setup log after sentinel");
        goto out_wait;
    }
    pass("nix-like parent reads long setup log after sentinel");

    n = read_line_timeout(master, line, sizeof(line), 5000);
    if (n <= 0 || strstr(line, "CHILD_OK") == NULL) {
        fail("nix-like parent reads child stderr after sentinel");
        goto out_wait;
    }
    pass("nix-like parent reads child stderr after sentinel");

out_wait:
    {
        int status = 0;
        waitpid(pid, &status, 0);
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            fail("nix-like child exits cleanly");
        } else {
            pass("nix-like child exits cleanly");
        }
    }
    close(master);
}

int main(void)
{
    printf("=== pty-master-close regression ===\n");

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("posix_openpt");
        goto out;
    }
    pass("posix_openpt");

    if (grantpt(master) != 0) {
        fail("grantpt");
        goto out_master;
    }
    pass("grantpt");

    if (unlockpt(master) != 0) {
        fail("unlockpt");
        goto out_master;
    }
    pass("unlockpt");

    char *slave_name = ptsname(master);
    if (slave_name == NULL) {
        fail("ptsname");
        goto out_master;
    }
    pass("ptsname");

    int slave = open(slave_name, O_RDWR | O_NOCTTY);
    if (slave < 0) {
        fail("open slave");
        goto out_master;
    }
    pass("open slave");

    int slave_dup = dup(slave);
    if (slave_dup < 0) {
        fail("dup slave");
        goto out_slave;
    }
    pass("dup slave");

    if (close(slave) != 0) {
        fail("close slave");
        goto out_slave_dup;
    }
    slave = -1;
    pass("close one slave fd");

    struct pollfd pfd = {
        .fd = master,
        .events = POLLIN,
        .revents = 0,
    };
    int poll_rc = poll(&pfd, 1, 100);
    if (poll_rc != 0) {
        fail("master must not report close while dup slave is still open");
        goto out_slave_dup;
    }
    pass("master stays pending while dup slave is open");

    const char setup_done[] = "\002\n";
    ssize_t written = write(slave_dup, setup_done, sizeof(setup_done) - 1);
    if (written != (ssize_t)(sizeof(setup_done) - 1)) {
        fail("write setup sentinel through dup slave");
        goto out_slave_dup;
    }
    pass("write setup sentinel through dup slave");

    pfd.revents = 0;
    poll_rc = poll(&pfd, 1, 1000);
    if (poll_rc != 1 || (pfd.revents & POLLIN) == 0) {
        fail("master poll reports sentinel data");
        goto out_slave_dup;
    }
    pass("master poll reports sentinel data");

    char setup_line[8] = {0};
    ssize_t setup_read = read_line_timeout(master, setup_line, sizeof(setup_line), 1000);
    if (setup_read <= 0 || setup_line[0] != '\002') {
        fail("master reads setup sentinel line");
        goto out_slave_dup;
    }
    pass("master reads setup sentinel line");

    if (close(slave_dup) != 0) {
        fail("close dup slave");
        goto out_master;
    }
    slave_dup = -1;
    pass("close last slave fd");

    pfd.revents = 0;
    poll_rc = poll(&pfd, 1, 1000);
    if (poll_rc != 1) {
        fail("master poll returns after slave close");
        goto out_master;
    }
    pass("master poll returns after slave close");

    if ((pfd.revents & (POLLIN | POLLHUP | POLLERR)) == 0) {
        fail("master poll reports close readiness");
        goto out_master;
    }
    printf("  INFO: master revents=0x%x\n", pfd.revents);
    pass("master poll reports close readiness");

    char byte = 0;
    errno = 0;
    ssize_t n = read(master, &byte, 1);
    if (n == 0 || (n < 0 && errno == EIO)) {
        pass("master read completes after slave close");
    } else {
        fail("master read should complete with EOF or EIO after slave close");
    }

    check_nix_like_child();
    check_data_before_eof_without_poll();
    check_large_payload_with_partial_reads();

out_slave_dup:
    if (slave_dup >= 0) {
        close(slave_dup);
    }
out_slave:
    if (slave >= 0) {
        close(slave);
    }
out_master:
    close(master);
out:
    printf("\n=== Results: %s ===\n", fails == 0 ? "pass" : "fail");
    if (fails == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
