#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_pivot_root
#define SYS_pivot_root __NR_pivot_root
#endif

static long pivot_root_call(const char *new_root, const char *put_old) {
    return syscall(SYS_pivot_root, new_root, put_old);
}

static int verify_regular_directory_is_rejected(void) {
    if (mkdir("/tmp/not-a-mount", 0755) < 0 && errno != EEXIST) {
        perror("child: mkdir /tmp/not-a-mount");
        return 1;
    }
    if (mkdir("/tmp/not-a-mount/old", 0700) < 0 && errno != EEXIST) {
        perror("child: mkdir /tmp/not-a-mount/old");
        return 1;
    }
    if (chdir("/tmp/not-a-mount") < 0) {
        perror("child: chdir /tmp/not-a-mount");
        return 1;
    }
    if (pivot_root_call(".", "old") == 0 || errno != EINVAL) {
        fprintf(stderr, "child: FAIL regular directory pivot errno=%d\n", errno);
        return 1;
    }
    if (chdir("/") < 0) {
        perror("child: chdir /");
        return 1;
    }
    return 0;
}

static int mountinfo_entry_count(void) {
    FILE *mountinfo = fopen("/proc/self/mountinfo", "r");
    if (!mountinfo) {
        perror("child: open /proc/self/mountinfo");
        return -1;
    }
    char line[1024];
    int count = 0;
    while (fgets(line, sizeof(line), mountinfo)) {
        count++;
    }
    fclose(mountinfo);
    return count;
}

static int child_body(void) {
    if (verify_regular_directory_is_rejected() != 0) {
        return 1;
    }

    if (mkdir("/tmp/chroot", 0755) < 0 && errno != EEXIST) {
        perror("child: mkdir /tmp/chroot");
        return 1;
    }
    if (mkdir("/tmp/chroot/old", 0700) < 0 && errno != EEXIST) {
        perror("child: mkdir /tmp/chroot/old");
        return 1;
    }

    FILE *probe = fopen("/tmp/chroot/pivot_marker", "w");
    if (!probe) {
        perror("child: create pivot_marker");
        return 1;
    }
    fputs("PIVOT_ROOT_OK\n", probe);
    fclose(probe);

    int mount_count_before = mountinfo_entry_count();
    if (mount_count_before < 0) {
        return 1;
    }
    if (mount("/tmp/chroot", "/tmp/chroot", NULL, MS_BIND, NULL) < 0) {
        perror("child: bind /tmp/chroot");
        return 1;
    }
    int mount_count_after = mountinfo_entry_count();
    if (mount_count_after != mount_count_before + 1) {
        fprintf(stderr, "child: FAIL self-bind mountinfo count %d -> %d\n",
                mount_count_before, mount_count_after);
        return 1;
    }

    if (chdir("/tmp/chroot") < 0) {
        perror("child: chdir /tmp/chroot");
        return 1;
    }

    if (pivot_root_call(".", "old") < 0) {
        perror("child: pivot_root");
        fprintf(stderr, "child: errno=%d\n", errno);
        return 1;
    }

    FILE *verify = fopen("/pivot_marker", "r");
    if (!verify) {
        fprintf(stderr, "child: FAIL /pivot_marker missing after pivot_root\n");
        return 1;
    }
    char line[64];
    if (!fgets(line, sizeof(line), verify) ||
        strstr(line, "PIVOT_ROOT_OK") == NULL) {
        fprintf(stderr, "child: FAIL /pivot_marker content mismatch\n");
        fclose(verify);
        return 1;
    }
    fclose(verify);

    if (umount2("/old", MNT_DETACH) < 0) {
        perror("child: umount2 /old");
        return 1;
    }

    int old_marker = open("/old/pivot_marker", O_RDONLY);
    if (old_marker >= 0) {
        close(old_marker);
        fprintf(stderr, "child: FAIL old root remains traversable\n");
        return 1;
    }
    if (errno != ENOENT && errno != ENOTDIR) {
        fprintf(stderr, "child: FAIL old-root probe errno=%d\n", errno);
        return 1;
    }

    return 0;
}

int main(void) {
    if (mkdir("/tmp", 0755) < 0 && errno != EEXIST) {
        perror("mkdir /tmp");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        _exit(child_body());
    }

    int status;
    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: child status=%d\n", status);
        return 1;
    }

    printf("TEST_PIVOT_ROOT_PASSED\n");
    return 0;
}
