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

static int has_mount_option(const char *options, const char *expected) {
    size_t expected_len = strlen(expected);
    const char *option = options;

    while (option != NULL) {
        const char *separator = strchr(option, ',');
        size_t option_len = separator ? (size_t)(separator - option) : strlen(option);

        if (option_len == expected_len && strncmp(option, expected, expected_len) == 0) {
            return 1;
        }
        option = separator ? separator + 1 : NULL;
    }
    return 0;
}

int main(void) {
    mkdir("/bind_src", 0755);
    mkdir("/bind_dst", 0755);

    unsigned long source_flags = MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_NOATIME;
    if (mount("tmpfs", "/bind_src", "tmpfs", source_flags, NULL) < 0) {
        perror("mount tmpfs /bind_src");
        return 1;
    }

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

    char *line_saveptr = NULL;
    char *line = strtok_r(buf, "\n", &line_saveptr);
    int found = 0;
    while (line != NULL) {
        char *field_saveptr = NULL;
        char *field = strtok_r(line, " ", &field_saveptr);
        const char *mount_point = NULL;
        const char *options = NULL;

        for (int field_number = 1; field != NULL && field_number <= 6; field_number++) {
            if (field_number == 5) {
                mount_point = field;
            } else if (field_number == 6) {
                options = field;
            }
            field = strtok_r(NULL, " ", &field_saveptr);
        }

        if (mount_point != NULL && options != NULL && strcmp(mount_point, "/bind_dst") == 0) {
            found = 1;
            const char *required_options[] = {"nosuid", "nodev", "noexec", "noatime"};
            for (size_t i = 0; i < sizeof(required_options) / sizeof(required_options[0]); i++) {
                if (!has_mount_option(options, required_options[i])) {
                    fprintf(stderr, "FAIL: /bind_dst options='%s' missing %s\n", options,
                            required_options[i]);
                    return 1;
                }
            }
            break;
        }
        line = strtok_r(NULL, "\n", &line_saveptr);
    }

    if (!found) {
        fprintf(stderr, "FAIL: /bind_dst not found in mountinfo\n");
        return 1;
    }

    /* Cleanup */
    umount("/bind_dst");
    unlink("/bind_src/hello.txt");
    umount("/bind_src");
    rmdir("/bind_src");
    rmdir("/bind_dst");

    printf("TEST_MOUNT_BIND_PASSED\n");
    return 0;
}
