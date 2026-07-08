#define _GNU_SOURCE
#include <errno.h>
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

static int child_body(void) {
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

    if (mount("/tmp/chroot", "/tmp/chroot", NULL, MS_BIND, NULL) < 0) {
        perror("child: bind /tmp/chroot");
        return 1;
    }

    /* pivot_root uses absolute paths because the StarryOS sys_pivot_root
     * implementation string-validates put_old against new_root, unlike
     * Linux which resolves paths. See syscall/fs/mount.rs. */
    if (pivot_root_call("/tmp/chroot", "/tmp/chroot/old") < 0) {
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

    /* /proc/self/mountinfo would need /proc mounted in the new root; we
     * skip that here — the probe-file reachability above already proves
     * the pivot happened. The kernel's chroot_fs_refs propagation will
     * repoint every other task whose root or cwd matched the old root,
     * so the surrounding test runner environment will be left inside the
     * new root once this test exits. The runner puts pivot-root last to
     * tolerate that. */
    if (umount2("/old", MNT_DETACH) < 0) {
        fprintf(stderr, "child: NOTE umount2(/old) errno=%d\n", errno);
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
