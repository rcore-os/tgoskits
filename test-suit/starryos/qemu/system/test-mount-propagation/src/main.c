// Migrated from the former nix-sandbox-debug suite.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

#define BUF_SIZE 65536

static int read_mountinfo(char *buf, size_t size) {
    FILE *f = fopen("/proc/self/mountinfo", "r");
    if (!f) {
        return -1;
    }
    size_t n = fread(buf, 1, size - 1, f);
    fclose(f);
    buf[n] = '\0';
    return (int)n;
}

static int mountinfo_contains(const char *buf, const char *path) {
    const char *p = buf;
    while ((p = strstr(p, path)) != NULL) {
        const char *next = p + strlen(path);
        char trailing = *next;
        if (trailing == ' ' || trailing == '\n' || trailing == '\0') {
            return 1;
        }
        p = next;
    }
    return 0;
}

int main(void) {
    mkdir("/prop_shared", 0755);
    mkdir("/prop_peer", 0755);

    if (mount("tmpfs", "/prop_shared", "tmpfs", 0, NULL) < 0) {
        perror("mount tmpfs /prop_shared");
        return 1;
    }

    if (mount(NULL, "/prop_shared", NULL, MS_SHARED, NULL) < 0) {
        perror("mount --make-shared /prop_shared");
        return 1;
    }

    if (mount("/prop_shared", "/prop_peer", NULL, MS_BIND, NULL) < 0) {
        perror("mount --bind /prop_shared /prop_peer");
        return 1;
    }

    char buf[BUF_SIZE];
    if (read_mountinfo(buf, sizeof(buf)) < 0) {
        fprintf(stderr, "FAIL: cannot read /proc/self/mountinfo\n");
        return 1;
    }

    if (!mountinfo_contains(buf, "/prop_shared")) {
        fprintf(stderr, "FAIL: /prop_shared missing from mountinfo before unmount\n");
        return 1;
    }
    if (!mountinfo_contains(buf, "/prop_peer")) {
        fprintf(stderr, "FAIL: /prop_peer missing from mountinfo before unmount\n");
        return 1;
    }

    if (umount("/prop_shared") < 0) {
        perror("umount /prop_shared");
        return 1;
    }

    if (read_mountinfo(buf, sizeof(buf)) < 0) {
        fprintf(stderr, "FAIL: cannot re-read /proc/self/mountinfo\n");
        return 1;
    }

    if (mountinfo_contains(buf, "/prop_shared")) {
        fprintf(stderr, "FAIL: /prop_shared still in mountinfo after umount\n");
        return 1;
    }
    if (mountinfo_contains(buf, "/prop_peer")) {
        fprintf(stderr,
                "FAIL: /prop_peer still in mountinfo — peer-group unmount did NOT propagate\n");
        return 1;
    }

    rmdir("/prop_shared");
    rmdir("/prop_peer");

    printf("TEST_MOUNT_PROPAGATION_PASSED\n");
    return 0;
}
