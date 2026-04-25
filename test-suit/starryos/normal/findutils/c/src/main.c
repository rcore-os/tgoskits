#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <dirent.h>

static int run_find(const char *path) {
    pid_t pid = fork();
    if (pid < 0) return 1;
    if (pid == 0) {
        int fd = open("/dev/null", O_WRONLY);
        if (fd < 0) _exit(1);
        dup2(fd, STDOUT_FILENO);
        dup2(fd, STDERR_FILENO);
        close(fd);
        execl("/usr/bin/find", "find", path, "-maxdepth", "2", NULL);
        _exit(1);
    }
    int status;
    if (waitpid(pid, &status, 0) < 0) return 1;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 1;
    return 0;
}

static int check_dir(const char *path) {
    DIR *d = opendir(path);
    if (!d) return 1;
    int count = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL) {
        if (ent->d_name[0] == '.') continue;
        count++;
    }
    closedir(d);
    return count > 0 ? 0 : 1;
}

int main(void) {
    /* Test ext4 rootfs */
    printf("Testing / (ext4) ...\n");
    if (run_find("/etc") != 0) return 1;

    /* Test devfs */
    printf("Testing /dev (devfs) ...\n");
    if (check_dir("/dev") != 0) return 1;
    if (run_find("/dev") != 0) return 1;

    /* Test procfs */
    printf("Testing /proc (procfs) ...\n");
    if (check_dir("/proc") != 0) return 1;
    if (run_find("/proc") != 0) return 1;

    /* Test tmpfs */
    printf("Testing /tmp (tmpfs) ...\n");
    if (run_find("/tmp") != 0) return 1;

    /* Test sysfs (tmpfs-backed) */
    printf("Testing /sys (tmpfs) ...\n");
    if (run_find("/sys") != 0) return 1;

    printf("FINDUTILS TEST PASSED\n");
    return 0;
}
