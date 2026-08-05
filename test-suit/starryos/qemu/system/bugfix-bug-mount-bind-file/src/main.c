#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

static int create_file(const char *path, const char *content) {
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0644);
    if (fd < 0) {
        fprintf(stderr, "open %s failed: %s\n", path, strerror(errno));
        return -1;
    }

    size_t length = strlen(content);
    ssize_t written = write(fd, content, length);
    if (written < 0 || (size_t)written != length) {
        fprintf(stderr, "write %s failed: %s\n", path, strerror(errno));
        close(fd);
        return -1;
    }

    if (close(fd) < 0) {
        fprintf(stderr, "close %s failed: %s\n", path, strerror(errno));
        return -1;
    }
    return 0;
}

static int verify_file_bind(void) {
    const char *source = "/tmp/starry-bind-source";
    const char *target = "/tmp/starry-bind-target";
    const char *content = "file bind source\n";

    if (create_file(source, content) < 0 || create_file(target, "covered target\n") < 0) {
        return -1;
    }
    if (mount(source, target, NULL, MS_BIND, NULL) < 0) {
        fprintf(stderr, "file-to-file bind failed: %s\n", strerror(errno));
        return -1;
    }

    char buffer[64] = {0};
    int fd = open(target, O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "open bound target failed: %s\n", strerror(errno));
        return -1;
    }
    ssize_t length = read(fd, buffer, sizeof(buffer) - 1);
    close(fd);
    if (length < 0 || strcmp(buffer, content) != 0) {
        fprintf(stderr, "bound target content mismatch: %s\n", buffer);
        return -1;
    }

    if (umount(target) < 0) {
        fprintf(stderr, "unmount file bind failed: %s\n", strerror(errno));
        return -1;
    }
    unlink(source);
    unlink(target);
    return 0;
}

static int expect_type_mismatch(const char *source, const char *target) {
    errno = 0;
    if (mount(source, target, NULL, MS_BIND, NULL) == 0) {
        fprintf(stderr, "cross-type bind unexpectedly succeeded: %s -> %s\n", source, target);
        umount(target);
        return -1;
    }
    if (errno != ENOTDIR) {
        fprintf(stderr, "cross-type bind returned %s, expected ENOTDIR: %s -> %s\n",
                strerror(errno), source, target);
        return -1;
    }
    return 0;
}

int main(void) {
    const char *file = "/tmp/starry-bind-type-file";
    const char *directory = "/tmp/starry-bind-type-directory";

    if (verify_file_bind() < 0) {
        return 1;
    }
    if (create_file(file, "type check\n") < 0 || mkdir(directory, 0755) < 0) {
        return 1;
    }
    if (expect_type_mismatch(file, directory) < 0 ||
        expect_type_mismatch(directory, file) < 0) {
        return 1;
    }

    unlink(file);
    rmdir(directory);
    printf("TEST_MOUNT_BIND_FILE_PASSED\n");
    return 0;
}
