#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

struct paths {
    char base[128];
    char container[160];
    char jail[192];
    char mountpoint[224];
    char parent_secret[192];
    char grandparent_secret[192];
    char jail_secret[224];
    char escape_link[224];
    int mounted;
};

static int make_directory(const char *path)
{
    if (mkdir(path, 0700) == 0 || errno == EEXIST) {
        return 0;
    }
    perror(path);
    return -1;
}

static int join_path(char *path, size_t size, const char *dir, const char *name)
{
    int len = snprintf(path, size, "%s/%s", dir, name);
    return len < 0 || (size_t)len >= size ? -1 : 0;
}

static int create_secret(const char *path)
{
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
    if (fd < 0) {
        perror(path);
        return -1;
    }
    if (write(fd, "secret", 6) != 6) {
        perror("write secret");
        close(fd);
        return -1;
    }
    if (close(fd) < 0) {
        perror("close secret");
        return -1;
    }
    return 0;
}

static void cleanup(struct paths *paths)
{
    if (paths->mounted) {
        umount2(paths->mountpoint, 0);
        paths->mounted = 0;
    }
    unlink(paths->escape_link);
    unlink(paths->jail_secret);
    unlink(paths->parent_secret);
    unlink(paths->grandparent_secret);
    rmdir(paths->mountpoint);
    rmdir(paths->jail);
    rmdir(paths->container);
    rmdir(paths->base);
}

static int expect_hidden(const char *path)
{
    int fd = open(path, O_RDONLY);
    if (fd >= 0) {
        close(fd);
        fprintf(stderr, "FAIL: chroot escaped through %s\n", path);
        return -1;
    }
    if (errno != ENOENT) {
        fprintf(stderr, "FAIL: open(%s) errno=%d, expected ENOENT\n", path, errno);
        return -1;
    }
    return 0;
}

static int chroot_to(const char *path)
{
    if (chdir(path) < 0) {
        perror("chdir chroot");
        return 1;
    }
    if (chroot(".") < 0) {
        perror("chroot");
        return 1;
    }
    return 0;
}

static int child_from_jail_mount(const struct paths *paths)
{
    if (chroot_to(paths->jail) != 0) {
        return 1;
    }
    if (chdir("mountpoint") < 0) {
        perror("chdir mountpoint");
        return 1;
    }
    if (expect_hidden("../../parent-secret") < 0 ||
        expect_hidden("../../grandparent-secret") < 0 ||
        expect_hidden("../../../grandparent-secret") < 0) {
        return 1;
    }
    return 0;
}

static int child_from_mount_root(const struct paths *paths)
{
    if (chroot_to(paths->mountpoint) != 0) {
        return 1;
    }
    if (expect_hidden("../jail-secret") < 0 ||
        expect_hidden("../../parent-secret") < 0) {
        return 1;
    }
    return 0;
}

static int child_from_jail_root(const struct paths *paths)
{
    if (chroot_to(paths->jail) != 0) {
        return 1;
    }
    if (expect_hidden("../parent-secret") < 0 ||
        expect_hidden("../../grandparent-secret") < 0 ||
        expect_hidden("escape-link") < 0) {
        return 1;
    }
    if (unlink("escape-link") < 0) {
        perror("unlink escape-link");
        return 1;
    }
    errno = 0;
    if (rmdir("..") == 0 || errno != EBUSY) {
        fprintf(stderr, "FAIL: rmdir(..) escaped the jail errno=%d\n", errno);
        return 1;
    }
    return 0;
}

static int run_child(const struct paths *paths, int (*body)(const struct paths *), const char *name)
{
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return -1;
    }
    if (child == 0) {
        _exit(body(paths));
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: %s chroot child status=%d errno=%d\n", name, status, errno);
        return -1;
    }
    return 0;
}

int main(void)
{
    struct paths paths = {0};
    int len = snprintf(paths.base, sizeof(paths.base),
                       "/tmp/bug-chroot-parent-escape-%ld", (long)getpid());
    if (len < 0 || (size_t)len >= sizeof(paths.base)) {
        return 1;
    }
    if (join_path(paths.container, sizeof(paths.container), paths.base, "container") < 0 ||
        join_path(paths.jail, sizeof(paths.jail), paths.container, "jail") < 0 ||
        join_path(paths.mountpoint, sizeof(paths.mountpoint), paths.jail, "mountpoint") < 0 ||
        join_path(paths.parent_secret, sizeof(paths.parent_secret), paths.container, "parent-secret") < 0 ||
        join_path(paths.grandparent_secret, sizeof(paths.grandparent_secret), paths.base, "grandparent-secret") < 0 ||
        join_path(paths.jail_secret, sizeof(paths.jail_secret), paths.jail, "jail-secret") < 0 ||
        join_path(paths.escape_link, sizeof(paths.escape_link), paths.jail, "escape-link") < 0) {
        return 1;
    }

    cleanup(&paths);
    if (make_directory("/tmp") < 0 || make_directory(paths.base) < 0 ||
        make_directory(paths.container) < 0 || make_directory(paths.jail) < 0 ||
        make_directory(paths.mountpoint) < 0 ||
        create_secret(paths.parent_secret) < 0 || create_secret(paths.grandparent_secret) < 0 ||
        create_secret(paths.jail_secret) < 0 ||
        symlink("../../grandparent-secret", paths.escape_link) < 0) {
        cleanup(&paths);
        return 1;
    }

    if (mount("tmpfs", paths.mountpoint, "tmpfs", 0, NULL) < 0) {
        perror("mount tmpfs");
        cleanup(&paths);
        return 1;
    }
    paths.mounted = 1;

    int result = 0;
    if (run_child(&paths, child_from_jail_root, "jail root") < 0 ||
        run_child(&paths, child_from_jail_mount, "jail mount") < 0 ||
        run_child(&paths, child_from_mount_root, "mount root") < 0) {
        result = 1;
    }
    cleanup(&paths);
    if (result == 0) {
        puts("PASS: chroot parent traversal remains inside the jail");
    }
    return result;
}
