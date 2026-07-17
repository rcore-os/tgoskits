// Migrated from the former nix-sandbox-debug suite.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

#define BUF_SIZE 65536

int main(void) {
    mkdir("/bind_src", 0755);
    mkdir("/bind_dst", 0755);

    /* Create a file in source */
    FILE *f = fopen("/bind_src/hello.txt", "w");
    if (!f) {
        perror("fopen /bind_src/hello.txt");
        return 1;
    }
    fprintf(f, "bind mount test\n");
    fclose(f);

    /* Bind mount /bind_src -> /bind_dst */
    if (mount("/bind_src", "/bind_dst", NULL, MS_BIND, NULL) < 0) {
        perror("mount --bind");
        return 1;
    }

    /* Verify /bind_dst/hello.txt is accessible */
    f = fopen("/bind_dst/hello.txt", "r");
    if (!f) {
        fprintf(stderr, "FAIL: cannot read /bind_dst/hello.txt after bind mount\n");
        return 1;
    }
    char content[256];
    if (!fgets(content, sizeof(content), f)) {
        fprintf(stderr, "FAIL: cannot read content\n");
        return 1;
    }
    fclose(f);

    if (strstr(content, "bind mount test") == NULL) {
        fprintf(stderr, "FAIL: content mismatch: %s\n", content);
        return 1;
    }

    /* Verify /bind_dst appears in mountinfo */
    f = fopen("/proc/self/mountinfo", "r");
    if (!f) {
        fprintf(stderr, "FAIL: cannot open /proc/self/mountinfo\n");
        return 1;
    }

    char buf[BUF_SIZE];
    size_t n = fread(buf, 1, sizeof(buf) - 1, f);
    fclose(f);
    buf[n] = '\0';

    if (strstr(buf, "/bind_dst") == NULL) {
        fprintf(stderr, "FAIL: /bind_dst not found in mountinfo\n");
        return 1;
    }

    /* Cleanup */
    umount("/bind_dst");
    unlink("/bind_src/hello.txt");
    rmdir("/bind_src");
    rmdir("/bind_dst");

    printf("TEST_MOUNT_BIND_PASSED\n");
    return 0;
}
