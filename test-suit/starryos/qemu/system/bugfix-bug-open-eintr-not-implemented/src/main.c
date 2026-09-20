/* FIFO opens must rendezvous with their peer and support signal cancellation. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *fifo = "/tmp/bug_eintr_fifo";
static volatile sig_atomic_t alrm_fired;
static volatile sig_atomic_t signal_ready_fd = -1;

static void alrm_handler(int sig)
{
    (void)sig;
    int saved_errno = errno;
    if (!alrm_fired && signal_ready_fd >= 0) {
        char byte = 's';
        if (write(signal_ready_fd, &byte, 1) != 1)
            _exit(1);
    }
    alrm_fired = 1;
    errno = saved_errno;
}

static void require(int condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "FAIL: %s (errno=%d: %s)\n", message, errno, strerror(errno));
        exit(1);
    }
}

static int fifo_open(int flags)
{
    return syscall(SYS_openat, AT_FDCWD, fifo, flags, 0);
}

static void interrupted_open(int flags)
{
    /* Repeating delivery also covers a signal arriving just before open. */
    struct itimerval timer = {
        .it_interval = {.tv_usec = 100000},
        .it_value = {.tv_usec = 100000},
    };
    alrm_fired = 0;
    require(setitimer(ITIMER_REAL, &timer, NULL) == 0, "arm signal timer");
    errno = 0;
    int fd = fifo_open(flags);
    int error = errno;
    timer = (struct itimerval){0};
    require(setitimer(ITIMER_REAL, &timer, NULL) == 0, "disarm signal timer");
    if (fd >= 0)
        close(fd);
    if (fd != -1 || error != EINTR || !alrm_fired) {
        fprintf(stderr, "FAIL: blocking FIFO flags=%d expected EINTR, fd=%d errno=%d signal=%d\n",
                flags, fd, error, alrm_fired);
        exit(1);
    }
    printf("PASS: blocking FIFO flags=%d interrupted with EINTR\n", flags);
}

static void no_reader(void)
{
    errno = 0;
    int fd = fifo_open(O_WRONLY | O_NONBLOCK);
    int error = errno;
    if (fd >= 0)
        close(fd);
    require(fd == -1 && error == ENXIO, "nonblocking writer without reader returns ENXIO");
}

static void nonblocking_pair(void)
{
    int reader = fifo_open(O_RDONLY | O_NONBLOCK);
    require(reader >= 0, "nonblocking reader opens without writer");
    struct stat st;
    require(fstat(reader, &st) == 0 && S_ISFIFO(st.st_mode), "fstat preserves FIFO type");
    struct statfs by_fd, by_path;
    require(syscall(SYS_fstatfs, reader, &by_fd) == 0 &&
            syscall(SYS_statfs, fifo, &by_path) == 0 && by_fd.f_type == by_path.f_type,
            "fstatfs preserves backing filesystem type");
    require(syscall(SYS_fchmod, reader, 0640) == 0 &&
            fstat(reader, &st) == 0 && (st.st_mode & 0777) == 0640,
            "fchmod updates FIFO permissions");
    char fd_path[64];
    snprintf(fd_path, sizeof(fd_path), "/proc/self/fd/%d", reader);
    int reopened = syscall(SYS_openat, AT_FDCWD, fd_path, O_RDONLY | O_NONBLOCK, 0);
    require(reopened >= 0, "reopen FIFO through proc fd link");
    require(syscall(SYS_fcntl, reopened, F_SETFL, 0) == 0 &&
            (syscall(SYS_fcntl, reader, F_GETFL) & O_NONBLOCK),
            "reopened FIFO has independent status flags");
    close(reopened);
    require(syscall(SYS_fcntl, reader, F_SETFL, O_NONBLOCK | O_APPEND) == 0 &&
            (syscall(SYS_fcntl, reader, F_GETFL) & O_APPEND),
            "FIFO retains O_APPEND status flag");
    require(syscall(SYS_fcntl, reader, F_SETFL, O_NONBLOCK) == 0 &&
            !(syscall(SYS_fcntl, reader, F_GETFL) & O_APPEND),
            "FIFO clears O_APPEND status flag");
    errno = 0;
    require(syscall(SYS_lseek, reader, 0, SEEK_SET) == -1 && errno == ESPIPE, "FIFO is not seekable");
    struct pollfd event = {.fd = reader, .events = POLLIN};
    require(poll(&event, 1, 0) == 0, "reader has no HUP before first writer");
    char byte = 0;
    require(syscall(SYS_read, reader, &byte, 1) == 0, "reader without writer observes EOF");
    int notify = syscall(SYS_inotify_init1, IN_NONBLOCK);
    require(notify >= 0, "create FIFO notification instance");
    int watch = syscall(SYS_inotify_add_watch, notify, fifo, IN_MODIFY | IN_CLOSE_WRITE);
    require(watch >= 0, "watch FIFO writes and writer close");
    int writer = fifo_open(O_WRONLY | O_NONBLOCK);
    require(writer >= 0, "nonblocking writer opens with reader");
    require(syscall(SYS_write, writer, "x", 1) == 1, "write through FIFO");
    require(syscall(SYS_read, reader, &byte, 1) == 1 && byte == 'x', "read peer data through FIFO");
    close(writer);
    _Alignas(struct inotify_event) char events[4096];
    ssize_t count = syscall(SYS_read, notify, events, sizeof(events));
    require(count > 0, "FIFO produces inotify events");
    uint32_t observed = 0;
    for (size_t offset = 0; offset < (size_t)count;) {
        require((size_t)count - offset >= sizeof(struct inotify_event), "complete inotify event header");
        struct inotify_event entry;
        memcpy(&entry, events + offset, sizeof(entry));
        size_t size = sizeof(entry) + entry.len;
        require(size <= (size_t)count - offset, "complete inotify event payload");
        if (entry.wd == watch)
            observed |= entry.mask;
        offset += size;
    }
    require((observed & (IN_MODIFY | IN_CLOSE_WRITE)) == (IN_MODIFY | IN_CLOSE_WRITE),
            "FIFO preserves IN_MODIFY and IN_CLOSE_WRITE notifications");
    close(notify);
    event.revents = 0;
    require(poll(&event, 1, 0) == 1 && (event.revents & POLLHUP), "last writer close reports HUP");
    require(syscall(SYS_read, reader, &byte, 1) == 0, "last writer close restores EOF");
    close(reader);
    no_reader();
    puts("PASS: nonblocking FIFO endpoints transfer data and close cleanly");
}

static void blocking_pair(void)
{
    fflush(NULL);
    pid_t child = fork();
    require(child >= 0, "fork FIFO reader");
    if (child == 0) {
        alarm(5);
        int reader = fifo_open(O_RDONLY);
        char byte = 0;
        int ok = reader >= 0 && syscall(SYS_read, reader, &byte, 1) == 1 && byte == 'y';
        if (reader >= 0)
            close(reader);
        _exit(ok ? 0 : 1);
    }
    alarm(5);
    int writer = fifo_open(O_WRONLY);
    require(writer >= 0, "blocking writer rendezvous with reader");
    require(syscall(SYS_write, writer, "y", 1) == 1, "blocking pair transfers data");
    close(writer);
    int status = 0;
    require(waitpid(child, &status, 0) == child, "wait for FIFO reader");
    alarm(0);
    require(WIFEXITED(status) && WEXITSTATUS(status) == 0, "blocking reader received peer data");
    no_reader();
    puts("PASS: blocking FIFO opens rendezvous across processes");
}

static void restarted_open(void)
{
    int ready[2];
    require(pipe(ready) == 0, "create signal notification pipe");
    struct sigaction sa = {.sa_handler = alrm_handler, .sa_flags = SA_RESTART};
    sigemptyset(&sa.sa_mask);
    require(sigaction(SIGALRM, &sa, NULL) == 0, "install restarting signal handler");
    fflush(NULL);
    pid_t child = fork();
    require(child >= 0, "fork restarting-open writer");
    if (child == 0) {
        close(ready[1]);
        char byte = 0;
        if (read(ready[0], &byte, 1) != 1 || byte != 's')
            _exit(1);
        close(ready[0]);
        int writer = fifo_open(O_WRONLY);
        int ok = writer >= 0 && syscall(SYS_write, writer, "r", 1) == 1;
        if (writer >= 0)
            close(writer);
        _exit(ok ? 0 : 1);
    }
    close(ready[0]);
    signal_ready_fd = ready[1];
    alrm_fired = 0;
    /* The writer opens only after the signal handler explicitly releases it. */
    alarm(1);
    int reader = fifo_open(O_RDONLY);
    alarm(0);
    signal_ready_fd = -1;
    close(ready[1]);
    require(reader >= 0 && alrm_fired, "SA_RESTART resumes FIFO open after signal");
    char byte = 0;
    require(syscall(SYS_read, reader, &byte, 1) == 1 && byte == 'r', "restarted open receives peer data");
    close(reader);
    int status = 0;
    require(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
            "restarted-open writer completed");
    sa.sa_flags = 0;
    require(sigaction(SIGALRM, &sa, NULL) == 0, "restore non-restarting signal handler");
    no_reader();
    puts("PASS: SA_RESTART resumes FIFO open until its peer arrives");
}

int fifo_boundary_tests(void);

int main(void)
{
    unlink(fifo);
    require(mkfifo(fifo, 0600) == 0, "create FIFO");
    struct sigaction sa = {.sa_handler = alrm_handler};
    sigemptyset(&sa.sa_mask);
    require(sigaction(SIGALRM, &sa, NULL) == 0, "install handler without SA_RESTART");

    int path = fifo_open(O_PATH);
    require(path >= 0, "O_PATH opens FIFO without a peer");
    close(path);
    interrupted_open(O_RDONLY);
    no_reader();
    interrupted_open(O_WRONLY);
    no_reader();
    nonblocking_pair();
    blocking_pair();
    restarted_open();
    alarm(5);
    int both = fifo_open(O_RDWR);
    alarm(0);
    require(both >= 0, "read-write FIFO opens without a peer");
    close(both);
    no_reader();
    require(unlink(fifo) == 0, "remove FIFO");
    puts("PASS: FIFO open interruption and endpoint lifetime");
    return fifo_boundary_tests();
}
