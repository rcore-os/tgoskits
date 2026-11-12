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

/* Read through the real procfs boundary after changing the process root.
 * The namespace still contains the original root and /bind_src mounts. */
static int check_rooted_mounts(const char *path, int mountinfo) {
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    if (fd < 0) {
        perror(path);
        return 1;
    }
    char buf[BUF_SIZE];
    size_t used = 0;
    for (;;) {
        long n = syscall(SYS_read, fd, buf + used, sizeof(buf) - 1 - used);
        if (n < 0) {
            perror("read rooted mounts");
            close(fd);
            return 1;
        }
        if (n == 0) {
            break;
        }
        used += (size_t)n;
        if (used == sizeof(buf) - 1) {
            fprintf(stderr, "FAIL: rooted mount table exceeds buffer\n");
            close(fd);
            return 1;
        }
    }
    close(fd);
    buf[used] = '\0';
    int root_count = 0, proc_count = 0;
    char *save = NULL;
    for (char *line = strtok_r(buf, "\n", &save); line;
         line = strtok_r(NULL, "\n", &save)) {
        char point[4096];
        int fields = mountinfo
            ? sscanf(line, "%*s %*s %*s %*s %4095s", point)
            : sscanf(line, "%*s %4095s", point);
        if (fields != 1) {
            fprintf(stderr, "FAIL: malformed rooted mount row: %s\n", line);
            return 1;
        }
        if (strcmp(point, "/") == 0) {
            root_count++;
        } else if (strcmp(point, "/proc") == 0) {
            proc_count++;
        } else {
            fprintf(stderr, "FAIL: mount outside chroot is visible: %s\n", point);
            return 1;
        }
    }
    if (root_count != 1 || proc_count != 1) {
        fprintf(stderr, "FAIL: %s has %d root rows and %d proc rows\n",
                path, root_count, proc_count);
        return 1;
    }
    return 0;
}

static int check_chroot_mount_visibility(void) {
    if (mkdir("/bind_dst/proc", 0755) < 0 ||
        mount("/proc", "/bind_dst/proc", NULL, MS_BIND, NULL) < 0) {
        perror("prepare chroot procfs");
        return 1;
    }
    pid_t child = fork();
    if (child < 0) {
        perror("fork chroot observer");
        return 1;
    }
    if (child == 0) {
        if (syscall(SYS_chroot, "/bind_dst") < 0 || chdir("/") < 0) {
            perror("chroot bind destination");
            _exit(1);
        }
        _exit(check_rooted_mounts("/proc/self/mountinfo", 1) ||
              check_rooted_mounts("/proc/mounts", 0));
    }
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: rooted mount visibility\n");
        return 1;
    }
    if (umount("/bind_dst/proc") < 0 || rmdir("/bind_dst/proc") < 0) {
        perror("cleanup chroot procfs");
        return 1;
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
    if (fprintf(f, "bind mount test\n") < 0) {
        perror("fprintf /bind_src/hello.txt");
        fclose(f);
        return 1;
    }
    if (fclose(f) != 0) {
        perror("fclose /bind_src/hello.txt");
        return 1;
    }

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

    if (check_chroot_mount_visibility() != 0) {
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
