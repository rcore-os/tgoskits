#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef STATX_MNT_ID
#define STATX_MNT_ID 0x00001000U
#endif

#ifndef STATX_MNT_ID_UNIQUE
#define STATX_MNT_ID_UNIQUE 0x00004000U
#endif

static const char *const FIXTURE_DIR = "/tmp/starry-systemd-chase-etc-static";

static int failures;

static void check(int condition, const char *stage)
{
    if (condition) {
        printf("PASS: %s\n", stage);
        return;
    }

    printf("FAIL: %s errno=%d (%s)\n", stage, errno, strerror(errno));
    failures++;
}

static int open_path_directory_at(int dirfd, const char *path)
{
    return (int)syscall(
        SYS_openat,
        dirfd,
        path,
        O_PATH | O_DIRECTORY | O_CLOEXEC,
        0
    );
}

static int open_path_no_follow_at(int dirfd, const char *path)
{
    return (int)syscall(
        SYS_openat,
        dirfd,
        path,
        O_PATH | O_NOFOLLOW | O_CLOEXEC,
        0
    );
}

static int statx_path_fd(int fd, struct statx *metadata)
{
    unsigned int mask = STATX_TYPE | STATX_UID | STATX_INO | STATX_MNT_ID;

    mask |= STATX_MNT_ID_UNIQUE;

    return (int)syscall(
        SYS_statx,
        fd,
        "",
        AT_EMPTY_PATH,
        mask,
        metadata
    );
}

static int statx_path_fd_matches_type(int fd, mode_t expected_type)
{
    struct statx metadata = {};
    const unsigned int required_mask = STATX_TYPE | STATX_UID | STATX_INO;

    if (statx_path_fd(fd, &metadata) < 0) {
        return 0;
    }
    if ((metadata.stx_mask & required_mask) != required_mask ||
        (metadata.stx_mask & (STATX_MNT_ID | STATX_MNT_ID_UNIQUE)) == 0 ||
        (metadata.stx_mode & S_IFMT) != expected_type) {
        errno = EPROTO;
        return 0;
    }

    return 1;
}

static int reopen_path_fd_readonly(int path_fd)
{
    char proc_fd_path[64];

    if (snprintf(proc_fd_path, sizeof(proc_fd_path), "/proc/self/fd/%d", path_fd) < 0) {
        errno = EOVERFLOW;
        return -1;
    }

    return open(proc_fd_path, O_RDONLY | O_CLOEXEC);
}

static const char *relative_to_root(const char *path)
{
    return path[0] == '/' ? path + 1 : path;
}

static int create_directory(const char *path)
{
    if (mkdir(path, 0700) == 0 || errno == EEXIST) {
        return 0;
    }

    return -1;
}

static int create_fixture(
    char *first_link,
    size_t first_link_size,
    char *second_link,
    size_t second_link_size,
    char *config_path,
    size_t config_path_size
)
{
    char etc_dir[128];
    char etc_systemd_dir[160];
    char static_dir[160];
    char static_systemd_dir[192];
    char static_link[160];
    char static_config_link[224];
    int config_fd = -1;

    if (snprintf(etc_dir, sizeof(etc_dir), "%s/etc", FIXTURE_DIR) < 0 ||
        snprintf(etc_systemd_dir, sizeof(etc_systemd_dir), "%s/systemd", etc_dir) < 0 ||
        snprintf(static_dir, sizeof(static_dir), "%s/static", FIXTURE_DIR) < 0 ||
        snprintf(static_systemd_dir, sizeof(static_systemd_dir), "%s/systemd", static_dir) < 0 ||
        snprintf(static_link, sizeof(static_link), "%s/static", etc_dir) < 0 ||
        snprintf(first_link, first_link_size, "%s/system.conf", etc_systemd_dir) < 0 ||
        snprintf(second_link, second_link_size, "%s/static/systemd/system.conf", etc_dir) < 0 ||
        snprintf(static_config_link, sizeof(static_config_link), "%s/system.conf", static_systemd_dir) <
            0 ||
        snprintf(config_path, config_path_size, "%s/nix-store-system.conf", FIXTURE_DIR) < 0) {
        errno = EOVERFLOW;
        return -1;
    }

    unlink(first_link);
    unlink(static_config_link);
    unlink(static_link);
    unlink(config_path);

    if (create_directory(FIXTURE_DIR) < 0 ||
        create_directory(etc_dir) < 0 ||
        create_directory(etc_systemd_dir) < 0 ||
        create_directory(static_dir) < 0 ||
        create_directory(static_systemd_dir) < 0) {
        return -1;
    }

    config_fd = open(config_path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (config_fd < 0) {
        return -1;
    }
    if (write(config_fd, "Manager\\n", 8) != 8 || close(config_fd) < 0) {
        return -1;
    }

    if (symlink(static_dir, static_link) < 0 ||
        symlink(second_link, first_link) < 0 ||
        symlink(config_path, static_config_link) < 0) {
        return -1;
    }

    return 0;
}

static int read_link_target(
    int dirfd,
    const char *path,
    char *target,
    size_t target_size
)
{
    ssize_t target_length = readlinkat(dirfd, path, target, target_size - 1);
    if (target_length < 0 || (size_t)target_length >= target_size - 1) {
        if (target_length >= 0) {
            errno = EOVERFLOW;
        }
        return -1;
    }

    target[target_length] = '\0';
    return 0;
}

static int reopen_root_directory(int root_fd, int *reopened_root_fd)
{
    struct stat metadata;
    int fd = open_path_directory_at(root_fd, ".");

    if (fd < 0) {
        return -1;
    }
    if (fstat(fd, &metadata) < 0 || !S_ISDIR(metadata.st_mode)) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno != 0 ? saved_errno : EPROTO;
        return -1;
    }

    *reopened_root_fd = fd;
    return 0;
}

int main(void)
{
    char first_link[192];
    char second_link[224];
    char config_path[192];
    char config_parent_path[192];
    char first_target[224];
    char second_target[224];
    struct stat metadata;
    int root_fd = -1;
    int first_root_fd = -1;
    int second_root_fd = -1;
    int first_link_fd = -1;
    int second_link_fd = -1;
    int config_parent_fd = -1;
    int config_path_fd = -1;
    int config_fd = -1;

    setvbuf(stdout, NULL, _IONBF, 0);
    printf("STARRY_SYSTEM_TEST_BEGIN: bugfix-systemd-chase-etc-static\n");

    if (create_fixture(
            first_link,
            sizeof(first_link),
            second_link,
            sizeof(second_link),
            config_path,
            sizeof(config_path)
        ) < 0) {
        check(0, "create NixOS-style /etc/static fixture");
        goto out;
    }

    root_fd = open_path_directory_at(AT_FDCWD, "/");
    check(root_fd >= 0, "open O_PATH root directory");
    if (root_fd < 0) {
        goto out;
    }

    check(
        reopen_root_directory(root_fd, &first_root_fd) == 0,
        "reopen O_PATH root with openat(fd, \".\")"
    );
    if (first_root_fd < 0) {
        goto out;
    }
    check(
        reopen_root_directory(first_root_fd, &second_root_fd) == 0,
        "repeat O_PATH root reopen after absolute symlink chase"
    );
    if (second_root_fd < 0) {
        goto out;
    }
    check(
        statx_path_fd_matches_type(second_root_fd, S_IFDIR),
        "statx AT_EMPTY_PATH reports the O_PATH root metadata systemd requires"
    );

    first_link_fd = open_path_no_follow_at(second_root_fd, relative_to_root(first_link));
    check(
        first_link_fd >= 0,
        "pin /etc/systemd/system.conf through O_PATH root descriptor"
    );
    if (first_link_fd < 0) {
        goto out;
    }
    check(
        fstat(first_link_fd, &metadata) == 0 && S_ISLNK(metadata.st_mode),
        "fstat first O_PATH pin reports symlink"
    );
    check(
        statx_path_fd_matches_type(first_link_fd, S_IFLNK),
        "statx AT_EMPTY_PATH reports the first O_PATH symlink metadata"
    );
    check(
        read_link_target(
            second_root_fd,
            relative_to_root(first_link),
            first_target,
            sizeof(first_target)
        ) == 0 &&
            strcmp(first_target, second_link) == 0,
        "read first absolute NixOS /etc/static link"
    );
    if (failures != 0) {
        goto out;
    }

    second_link_fd = open_path_no_follow_at(second_root_fd, relative_to_root(first_target));
    check(
        second_link_fd >= 0,
        "pin /etc/static/systemd/system.conf through O_PATH root descriptor"
    );
    if (second_link_fd < 0) {
        goto out;
    }
    check(
        fstat(second_link_fd, &metadata) == 0 && S_ISLNK(metadata.st_mode),
        "fstat second O_PATH pin reports symlink"
    );
    check(
        statx_path_fd_matches_type(second_link_fd, S_IFLNK),
        "statx AT_EMPTY_PATH reports the second O_PATH symlink metadata"
    );
    check(
        read_link_target(
            second_root_fd,
            relative_to_root(first_target),
            second_target,
            sizeof(second_target)
        ) == 0 &&
            strcmp(second_target, config_path) == 0,
        "read final Nix store configuration link"
    );
    if (failures != 0) {
        goto out;
    }

    config_path_fd = open_path_no_follow_at(second_root_fd, relative_to_root(second_target));
    check(config_path_fd >= 0, "pin final Nix store configuration through O_PATH root descriptor");
    if (config_path_fd < 0) {
        goto out;
    }
    check(
        fstat(config_path_fd, &metadata) == 0 && S_ISREG(metadata.st_mode),
        "fstat final O_PATH pin reports regular file"
    );
    check(
        statx_path_fd_matches_type(config_path_fd, S_IFREG),
        "statx AT_EMPTY_PATH reports the final O_PATH file metadata"
    );
    config_fd = reopen_path_fd_readonly(config_path_fd);
    check(
        config_fd >= 0,
        "reopen final configuration from its O_PATH descriptor through procfs"
    );
    if (config_fd >= 0) {
        check(
            fstat(config_fd, &metadata) == 0 && S_ISREG(metadata.st_mode),
            "fstat procfs-reopened final configuration reports a regular file"
        );
        if (close(config_fd) < 0) {
            check(0, "close procfs-reopened final configuration");
            goto out;
        }
        config_fd = -1;
    }

    if (snprintf(config_parent_path, sizeof(config_parent_path), "%s", FIXTURE_DIR) < 0) {
        errno = EOVERFLOW;
        check(0, "format final NixOS configuration parent path");
        goto out;
    }
    config_parent_fd = open_path_directory_at(
        second_root_fd,
        relative_to_root(config_parent_path)
    );
    check(
        config_parent_fd >= 0,
        "open NixOS configuration parent through O_PATH root descriptor"
    );
    if (config_parent_fd < 0) {
        goto out;
    }

    config_fd = (int)syscall(
        SYS_openat,
        config_parent_fd,
        "nix-store-system.conf",
        O_RDONLY | O_NOFOLLOW | O_CLOEXEC,
        0
    );
    check(
        config_fd >= 0,
        "open final configuration from O_PATH parent with O_RDONLY|O_NOFOLLOW"
    );
    if (config_fd >= 0) {
        char byte;

        check(
            fstat(config_fd, &metadata) == 0 && S_ISREG(metadata.st_mode),
            "fstat direct-opened final configuration reports regular file"
        );
        check(
            read(config_fd, &byte, sizeof(byte)) == (ssize_t)sizeof(byte) && byte == 'M',
            "read direct-opened final configuration"
        );
    }
out:
    if (config_fd >= 0) {
        close(config_fd);
    }
    if (config_path_fd >= 0) {
        close(config_path_fd);
    }
    if (second_link_fd >= 0) {
        close(second_link_fd);
    }
    if (config_parent_fd >= 0) {
        close(config_parent_fd);
    }
    if (first_link_fd >= 0) {
        close(first_link_fd);
    }
    if (second_root_fd >= 0) {
        close(second_root_fd);
    }
    if (first_root_fd >= 0) {
        close(first_root_fd);
    }
    if (root_fd >= 0) {
        close(root_fd);
    }

    if (failures != 0) {
        printf("STARRY_GROUPED_TEST_FAILED: bugfix-systemd-chase-etc-static\n");
        return EXIT_FAILURE;
    }

    printf("STARRY_GROUPED_TEST_PASSED: bugfix-systemd-chase-etc-static\n");
    return EXIT_SUCCESS;
}
