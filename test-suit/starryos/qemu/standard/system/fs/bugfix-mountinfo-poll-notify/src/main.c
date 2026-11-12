#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define MOUNTINFO_PATH "/proc/self/mountinfo"
#define MOUNT_PATH "/tmp/bugfix-mountinfo-poll-notify"
#define MOUNTINFO_SIZE (128 * 1024)

static char mountinfo[MOUNTINFO_SIZE];

static void fail(const char *operation)
{
    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", operation, errno,
            strerror(errno));
    exit(EXIT_FAILURE);
}

static ssize_t read_mountinfo_from_start(int fd)
{
    size_t total = 0;

    if (lseek(fd, 0, SEEK_SET) != 0) {
        return -1;
    }
    while (total + 1 < sizeof(mountinfo)) {
        ssize_t count =
            read(fd, mountinfo + total, sizeof(mountinfo) - total - 1);
        if (count < 0) {
            return -1;
        }
        if (count == 0) {
            mountinfo[total] = '\0';
            return (ssize_t)total;
        }
        total += (size_t)count;
    }

    errno = ENOSPC;
    return -1;
}

static int create_and_attach_tmpfs(void)
{
    int result = mount("tmpfs", MOUNT_PATH, "tmpfs", 0, "");
    if (result < 0) {
        perror("mount tmpfs");
    }
    return result;
}

int main(void)
{
    puts("mountinfo poll notification regression");

    if (mkdir(MOUNT_PATH, 0755) < 0 && errno != EEXIST) {
        fail("mkdir mountpoint");
    }

    int mountinfo_fd = open(MOUNTINFO_PATH, O_RDONLY | O_CLOEXEC);
    if (mountinfo_fd < 0) {
        fail("open mountinfo");
    }
    if (read_mountinfo_from_start(mountinfo_fd) < 0) {
        fail("read initial mountinfo");
    }

    struct pollfd monitor = {
        .fd = mountinfo_fd,
        .events = POLLPRI,
    };
    if (poll(&monitor, 1, 0) != 0) {
        errno = EPROTO;
        fail("mountinfo has a spurious initial change event");
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork mount helper");
    }
    if (child == 0) {
        if (create_and_attach_tmpfs() < 0) {
            fail("child mount");
        }
        _exit(EXIT_SUCCESS);
    }

    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("wait for mount helper");
    }

    monitor.revents = 0;
    int ready = poll(&monitor, 1, 1000);
    if (ready < 0) {
        fail("poll mountinfo change");
    }
    if (ready != 1 ||
        (monitor.revents & (POLLPRI | POLLERR)) != (POLLPRI | POLLERR)) {
        fprintf(stderr,
                "FAIL: mountinfo change event: ready=%d revents=%#x expected "
                "POLLPRI|POLLERR\n",
                ready, monitor.revents);
        return EXIT_FAILURE;
    }

    monitor.revents = 0;
    if (poll(&monitor, 1, 0) != 0) {
        errno = EPROTO;
        fail("mountinfo repeated an already consumed change event");
    }

    if (read_mountinfo_from_start(mountinfo_fd) < 0) {
        fail("reread mountinfo after child mount");
    }
    if (strstr(mountinfo, " " MOUNT_PATH " ") == NULL) {
        errno = ENOENT;
        fail("parent mountinfo contains child mount");
    }

    close(mountinfo_fd);
    if (umount2(MOUNT_PATH, 0) < 0) {
        fail("unmount test tmpfs");
    }
    if (rmdir(MOUNT_PATH) < 0) {
        fail("remove mountpoint");
    }

    puts("PASS: parent observed child mount through mountinfo notification");
    puts("STARRY_MOUNTINFO_POLL_NOTIFY_PASSED");
    return EXIT_SUCCESS;
}
