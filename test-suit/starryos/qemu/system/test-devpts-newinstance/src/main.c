#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int failures;

static void pass(const char *message)
{
    printf("  PASS: %s\n", message);
}

static void fail(const char *message)
{
    printf("  FAIL: %s (errno=%d: %s)\n", message, errno, strerror(errno));
    failures++;
}

static int mount_devpts(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        fail("create devpts mountpoint");
        return -1;
    }
    if (mount("none", path, "devpts", 0,
              "newinstance,mode=0620,gid=5,ptmxmode=0666") != 0) {
        fail("mount private devpts instance");
        return -1;
    }
    return 0;
}

static int mount_legacy_devpts(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        fail("create legacy devpts mountpoint");
        return -1;
    }
    if (mount("none", path, "devpts", 0,
              "mode=0620,gid=5,ptmxmode=0666") != 0) {
        fail("mount legacy devpts instance");
        return -1;
    }
    return 0;
}

static void check_metadata(const char *path, mode_t mode, gid_t gid,
                           const char *message)
{
    struct stat metadata;
    if (stat(path, &metadata) != 0) {
        fail(message);
        return;
    }
    if ((metadata.st_mode & 07777) != mode || metadata.st_gid != gid) {
        errno = EPROTO;
        fail(message);
        return;
    }
    pass(message);
}

static int allocate_pty(const char *mountpoint, unsigned int *number)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "%s/ptmx", mountpoint);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("format ptmx path");
        return -1;
    }

    int master = open(path, O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("open per-instance ptmx");
        return -1;
    }
    if (ioctl(master, TIOCGPTN, number) != 0) {
        fail("read allocated PTY number");
        close(master);
        return -1;
    }
    return master;
}

static void close_masters(int *masters, size_t count)
{
    for (size_t index = 0; index < count; index++) {
        close(masters[index]);
    }
    free(masters);
}

static int slave_exists(const char *mountpoint, unsigned int number)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "%s/%u", mountpoint, number);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }

    struct stat metadata;
    return stat(path, &metadata);
}

static void check_slave_visible(const char *mountpoint, unsigned int number,
                                const char *message)
{
    if (slave_exists(mountpoint, number) != 0) {
        fail(message);
    } else {
        pass(message);
    }
}

static void check_legacy_mounts_share_initial_instance(void)
{
    static const char first_mount[] = "/tmp/devpts-legacy-a";
    static const char second_mount[] = "/tmp/devpts-legacy-b";

    if (mount_legacy_devpts(first_mount) != 0 ||
        mount_legacy_devpts(second_mount) != 0) {
        return;
    }

    unsigned int first_number = ~0U;
    int first_master = allocate_pty(first_mount, &first_number);
    if (first_master < 0) {
        return;
    }
    check_slave_visible(second_mount, first_number,
                        "legacy mounts share slaves with each other");
    check_slave_visible("/dev/pts", first_number,
                        "legacy mounts share slaves with the initial instance");

    unsigned int second_number = ~0U;
    int second_master = allocate_pty(second_mount, &second_number);
    if (second_master < 0) {
        close(first_master);
        return;
    }
    if (second_number == first_number) {
        errno = EPROTO;
        fail("legacy mounts share one PTY index allocator");
    } else {
        pass("legacy mounts share one PTY index allocator");
    }
    check_slave_visible(first_mount, second_number,
                        "second legacy allocation is visible in the first mount");

    close(second_master);
    close(first_master);
    umount2(second_mount, MNT_DETACH);
    umount2(first_mount, MNT_DETACH);
    rmdir(second_mount);
    rmdir(first_mount);
}

static void check_private_controlling_tty(const char *mountpoint,
                                          unsigned int number)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "%s/%u", mountpoint, number);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("format private slave path");
        return;
    }

    int slave = open(path, O_RDWR | O_NOCTTY);
    if (slave < 0) {
        fail("open private slave for controlling terminal");
        return;
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork controlling terminal child");
        close(slave);
        return;
    }
    if (child == 0) {
        if (setsid() < 0) {
            _exit(10);
        }
        if (ioctl(slave, TIOCSCTTY, 0) != 0) {
            _exit(11);
        }

        int current_tty = open("/dev/tty", O_RDWR | O_NOCTTY);
        if (current_tty < 0) {
            _exit(12);
        }

        struct stat metadata;
        if (fstat(current_tty, &metadata) != 0) {
            _exit(13);
        }
        if ((metadata.st_mode & 07777) != 0620 || metadata.st_gid != 5) {
            _exit(14);
        }
        close(current_tty);
        close(slave);
        _exit(0);
    }

    close(slave);
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = EPROTO;
        fail("/dev/tty reopens the private devpts controlling terminal");
        return;
    }
    pass("/dev/tty reopens the private devpts controlling terminal");
}

int main(void)
{
    static const char first_mount[] = "/tmp/devpts-newinstance-a";
    static const char second_mount[] = "/tmp/devpts-newinstance-b";

    if (mount_devpts(first_mount) != 0 || mount_devpts(second_mount) != 0) {
        return 1;
    }

    check_metadata("/tmp/devpts-newinstance-a/ptmx", 0666, 0,
                   "ptmxmode applies to the per-instance ptmx node");

    unsigned int root_number = ~0U;
    int root_master = allocate_pty("/dev/pts", &root_number);
    if (root_master < 0) {
        return 1;
    }

    size_t first_master_count = (size_t)root_number + 1;
    int *first_masters = calloc(first_master_count, sizeof(*first_masters));
    if (first_masters == NULL) {
        fail("allocate private PTY master table");
        close(root_master);
        return 1;
    }

    unsigned int first_allocated_number = ~0U;
    unsigned int first_number = ~0U;
    for (size_t index = 0; index < first_master_count; index++) {
        first_masters[index] = allocate_pty(first_mount, &first_number);
        if (first_masters[index] < 0) {
            close_masters(first_masters, index);
            close(root_master);
            return 1;
        }
        if (index == 0) {
            first_allocated_number = first_number;
        }
    }
    if (first_allocated_number != 0) {
        errno = EPROTO;
        fail("first devpts instance starts PTY numbering at zero");
    } else {
        pass("first devpts instance starts PTY numbering at zero");
    }
    if (first_number != root_number) {
        errno = EPROTO;
        fail("private devpts reaches the root instance PTY number");
    } else {
        pass("private devpts reaches the root instance PTY number");
    }
    if (slave_exists(first_mount, first_number) != 0) {
        fail("allocated slave is visible in its owning devpts instance");
    } else {
        pass("allocated slave is visible in its owning devpts instance");
    }
    char first_slave_path[128];
    int first_slave_path_length =
        snprintf(first_slave_path, sizeof(first_slave_path), "%s/%u",
                 first_mount, first_number);
    if (first_slave_path_length < 0 ||
        (size_t)first_slave_path_length >= sizeof(first_slave_path)) {
        errno = ENAMETOOLONG;
        fail("format first private slave path");
    } else {
        check_metadata(first_slave_path, 0620, 5,
                       "mode and gid apply to the allocated slave node");
    }
    check_private_controlling_tty(first_mount, first_number);

    errno = 0;
    if (slave_exists(second_mount, first_number) == 0 || errno != ENOENT) {
        errno = EPROTO;
        fail("slave from first instance is hidden from second instance");
    } else {
        pass("slave from first instance is hidden from second instance");
    }

    unsigned int second_number = ~0U;
    int second_master = allocate_pty(second_mount, &second_number);
    if (second_master < 0) {
        close_masters(first_masters, first_master_count);
        close(root_master);
        return 1;
    }
    if (second_number != 0) {
        errno = EPROTO;
        fail("second devpts instance has an independent PTY index space");
    } else {
        pass("second devpts instance has an independent PTY index space");
    }
    if (slave_exists(second_mount, second_number) != 0) {
        fail("second instance exposes its own allocated slave");
    } else {
        pass("second instance exposes its own allocated slave");
    }

    close(second_master);
    close_masters(first_masters, first_master_count);
    close(root_master);
    umount2(second_mount, MNT_DETACH);
    umount2(first_mount, MNT_DETACH);
    rmdir(second_mount);
    rmdir(first_mount);

    check_legacy_mounts_share_initial_instance();

    if (failures != 0) {
        printf("TEST_DEVPTS_NEWINSTANCE_FAILED failures=%d\n", failures);
        return 1;
    }
    puts("TEST_DEVPTS_NEWINSTANCE_PASSED");
    return 0;
}
