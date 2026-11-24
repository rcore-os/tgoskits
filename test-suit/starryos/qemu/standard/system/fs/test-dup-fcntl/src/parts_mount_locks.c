#include "test_framework.h"
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void require(int condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "FAIL: mount locks: %s errno=%d (%s)\n",
                message, errno, strerror(errno));
        exit(1);
    }
}

static int new_mount(int *alias)
{
    int context = syscall(SYS_fsopen, "ramfs", 1);
    require(context >= 0, "fsopen");
    require(syscall(SYS_fsconfig, context, 6, NULL, NULL, 0) == 0, "fsconfig create");
    int first = syscall(SYS_fsmount, context, 1, 0);
    require(first >= 0, "fsmount");
    if (alias) {
        *alias = syscall(SYS_fsmount, context, 1, 0);
        /* Linux consumes the context, so obtain its alias by cloning. */
        if (*alias < 0 && errno == EBUSY)
            *alias = syscall(SYS_open_tree, first, "", 1 | AT_EMPTY_PATH | O_CLOEXEC);
        require(*alias >= 0, "create mount alias");
    }
    close(context);
    return first;
}

static int record_lock(int fd, int command, short type)
{
    struct flock lock = {.l_type = type, .l_whence = SEEK_SET, .l_len = 1};
    return syscall(SYS_fcntl, fd, command, &lock);
}

static void query_lock(int fd, int command, short expected)
{
    struct flock lock = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_len = 1};
    require(syscall(SYS_fcntl, fd, command, &lock) == 0 && lock.l_type == expected,
            "query sees lock through other mount");
}

static int open_node(int mount_fd, const char *name, int directory)
{
    int flags = directory ? O_RDONLY | O_DIRECTORY : O_RDWR | O_NONBLOCK;
    int fd = syscall(SYS_openat, mount_fd, name, flags, 0);
    require(fd >= 0, "open node through mount");
    return fd;
}

static void wait_until_sleeping(pid_t child)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%ld/stat", (long)child);
    struct timespec start, now;
    require(clock_gettime(CLOCK_MONOTONIC, &start) == 0, "start wait deadline");
    for (;;) {
        int status;
        require(waitpid(child, &status, WNOHANG) == 0, "waiter has not bypassed lock");
        FILE *stat = fopen(path, "r");
        require(stat != NULL, "open waiter state");
        char line[1024];
        require(fgets(line, sizeof(line), stat) != NULL, "read waiter state");
        fclose(stat);
        char *end = strrchr(line, ')');
        require(end != NULL, "parse waiter state");
        /* After the ready byte the child only executes the blocking lock call. */
        if (end[1] == ' ' && end[2] == 'S')
            return;
        require(clock_gettime(CLOCK_MONOTONIC, &now) == 0, "read wait deadline");
        require(now.tv_sec - start.tv_sec < 5, "waiter enters lock sleep");
        sched_yield();
    }
}

static void close_wakes_alias(int first, int second, const char *name, int directory, int kind)
{
    int holder = open_node(first, name, directory);
    int other = open_node(second, name, directory);
    int command = kind == 0 ? F_SETLK : F_OFD_SETLK;
    int result = kind == 2 ? syscall(SYS_flock, holder, LOCK_EX | LOCK_NB)
                          : record_lock(holder, command, F_WRLCK);
    require(result == 0, "acquire holder lock");
    int ready[2];
    require(pipe(ready) == 0, "create waiter handshake");
    pid_t child = fork();
    require(child >= 0, "fork alias waiter");
    if (child == 0) {
        close(ready[0]);
        close(holder);
        close(other);
        int waiter = open_node(second, name, directory);
        errno = 0;
        result = kind == 2 ? syscall(SYS_flock, waiter, LOCK_EX | LOCK_NB)
                           : record_lock(waiter, command, F_WRLCK);
        require(result == -1 && (errno == EAGAIN || errno == EACCES),
                "cross-mount exclusive locks conflict");
        require(write(ready[1], "r", 1) == 1, "announce waiter");
        alarm(10);
        result = kind == 2 ? syscall(SYS_flock, waiter, LOCK_EX)
                           : record_lock(waiter, kind == 0 ? F_SETLKW : F_OFD_SETLKW, F_WRLCK);
        require(result == 0, "blocked alias acquires after close");
        close(waiter);
        _exit(0);
    }
    close(ready[1]);
    char byte;
    require(read(ready[0], &byte, 1) == 1 && byte == 'r', "waiter confirms conflict");
    close(ready[0]);
    wait_until_sleeping(child);
    if (kind == 0) {
        /* Closing another alias must remove this process's inode-wide locks. */
        close(other);
    } else {
        int duplicate = dup(holder);
        require(duplicate >= 0, "duplicate lock owner");
        close(holder);
        errno = 0;
        result = kind == 2 ? syscall(SYS_flock, other, LOCK_EX | LOCK_NB)
                           : record_lock(other, command, F_WRLCK);
        require(result == -1 && errno == EAGAIN, "duplicate retains lock owner");
        close(duplicate);
    }
    int status;
    require(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
            "alias waiter completes after release");
    close(kind == 0 ? holder : other);
}

int parts_mount_locks(void)
{
    int second;
    int first = new_mount(&second);
    int separate = new_mount(NULL);
    const char *names[] = {"regular", "fifo", "directory"};
    for (int node = 0; node < 3; node++) {
        int roots[] = {first, separate};
        for (size_t i = 0; i < sizeof(roots) / sizeof(roots[0]); i++) {
            if (node == 0) {
                int fd = syscall(SYS_openat, roots[i], names[node], O_CREAT | O_RDWR, 0600);
                require(fd >= 0, "create regular file");
                close(fd);
            } else if (node == 1) {
                require(mkfifoat(roots[i], names[node], 0600) == 0, "create FIFO");
            } else {
                require(mkdirat(roots[i], names[node], 0700) == 0, "create directory");
            }
        }
        int holder = open_node(first, names[node], node == 2);
        int peer = open_node(second, names[node], node == 2);
        int isolated = open_node(separate, names[node], node == 2);
        struct stat before, after;
        require(fstat(peer, &before) == 0, "snapshot alias metadata");
        short type = node == 2 ? F_RDLCK : F_WRLCK;
        require(record_lock(holder, F_OFD_SETLK, type) == 0, "acquire OFD record lock");
        query_lock(peer, F_GETLK, type);
        query_lock(peer, F_OFD_GETLK, type);
        query_lock(isolated, F_OFD_GETLK, F_UNLCK);
        require(record_lock(peer, F_SETLK, F_UNLCK) == 0, "other owner unlock");
        query_lock(peer, F_GETLK, type);
        require(record_lock(holder, F_OFD_SETLK, F_UNLCK) == 0, "release OFD record lock");
        query_lock(peer, F_OFD_GETLK, F_UNLCK);
        require(record_lock(holder, F_SETLK, type) == 0, "acquire POSIX record lock");
        query_lock(peer, F_OFD_GETLK, type);
        close(peer);
        query_lock(holder, F_OFD_GETLK, F_UNLCK);
        require(syscall(SYS_flock, holder, LOCK_EX | LOCK_NB) == 0 &&
                syscall(SYS_flock, isolated, LOCK_EX | LOCK_NB) == 0,
                "separate filesystems have independent flock locks");
        close(isolated);
        close(holder);
        peer = open_node(second, names[node], node == 2);
        require(fstat(peer, &after) == 0 && before.st_dev == after.st_dev &&
                before.st_ino == after.st_ino, "locking preserves stat identity");
        close(peer);
        for (int kind = node == 2 ? 2 : 0; kind < 3; kind++)
            close_wakes_alias(first, second, names[node], node == 2, kind);
        printf("PASS: cross-mount lock identity and release: %s\n", names[node]);
    }
    close(separate);
    close(second);
    close(first);
    CHECK(1, "mount aliases share advisory locks and independent filesystems stay isolated");
    return 0;
}
