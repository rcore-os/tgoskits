#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static void fail(const char *stage)
{
    printf("TEST_NIX_BUILDER_INIT_FAILED stage=%s errno=%d (%s)\n", stage,
           errno, strerror(errno));
    exit(1);
}

static void child_fail(const char *stage)
{
    dprintf(STDERR_FILENO,
            "TEST_NIX_BUILDER_INIT_FAILED stage=%s errno=%d (%s)\n", stage,
            errno, strerror(errno));
    _exit(1);
}

static void timeout_handler(int signo)
{
    (void)signo;
    static const char marker[] =
        "TEST_NIX_BUILDER_INIT_FAILED stage=timeout\n";
    ssize_t ignored = write(STDOUT_FILENO, marker, sizeof(marker) - 1);
    (void)ignored;
    _exit(1);
}

static ssize_t read_line(int fd, char *buffer, size_t capacity)
{
    size_t length = 0;

    while (length + 1 < capacity) {
        fd_set read_fds;
        struct timeval timeout = { .tv_sec = 5, .tv_usec = 0 };
        FD_ZERO(&read_fds);
        FD_SET(fd, &read_fds);
        int ready = select(fd + 1, &read_fds, NULL, NULL, &timeout);
        if (ready == 0) {
            errno = ETIMEDOUT;
            return -1;
        }
        if (ready < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }

        ssize_t count = read(fd, &buffer[length], 1);
        if (count <= 0)
            return count;
        if (buffer[length++] == '\n')
            break;
    }
    buffer[length] = '\0';
    return (ssize_t)length;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, timeout_handler);
    alarm(10);

    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0)
        fail("posix-openpt");
    if (grantpt(master) < 0)
        fail("grantpt");
    if (unlockpt(master) < 0)
        fail("unlockpt");
    char *slave_name = ptsname(master);
    if (slave_name == NULL)
        fail("ptsname");

    pid_t child = fork();
    if (child < 0)
        fail("fork");
    if (child == 0) {
        close(master);
        if (prctl(PR_SET_PDEATHSIG, SIGKILL) < 0)
            child_fail("prctl-pdeathsig");

        int slave = open(slave_name, O_RDWR | O_NOCTTY | O_CLOEXEC);
        if (slave < 0)
            child_fail("open-pty-slave");
        struct termios attributes;
        if (tcgetattr(slave, &attributes) < 0)
            child_fail("tcgetattr");
        cfmakeraw(&attributes);
        if (tcsetattr(slave, TCSANOW, &attributes) < 0)
            child_fail("tcsetattr");
        if (dup2(slave, STDERR_FILENO) < 0)
            child_fail("dup2-stderr");
        if (slave != STDERR_FILENO)
            close(slave);

        struct sigaction action;
        memset(&action, 0, sizeof(action));
        action.sa_handler = SIG_DFL;
        for (int signo = 1; signo < NSIG; signo++) {
            if (signo == SIGKILL || signo == SIGSTOP)
                continue;
            if (sigaction(signo, &action, NULL) < 0 && errno != EINVAL)
                child_fail("restore-signals");
        }
        if (setsid() < 0)
            child_fail("setsid");
        if (dup2(STDERR_FILENO, STDOUT_FILENO) < 0)
            child_fail("dup2-stdout");

        int null_fd = open("/dev/null", O_RDWR | O_CLOEXEC);
        if (null_fd < 0)
            child_fail("open-dev-null");
        if (dup2(null_fd, STDIN_FILENO) < 0)
            child_fail("dup2-stdin");
        if (null_fd != STDIN_FILENO)
            close(null_fd);

        if (syscall(SYS_close_range, 3U, ~0U, 0U) < 0)
            child_fail("close-range");
        struct rlimit core_limit = { .rlim_cur = 0,
                                     .rlim_max = RLIM_INFINITY };
        if (setrlimit(RLIMIT_CORE, &core_limit) < 0)
            child_fail("setrlimit-core");
        umask(0022);
        if (dprintf(STDERR_FILENO, "\x02\n") < 0)
            child_fail("write-handshake");
        execl("/bin/true", "true", NULL);
        child_fail("exec");
    }

    char line[256];
    ssize_t length = read_line(master, line, sizeof(line));
    if (length < 0)
        fail("read-handshake");
    if (length == 0) {
        errno = EPIPE;
        fail("read-handshake-eof");
    }
    if (line[0] != '\x02') {
        printf("TEST_NIX_BUILDER_INIT_FAILED stage=child detail=%s", line);
        return 1;
    }

    int status;
    if (waitpid(child, &status, 0) != child)
        fail("waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("child-status");
    }

    close(master);
    alarm(0);
    puts("TEST_NIX_BUILDER_INIT_PASSED");
    return 0;
}
