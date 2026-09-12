#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static char directory[] = "/tmp/exec-write-XXXXXX";
static char executable[PATH_MAX], alias[PATH_MAX], invalid[PATH_MAX];
static char original[PATH_MAX];
static int owns_directory;

static void cleanup(void)
{
    if (owns_directory) {
        unlink(executable);
        unlink(alias);
        unlink(invalid);
        rmdir(directory);
    }
}

#define CHECK(condition, message) do { \
    if (!(condition)) { \
        fprintf(stderr, "FAIL: %s (line %d, errno=%d: %s)\n", \
                message, __LINE__, errno, strerror(errno)); \
        exit(1); \
    } \
} while (0)

static int open_file(const char *path, int flags)
{
    return (int)syscall(SYS_openat, AT_FDCWD, path, flags, 0700);
}

static void send_byte(int fd, char value)
{
    CHECK(write(fd, &value, 1) == 1, "pipe notification");
}

static char receive_byte(int fd)
{
    char value;
    CHECK(read(fd, &value, 1) == 1, "pipe acknowledgement");
    return value;
}

static void wait_success(pid_t pid)
{
    int status;
    CHECK(waitpid(pid, &status, 0) == pid, "wait for child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child completed successfully");
}

static void exec_helper(const char *path, const char *mode, int input,
                        int output, pid_t child)
{
    char in[24], out[24], pid[24];
    snprintf(in, sizeof(in), "%d", input);
    snprintf(out, sizeof(out), "%d", output);
    snprintf(pid, sizeof(pid), "%d", child);
    char *args[] = {(char *)path, (char *)mode, in, out, original, pid, NULL};
    char *env[] = {NULL};
    syscall(SYS_execve, path, args, env);
    CHECK(0, "exec helper");
}

/* A pipe handshake observes the new image; no scheduling delays are needed. */
static int helper(int argc, char **argv)
{
    CHECK(argc == 6, "helper arguments");
    int input = atoi(argv[2]), output = atoi(argv[3]);
    snprintf(original, sizeof(original), "%s", argv[4]);
    if (strcmp(argv[1], "--reap") == 0) {
        send_byte(output, 'E');
        wait_success((pid_t)atoi(argv[5]));
        send_byte(output, 'D');
        CHECK(receive_byte(input) == 'q', "reaper exit command");
        return 0;
    }
    send_byte(output, 'R');
    char command = receive_byte(input);
    if (command == 'e') {
        exec_helper(original, "--hold", input, output, 0);
    } else if (command == 'f') {
        pid_t child = fork();
        CHECK(child >= 0, "fork executable image");
        if (child == 0) {
            send_byte(output, 'F');
            CHECK(receive_byte(input) == 'q', "forked image exit command");
            return 0;
        }
        /* Only the forked child retains the copied executable after this exec. */
        exec_helper(original, "--reap", input, output, child);
    }
    CHECK(command == 'q', "helper exit command");
    return 0;
}

struct process {
    pid_t pid;
    int command, event;
};

static struct process start_process(const char *path)
{
    int commands[2], events[2];
    CHECK(pipe(commands) == 0 && pipe(events) == 0, "create synchronization pipes");
    pid_t child = fork();
    CHECK(child >= 0, "fork helper");
    if (child == 0) {
        owns_directory = 0;
        close(commands[1]);
        close(events[0]);
        if (path) {
            exec_helper(path, "--hold", commands[0], events[1], 0);
        } else {
            /* Retain inherited writable descriptions without exec. */
            send_byte(events[1], 'R');
            CHECK(receive_byte(commands[0]) == 'q', "writer exit command");
            _exit(0);
        }
    }
    close(commands[0]);
    close(events[1]);
    CHECK(receive_byte(events[0]) == 'R', "helper ready");
    return (struct process){child, commands[1], events[0]};
}

static void stop_process(struct process process)
{
    send_byte(process.command, 'q');
    wait_success(process.pid);
    close(process.command);
    close(process.event);
}

static void expect_open_error(const char *path, int flags, int error)
{
    errno = 0;
    int fd = open_file(path, flags);
    int actual = errno;
    if (fd >= 0)
        close(fd);
    CHECK(fd == -1 && actual == error, "open returns expected error");
}

static void expect_writable(const char *path)
{
    int fd = open_file(path, O_WRONLY);
    CHECK(fd >= 0, "released executable is writable");
    CHECK(close(fd) == 0, "close writable file");
}

static void expect_exec_error(const char *path, int error)
{
    pid_t child = fork();
    CHECK(child >= 0, "fork failed-exec probe");
    if (child == 0) {
        owns_directory = 0;
        char *args[] = {(char *)path, "--unexpected-exec", NULL};
        char *env[] = {NULL};
        errno = 0;
        long result = syscall(SYS_execve, path, args, env);
        CHECK(result == -1 && errno == error, "exec returns expected error");
        if (error == ENOEXEC)
            expect_writable(path);
        _exit(0);
    }
    wait_success(child);
}

static void copy_self(void)
{
    int source = open_file("/proc/self/exe", O_RDONLY);
    int target = open_file(executable, O_CREAT | O_EXCL | O_WRONLY);
    CHECK(source >= 0 && target >= 0, "open executable copy");
    char buffer[16384];
    ssize_t count;
    while ((count = read(source, buffer, sizeof(buffer))) > 0) {
        ssize_t written = 0;
        while (written < count) {
            ssize_t size = write(target, buffer + written, (size_t)(count - written));
            CHECK(size > 0, "write executable copy");
            written += size;
        }
    }
    CHECK(count == 0, "read executable copy");
    CHECK(close(source) == 0 && close(target) == 0, "close executable copy");
}

static void check_running_image(void)
{
    struct process running = start_process(executable);
    CHECK(link(executable, alias) == 0, "create executable hard link");
    expect_open_error(alias, O_WRONLY, ETXTBSY);
    CHECK(unlink(executable) == 0 && rename(alias, executable) == 0,
          "rename running executable alias");
    expect_open_error(executable, O_RDWR, ETXTBSY);
    struct stat before, after;
    CHECK(stat(executable, &before) == 0, "stat running executable");
    expect_open_error(executable, O_RDONLY | O_TRUNC, ETXTBSY);
    errno = 0;
    CHECK(syscall(SYS_truncate, executable, 0) == -1 && errno == ETXTBSY,
          "truncate running executable returns ETXTBSY");
    CHECK(stat(executable, &after) == 0 && before.st_size == after.st_size,
          "rejected truncation preserves executable size");
    stop_process(running);
    expect_writable(executable);

    running = start_process(executable);
    send_byte(running.command, 'e');
    CHECK(receive_byte(running.event) == 'R', "replacement image ready");
    expect_writable(executable);
    stop_process(running);

    running = start_process(executable);
    send_byte(running.command, 'f');
    char first = receive_byte(running.event), second = receive_byte(running.event);
    CHECK((first == 'E' && second == 'F') || (first == 'F' && second == 'E'),
          "forked image ready and parent replaced");
    expect_open_error(executable, O_WRONLY, ETXTBSY);
    send_byte(running.command, 'q');
    CHECK(receive_byte(running.event) == 'D', "forked image exited and reaped");
    expect_writable(executable);
    stop_process(running);
}

static void check_writer_lifetime(void)
{
    int writer = open_file(executable, O_WRONLY);
    CHECK(writer >= 0, "open writer before exec");
    int duplicate = dup(writer);
    CHECK(duplicate >= 0 && close(writer) == 0, "duplicate retains writer");
    expect_exec_error(executable, ETXTBSY);
    struct process inherited = start_process(NULL);
    CHECK(close(duplicate) == 0, "parent releases last writable descriptor");
    expect_exec_error(executable, ETXTBSY);
    stop_process(inherited);
    puts("checking release after inherited writer exit");
    struct process running = start_process(executable);
    stop_process(running);

    writer = open_file(executable, O_RDWR);
    CHECK(writer >= 0, "open writable mapping backing");
    size_t length = (size_t)sysconf(_SC_PAGESIZE);
    void *mapping = mmap(NULL, length, PROT_READ, MAP_PRIVATE, writer, 0);
    CHECK(mapping != MAP_FAILED && close(writer) == 0, "mapping retains writable description");
    expect_exec_error(executable, ETXTBSY);
    inherited = start_process(NULL);
    CHECK(munmap(mapping, length) == 0, "release parent writable description mapping");
    expect_exec_error(executable, ETXTBSY);
    stop_process(inherited);
    puts("checking release after inherited mapping exit");
    /* wait must observe release even if physical MM reclaim is deferred. */
    running = start_process(executable);
    stop_process(running);
}

int main(int argc, char **argv)
{
    if (argc > 1)
        return helper(argc, argv);
    expect_open_error("/proc/self/exe", O_WRONLY, ETXTBSY);
    puts("PASS: open(/proc/self/exe, O_WRONLY) -> -1 ETXTBSY");
    expect_open_error("/proc/self/exe", O_RDWR, ETXTBSY);
    int reader = open_file("/proc/self/exe", O_RDONLY);
    int path = open_file("/proc/self/exe", O_PATH | O_RDWR | O_TRUNC);
    CHECK(reader >= 0 && path >= 0, "read and O_PATH remain allowed");
    close(reader);
    close(path);
    ssize_t size = readlink("/proc/self/exe", original, sizeof(original) - 1);
    CHECK(size > 0 && (size_t)size < sizeof(original) - 1, "resolve original executable");
    original[size] = '\0';
    CHECK(mkdtemp(directory) != NULL, "create isolated fixture directory");
    owns_directory = 1;
    CHECK(atexit(cleanup) == 0, "register fixture cleanup");
    snprintf(executable, sizeof(executable), "%s/image", directory);
    snprintf(alias, sizeof(alias), "%s/alias", directory);
    snprintf(invalid, sizeof(invalid), "%s/invalid", directory);
    copy_self();
    check_running_image();
    puts("PASS: running image aliases, truncation, fork, exit and replacement");
    check_writer_lifetime();
    puts("PASS: writable descriptions survive dup, fork and mappings");
    int fd = open_file(invalid, O_WRONLY | O_CREAT | O_EXCL);
    CHECK(fd >= 0 && write(fd, "invalid", 7) == 7 && close(fd) == 0,
          "prepare invalid executable");
    expect_exec_error(invalid, ENOEXEC);
    expect_writable(invalid);
    puts("PASS: executable/write exclusion follows inode and resource lifetimes");
    return 0;
}
