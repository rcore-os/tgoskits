#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static int proc_root_lists_pid_1(void)
{
    DIR *dir = opendir("/proc");
    if (dir == NULL) {
        return 0;
    }

    int found = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, "1") == 0) {
            found = 1;
            break;
        }
    }

    closedir(dir);
    return found;
}

static ssize_t read_file(const char *path, char *buf, size_t size)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    ssize_t nread = read(fd, buf, size - 1);
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;

    if (nread >= 0) {
        buf[nread] = '\0';
    }
    return nread;
}

int main(void)
{
    char buf[512];
    struct stat st;

    check(stat("/proc/1", &st) == 0 && S_ISDIR(st.st_mode), "/proc/1 exists as a directory");
    check(proc_root_lists_pid_1(), "/proc readdir lists PID 1");

    errno = 0;
    ssize_t nread = read_file("/proc/1/stat", buf, sizeof(buf));
    check(nread > 0, "/proc/1/stat is readable");
    if (nread > 0) {
        check(strncmp(buf, "1 (", 3) == 0, "/proc/1/stat starts with PID 1");
    } else {
        printf("INFO: read /proc/1/stat failed: %s\n", strerror(errno));
        check(0, "/proc/1/stat starts with PID 1");
    }

    errno = 0;
    nread = read_file("/proc/1/cmdline", buf, sizeof(buf));
    check(nread > 0, "/proc/1/cmdline is non-empty");
    if (nread <= 0) {
        printf("INFO: read /proc/1/cmdline failed: %s\n", strerror(errno));
    }

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
