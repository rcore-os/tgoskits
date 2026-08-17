#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

#define CGROUP_PROBE_PATH "/tmp/starrynixos-cgroup2"

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
        return;
    }

    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", message, errno,
            strerror(errno));
    failures++;
}

static void check_directory(const char *path)
{
    struct stat metadata;

    errno = 0;
    int result = stat(path, &metadata);
    char message[128];
    snprintf(message, sizeof(message), "%s is a visible directory", path);
    check(result == 0 && S_ISDIR(metadata.st_mode), message);
}

static void check_pid1(void)
{
    char command[256];
    int fd;
    ssize_t length;

    errno = 0;
    fd = open("/proc/1/cmdline", O_RDONLY | O_CLOEXEC);
    check(fd >= 0, "PID 1 command line is accessible");
    if (fd < 0) {
        return;
    }

    errno = 0;
    length = read(fd, command, sizeof(command) - 1);
    check(length > 0, "PID 1 command line is non-empty");
    close(fd);
    if (length > 0) {
        command[length] = '\0';
        printf("OBSERVE: pid1_cmdline=%s\n", command);
    }
}

static void check_cgroup2(void)
{
    char content[256];
    int fd;
    ssize_t length;

    if (mkdir(CGROUP_PROBE_PATH, 0755) != 0 && errno != EEXIST) {
        check(0, "create cgroup2 probe mountpoint");
        return;
    }

    errno = 0;
    if (mount("none", CGROUP_PROBE_PATH, "cgroup2", 0, NULL) != 0) {
        check(0, "mount cgroup2 hierarchy");
        return;
    }
    check(1, "mount cgroup2 hierarchy");

    errno = 0;
    fd = open(CGROUP_PROBE_PATH "/cgroup.procs", O_RDONLY | O_CLOEXEC);
    check(fd >= 0, "cgroup2 cgroup.procs is visible");
    if (fd >= 0) {
        errno = 0;
        length = read(fd, content, sizeof(content) - 1);
        check(length > 0, "cgroup2 cgroup.procs reports processes");
        if (length > 0) {
            content[length] = '\0';
            printf("OBSERVE: cgroup.procs=%s", content);
        }
        close(fd);
    }

    errno = 0;
    check(umount2(CGROUP_PROBE_PATH, 0) == 0, "unmount cgroup2 hierarchy");
    rmdir(CGROUP_PROBE_PATH);
}

int main(void)
{
    check_pid1();
    check_directory("/proc");
    check_directory("/sys");
    check_directory("/dev");
    check_directory("/run");
    check_cgroup2();

    if (failures != 0) {
        fprintf(stderr, "STARRY_NIXOS_BASELINE_PROBES_FAILED: %d checks\n",
                failures);
        return 1;
    }

    puts("STARRY_NIXOS_BASELINE_PROBES_PASSED");
    return 0;
}
