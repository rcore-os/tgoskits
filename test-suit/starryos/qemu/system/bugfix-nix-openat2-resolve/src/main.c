/* Regression for the openat2 contract used by Nix NAR restoration. */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define RESOLVE_NO_SYMLINKS 0x04
#define RESOLVE_BENEATH 0x08

#ifndef SYS_openat2
#if defined(__x86_64__) || defined(__aarch64__) || defined(__riscv) ||         \
    defined(__loongarch64)
#define SYS_openat2 437
#else
#error "SYS_openat2 is unknown for this architecture"
#endif
#endif

struct open_how {
    uint64_t flags;
    uint64_t mode;
    uint64_t resolve;
};

static int failures;

#define CHECK(condition, message)                                            \
    do {                                                                     \
        if (condition) {                                                     \
            printf("PASS: %s\n", message);                                  \
        } else {                                                             \
            printf("FAIL: %s: errno=%d (%s)\n", message, errno,              \
                   strerror(errno));                                         \
            failures++;                                                      \
        }                                                                    \
    } while (0)

static int openat2_beneath_no_symlinks(int dirfd, const char *path,
                                       uint64_t flags, uint64_t mode)
{
    const struct open_how how = {
        .flags = flags,
        .mode = mode,
        .resolve = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
    };

    return (int)syscall(SYS_openat2, dirfd, path, &how, sizeof(how));
}

int main(void)
{
    const char *root = "/tmp/nix-openat2-resolve";
    char created[128];
    char link_path[128];

    snprintf(created, sizeof(created), "%s/sub/config.guess", root);
    snprintf(link_path, sizeof(link_path), "%s/link", root);
    unlink(created);
    unlink(link_path);
    rmdir("/tmp/nix-openat2-resolve/sub");
    rmdir(root);

    CHECK(mkdir(root, 0700) == 0, "create fixture root");
    CHECK(mkdir("/tmp/nix-openat2-resolve/sub", 0700) == 0,
          "create fixture subdirectory");

    int rootfd = open(root, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    CHECK(rootfd >= 0, "open fixture root directory");
    if (rootfd < 0)
        return 1;

    int filesystem_rootfd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    CHECK(filesystem_rootfd >= 0, "open filesystem root directory");
    if (filesystem_rootfd >= 0) {
        int dotfd = openat2_beneath_no_symlinks(
            filesystem_rootfd, ".", O_RDONLY | O_DIRECTORY | O_CLOEXEC, 0);
        CHECK(dotfd >= 0, "RESOLVE_BENEATH opens root directory dot path");
        if (dotfd >= 0)
            close(dotfd);
        close(filesystem_rootfd);
    }

    int fd = openat2_beneath_no_symlinks(
        rootfd, "sub/config.guess", O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC,
        0666);
    CHECK(fd >= 0,
          "Nix openat2 RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS file creation");
    if (fd >= 0) {
        CHECK(write(fd, "nix\n", 4) == 4, "write restored NAR file");
        close(fd);
    }

    errno = 0;
    fd = openat2_beneath_no_symlinks(
        rootfd, "../nix-openat2-escape",
        O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0666);
    CHECK(fd == -1 && (errno == EXDEV || errno == ELOOP),
          "RESOLVE_BENEATH rejects parent escape");
    if (fd >= 0) {
        close(fd);
        unlink("/tmp/nix-openat2-escape");
    }

    CHECK(symlink("sub", link_path) == 0, "create fixture symlink");
    errno = 0;
    fd = openat2_beneath_no_symlinks(
        rootfd, "link/via-symlink", O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC,
        0666);
    CHECK(fd == -1 && errno == ELOOP,
          "RESOLVE_NO_SYMLINKS rejects an intermediate symlink");
    if (fd >= 0) {
        close(fd);
        unlink("/tmp/nix-openat2-resolve/sub/via-symlink");
    }

    errno = 0;
    fd = openat2_beneath_no_symlinks(rootfd, "link", O_RDONLY | O_CLOEXEC, 0);
    CHECK(fd == -1 && errno == ELOOP,
          "RESOLVE_NO_SYMLINKS rejects a final symlink");
    if (fd >= 0)
        close(fd);

    errno = 0;
    fd = openat2_beneath_no_symlinks(
        rootfd, "link", O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0666);
    CHECK(fd == -1 && errno == ELOOP,
          "RESOLVE_NO_SYMLINKS takes precedence over O_EXCL for a symlink");
    if (fd >= 0)
        close(fd);

    close(rootfd);
    unlink(created);
    unlink(link_path);
    rmdir("/tmp/nix-openat2-resolve/sub");
    rmdir(root);

    if (failures != 0) {
        printf("NIX_OPENAT2_RESOLVE_FAILED: %d failure(s)\n", failures);
        return 1;
    }
    puts("NIX_OPENAT2_RESOLVE_PASSED");
    return 0;
}
