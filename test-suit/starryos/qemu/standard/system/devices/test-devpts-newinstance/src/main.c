#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <termios.h>
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

static int mount_default_devpts(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        fail("create default devpts mountpoint");
        return -1;
    }
    if (mount("none", path, "devpts", 0,
              "mode=0620,gid=5,ptmxmode=0666") != 0) {
        fail("mount default devpts instance");
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

static int open_pty(const char *path, unsigned int *number)
{
    int master = open(path, O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("open ptmx");
        return -1;
    }
    if (ioctl(master, TIOCGPTN, number) != 0) {
        fail("read allocated PTY number");
        close(master);
        return -1;
    }
    int locked = 0;
    if (ioctl(master, TIOCSPTLCK, &locked) != 0) {
        fail("unlock the allocated slave");
        close(master);
        return -1;
    }
    return master;
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

    return open_pty(path, number);
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

static void check_default_mounts_are_private(void)
{
    static const char first_mount[] = "/tmp/devpts-default-a";
    static const char second_mount[] = "/tmp/devpts-default-b";
    if (mount_default_devpts(first_mount) != 0 ||
        mount_default_devpts(second_mount) != 0) {
        return;
    }

    unsigned int first_number = ~0U;
    int first_master = allocate_pty(first_mount, &first_number);
    if (first_master < 0) return;
    errno = 0;
    if (first_number != 0 || slave_exists(second_mount, first_number) != -1 ||
        errno != ENOENT) {
        fail("default mounts keep the first allocation private");
    } else {
        pass("default mounts keep the first allocation private");
    }

    unsigned int second_number = ~0U;
    int second_master = allocate_pty(second_mount, &second_number);
    if (second_master >= 0) {
        if (second_number != 0) {
            fail("default mounts have independent PTY index spaces");
        } else {
            pass("default mounts have independent PTY index spaces");
        }
        check_slave_visible(first_mount, first_number,
                            "first default mount exposes its own slave");
        check_slave_visible(second_mount, second_number,
                            "second default mount exposes its own slave");
        struct stat first, second, initial;
        if (stat(first_mount, &first) != 0 || stat(second_mount, &second) != 0 ||
            stat("/dev/pts", &initial) != 0 || first.st_dev == second.st_dev ||
            first.st_dev == initial.st_dev || second.st_dev == initial.st_dev) {
            fail("default mounts and the initial instance have distinct filesystem identities");
        } else {
            pass("default mounts and the initial instance have distinct filesystem identities");
        }
        close(second_master);
    }
    close(first_master);
    umount2(second_mount, MNT_DETACH);
    umount2(first_mount, MNT_DETACH);
    rmdir(second_mount);
    rmdir(first_mount);
}

static void check_private_controlling_tty(const char *mountpoint,
                                          unsigned int number, int master)
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

        pid_t foreground;
        if (ioctl(current_tty, TIOCGPGRP, &foreground) != 0 || foreground != getpgrp()) {
            _exit(13);
        }
        if (write(current_tty, "T", 1) != 1) {
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
    struct pollfd output = { .fd = master, .events = POLLIN };
    char byte = 0;
    if (poll(&output, 1, 5000) != 1 || read(master, &byte, 1) != 1 || byte != 'T') {
        fail("/dev/tty writes to its private devpts master");
        return;
    }
    pass("/dev/tty reopens the private devpts controlling terminal");
}

static void check_dev_ptmx_uses_mounted_instance(void)
{
    if (unshare(CLONE_NEWNS) != 0 ||
        mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL) != 0 ||
        mount("none", "/dev/pts", "devpts", 0, "mode=0620,gid=5") != 0) {
        fail("mount devpts over /dev/pts in a private namespace");
        return;
    }

    unsigned int number;
    int master = open_pty("/dev/ptmx", &number);
    if (master >= 0) {
        char path[64];
        snprintf(path, sizeof(path), "/dev/pts/%u", number);
        int slave = open(path, O_RDWR | O_NOCTTY);
        if (slave < 0) {
            fail("/dev/ptmx creates its slave in the mounted /dev/pts");
        } else {
            struct termios mode;
            struct pollfd input = { .fd = slave, .events = POLLIN };
            char byte = 0;
            if (tcgetattr(slave, &mode) == 0) {
                cfmakeraw(&mode);
                if (tcsetattr(slave, TCSANOW, &mode) == 0 &&
                    write(master, "M", 1) == 1 &&
                    poll(&input, 1, 5000) == 1 &&
                    read(slave, &byte, 1) == 1 && byte == 'M') {
                    pass("/dev/ptmx sends data to the mounted slave");
                } else {
                    fail("/dev/ptmx sends data to the mounted slave");
                }
            } else {
                fail("read the mounted slave's terminal mode");
            }
            close(slave);
            check_private_controlling_tty("/dev/pts", number, master);
        }
        close(master);
    }
    if (umount2("/dev/pts", MNT_DETACH) != 0) {
        fail("detach the test devpts mount");
    }
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

    if (chmod("/tmp/devpts-newinstance-a/ptmx", 0620) != 0) {
        fail("change the instance's persistent ptmx permissions");
    } else {
        check_metadata("/tmp/devpts-newinstance-a/ptmx", 0620, 0,
                       "repeated lookups retain ptmx permission changes");
    }

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
    check_private_controlling_tty(first_mount, first_number,
                                   first_masters[first_master_count - 1]);

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

    check_default_mounts_are_private();
    check_dev_ptmx_uses_mounted_instance();

    if (failures != 0) {
        printf("TEST_DEVPTS_NEWINSTANCE_FAILED failures=%d\n", failures);
        return 1;
    }
    puts("TEST_DEVPTS_NEWINSTANCE_PASSED");
    return 0;
}
